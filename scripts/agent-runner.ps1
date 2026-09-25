<#
.SYNOPSIS
  Runner for containeriserte kodeagenter: poller agentens triggers/inbox.jsonl
  og kjører én engangs-container per ubehandlet event.

.DESCRIPTION
  Dum supervisor uten LLM, felles for alle agenter som følger
  engangs-container-mønsteret (jr, sr). Finner ubehandlede events (id uten
  linje i triggers/results.jsonl — samme dedupe-regel som resten av
  pipelinen), grupperer dem på topic (payload.origin.event_id uten
  delta-suffiks), og kjører `docker run --rm` per event. Seriellt innen et
  topic (samme workspace og Claude-sesjon), parallelt på tvers av topics
  (maks -MaxParallel samtidige).

  Hvert topic får sitt eget workspace i agents/<navn>/workspaces/<topic>/
  (gitignorert), mountet som /workspace. Containeren har aldri
  Docker-socket, og hverken monorepoet, deploy-klonen eller
  workspaces_repos/ mountes inn.

  Forutsetning: agents/<navn>/.env finnes (kopiér fra .env.example og fyll
  inn agentens eget scopede GH_TOKEN).

.PARAMETER AgentName
  Agentkatalogen under agents/ (f.eks. local-cc-jr-developer eller
  local-cc-coding-agent). Imaget heter <AgentName>:latest med mindre
  -Image overstyrer.

.EXAMPLE
  .\scripts\agent-runner.ps1 -AgentName local-cc-jr-developer
  .\scripts\agent-runner.ps1 -AgentName local-cc-coding-agent -MaxParallel 1
#>
param(
  [Parameter(Mandatory = $true)]
  [ValidatePattern('^[a-zA-Z0-9._-]+$')]
  [string]$AgentName,
  [int]$MaxParallel = 2,
  [int]$PollSeconds = 10,
  [string]$Image = ""
)

$ErrorActionPreference = "Stop"
if ($Image -eq "") { $Image = "${AgentName}:latest" }

$repoRoot = Split-Path $PSScriptRoot -Parent
$agentDir = Join-Path $repoRoot "agents\$AgentName"
$triggersDir = Join-Path $agentDir "triggers"
$inboxFile = Join-Path $triggersDir "inbox.jsonl"
$resultsFile = Join-Path $triggersDir "results.jsonl"
$queueDir = Join-Path $triggersDir ".queue"
$workspacesDir = Join-Path $agentDir "workspaces"
$envFile = Join-Path $agentDir ".env"
# Kunnskapsklonen er en anker-folder på MONOREPO-rot (én katalog over dette
# repoet i deploy-oppsettet); finnes den ikke der, prøves repo-rot.
$knowledgeDir = Join-Path (Split-Path $repoRoot -Parent) "workspaces_knowledge"
if (-not (Test-Path $knowledgeDir)) {
  $knowledgeDir = Join-Path $repoRoot "workspaces_knowledge"
}

function Write-Log([string]$msg) {
  Write-Host ("[{0:yyyy-MM-ddTHH:mm:ssZ}] [{1}] {2}" -f (Get-Date).ToUniversalTime(), $AgentName, $msg)
}

if (-not (Test-Path $agentDir)) {
  Write-Error "Ukjent agent: $agentDir finnes ikke."
}
if (-not (Test-Path $envFile)) {
  Write-Error "Mangler $envFile — kopiér .env.example til .env og fyll inn agentens token."
}

docker image inspect $Image *> $null
if ($LASTEXITCODE -ne 0) {
  Write-Log "Imaget $Image finnes ikke — bygger fra $agentDir ..."
  docker build -t $Image -f (Join-Path $agentDir "docker\Dockerfile") $agentDir
  if ($LASTEXITCODE -ne 0) { Write-Error "docker build feilet" }
}

New-Item -ItemType Directory -Force $queueDir, $workspacesDir, (Join-Path $triggersDir "logs") | Out-Null
if (-not (Test-Path $inboxFile)) { New-Item -ItemType File $inboxFile | Out-Null }
if (-not (Test-Path $resultsFile)) { New-Item -ItemType File $resultsFile | Out-Null }

function Get-TopicKey([object]$evt) {
  $key = $evt.payload.origin.event_id
  if (-not $key) { $key = $evt.id }
  # Delta-suffikset (-d1, -d2, ...) skiller oppfølgingsevents i samme tråd;
  # topicet er tråden selv.
  $key = $key -replace '-d\d+$', ''
  return ($key -replace '[^a-zA-Z0-9._-]', '_')
}

# topic → prosessobjekt for kjørende container
$running = @{}

Write-Log "Runner startet: poller $inboxFile hvert ${PollSeconds}s (maks $MaxParallel parallelle topics, image: $Image)"

while ($true) {
  # Rydd ferdige kjøringer
  foreach ($topic in @($running.Keys)) {
    if ($running[$topic].HasExited) {
      Write-Log "Topic ${topic}: container ferdig (exit $($running[$topic].ExitCode))"
      $running.Remove($topic)
    }
  }

  $doneIds = @{}
  foreach ($line in (Get-Content $resultsFile -ErrorAction SilentlyContinue)) {
    if (-not $line.Trim()) { continue }
    try { $doneIds[([string](ConvertFrom-Json $line).id)] = $true } catch {}
  }

  foreach ($line in (Get-Content $inboxFile -ErrorAction SilentlyContinue)) {
    if (-not $line.Trim()) { continue }
    try { $evt = ConvertFrom-Json $line } catch { continue }
    if (-not $evt.id -or $doneIds.ContainsKey([string]$evt.id)) { continue }

    $topic = Get-TopicKey $evt
    # Seriellt innen topic; parallelt på tvers, opptil taket
    if ($running.ContainsKey($topic)) { continue }
    if ($running.Count -ge $MaxParallel) { break }

    $safeId = [string]$evt.id -replace '[^a-zA-Z0-9._-]', '_'
    $eventFile = Join-Path $queueDir "$safeId.json"
    Set-Content -Path $eventFile -Value $line -Encoding utf8 -NoNewline
    $topicWs = Join-Path $workspacesDir $topic
    New-Item -ItemType Directory -Force $topicWs | Out-Null

    $dockerArgs = @(
      "run", "--rm",
      "--name", "agent-$AgentName-$safeId",
      "--add-host", "host.docker.internal:host-gateway",
      "--security-opt", "no-new-privileges:true",
      "--cap-drop", "ALL",
      "--env-file", $envFile,
      "-e", "EVENT_FILE=/triggers/.queue/$safeId.json",
      "-v", "${triggersDir}:/triggers",
      "-v", "${topicWs}:/workspace"
    )
    if (Test-Path $knowledgeDir) {
      $dockerArgs += @("-v", "${knowledgeDir}:/knowledge")
    }
    $dockerArgs += $Image

    Write-Log "Topic ${topic}: starter container for event $($evt.id)"
    $runnerLog = Join-Path $triggersDir "logs\$safeId.runner.log"
    $proc = Start-Process -FilePath "docker" -ArgumentList $dockerArgs `
      -NoNewWindow -PassThru `
      -RedirectStandardOutput $runnerLog `
      -RedirectStandardError "$runnerLog.err"
    $running[$topic] = $proc
  }

  Start-Sleep -Seconds $PollSeconds
}
