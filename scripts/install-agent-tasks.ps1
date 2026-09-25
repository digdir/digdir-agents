<#
.SYNOPSIS
  Registrerer (eller fjerner) Scheduled Tasks som kjører pipelinens
  host-prosesser headless — ingen lokale konsoller å passe på.

.DESCRIPTION
  Tre oppgaver, alle med start ved innlogging og automatisk restart ved feil:

    digdir-self-update       scripts/self-update.ps1 -WatchSeconds 300
                             (auto-deploy: pull + build + helsesjekk av
                             docker-klyngen ved merge til deploy-branchen)
    digdir-runner-jr         scripts/agent-runner.ps1 -AgentName local-cc-jr-developer
    digdir-runner-sr         scripts/agent-runner.ps1 -AgentName local-cc-coding-agent

  Kjøres fra deploy-klonen (repoRoot utledes fra scriptets plassering).
  Oppgavene kjører som innlogget bruker uten vindu. Merk at self-update i
  headless modus ikke har hurtigtastene R/Q — .env-endringer aktiveres med
  `docker compose up -d` fra repo-rota (eller restart av tasken).

  Logger: hver task skriver til logs/<tasknavn>.log under repo-rota
  (gitignorert i deploy-sammenheng; katalogen opprettes ved behov).

.PARAMETER Uninstall
  Fjern de tre oppgavene i stedet for å registrere dem.

.EXAMPLE
  pwsh scripts/install-agent-tasks.ps1              # registrer + start
  pwsh scripts/install-agent-tasks.ps1 -Uninstall   # fjern
#>
[CmdletBinding()]
param(
  [switch]$Uninstall
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path $PSScriptRoot -Parent
$pwshExe = (Get-Process -Id $PID).Path
$logDir = Join-Path $repoRoot "logs"

$tasks = @(
  @{ Name = "digdir-self-update"; Script = "self-update.ps1";  Args = "-WatchSeconds 300" },
  @{ Name = "digdir-runner-jr";   Script = "agent-runner.ps1"; Args = "-AgentName local-cc-jr-developer" },
  @{ Name = "digdir-runner-sr";   Script = "agent-runner.ps1"; Args = "-AgentName local-cc-coding-agent" }
)

if ($Uninstall) {
  foreach ($t in $tasks) {
    try {
      Unregister-ScheduledTask -TaskName $t.Name -Confirm:$false -ErrorAction Stop
      Write-Host "Fjernet: $($t.Name)"
    } catch {
      Write-Host "Ikke registrert (hopper over): $($t.Name)"
    }
  }
  return
}

New-Item -ItemType Directory -Force $logDir | Out-Null

foreach ($t in $tasks) {
  $scriptPath = Join-Path $repoRoot "scripts\$($t.Script)"
  if (-not (Test-Path $scriptPath)) { throw "Finner ikke $scriptPath" }
  $logFile = Join-Path $logDir "$($t.Name).log"

  # Transkripsjon til loggfil: Scheduled Tasks har ingen konsoll, så uten
  # dette forsvinner all output. Loggen trunkeres ved hver start.
  $command = "& { try { `$PSStyle.OutputRendering = 'PlainText' } catch {}; " +
             "& '$scriptPath' $($t.Args) *>&1 | Out-File -FilePath '$logFile' -Encoding utf8 }"
  $action = New-ScheduledTaskAction -Execute $pwshExe `
    -Argument "-NoProfile -NonInteractive -WindowStyle Hidden -Command `"$command`"" `
    -WorkingDirectory $repoRoot
  $trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
  $settings = New-ScheduledTaskSettingsSet `
    -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 2) `
    -ExecutionTimeLimit (New-TimeSpan -Days 0) `
    -MultipleInstances IgnoreNew `
    -StartWhenAvailable

  # Erstatt eksisterende registrering i stedet for å feile
  try { Unregister-ScheduledTask -TaskName $t.Name -Confirm:$false -ErrorAction Stop } catch {}
  Register-ScheduledTask -TaskName $t.Name -Action $action -Trigger $trigger `
    -Settings $settings -Description "digdir-ai-agents: $($t.Script) $($t.Args)" | Out-Null
  Start-ScheduledTask -TaskName $t.Name
  Write-Host "Registrert og startet: $($t.Name)  (logg: $logFile)"
}

Write-Host ""
Write-Host "Status: Get-ScheduledTask digdir-* | Get-ScheduledTaskInfo"
Write-Host "Stopp:  Stop-ScheduledTask <navn>   Fjern alt: -Uninstall"
