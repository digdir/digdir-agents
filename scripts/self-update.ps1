<#
.SYNOPSIS
  Selvoppgradering av agent-runtimen «i fart»: hent siste kode på
  deploy-branchen, bygg nye images FØR den kjørende klyngen røres, bytt over,
  verifiser helse — og rull tilbake til forrige versjon ved feil.

.DESCRIPTION
  Kjører på HOSTEN, utenfor det som oppgraderes: en container kan ikke trygt
  stoppe og gjenoppbygge seg selv, og agentene skal ikke ha Docker-tilgang.
  Utløseren er merge til deploy-branchen — når agent-pipelinen selv har fått
  en PR merget, plukker dette skriptet det opp (menneskelig review = gaten).

  A/B-prinsipp uten parallellkjøring (innboksene er single-consumer, to
  samtidige klynger dobbeltbehandler events):
    1. Kjørende images beholdes som :rollback-tag.
    2. Nye images bygges mens den gamle klyngen fortsatt kjører — byggefeil
       er den vanligste «brick»-årsaken og gir her null nedetid.
    3. Bytte (`docker compose up -d`) og helsesjekk: containerne kjører
       stabilt uten restarts, og klar-meldinger dukker opp i loggene.
    4. Ved feil rulles kode og images tilbake og forrige versjon startes.

  Kø/state ligger i filer og navngitte volumer, så events i innboksene
  overlever byttet.

  Skriptet er også drifts-inngangen: er koden allerede oppdatert men klyngen
  nede (første oppstart, etter reboot), startes den. Én kommando å kjøre.

.PARAMETER Branch
  Deploy-branchen som følges (default: main). Arbeidskopien må stå på denne.

.PARAMETER WatchSeconds
  > 0: kjør i løkke og sjekk origin hvert n-te sekund (Ctrl+C stopper).
  I en interaktiv konsoll lytter løkka på hurtigtaster: R gjenskaper
  containere med oppdatert .env-config (docker compose up -d + helsesjekk),
  Q avslutter ryddig.

.PARAMETER HealthTimeoutSeconds
  Hvor lenge helsesjekken venter på klar-meldinger før den konkluderer.

.PARAMETER Force
  Fortsett selv om arbeidskopien har ukommitterte endringer (da gjøres ingen
  automatisk `git reset` ved tilbakerulling).

.EXAMPLE
  pwsh scripts/self-update.ps1                     # én sjekk/oppgradering nå
  pwsh scripts/self-update.ps1 -WatchSeconds 300   # poll hvert 5. minutt
#>
[CmdletBinding()]
param(
  [string]$Branch = "main",
  [int]$WatchSeconds = 0,
  [int]$HealthTimeoutSeconds = 120,
  [switch]$Force
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path $PSScriptRoot -Parent

function Write-Step([string]$Message) {
  Write-Host "[self-update $(Get-Date -Format HH:mm:ss)] $Message"
}

# Klar-meldinger fra komponentene (best effort — brukes sammen med
# stabilitetssjekken, ikke alene).
$readyPattern = "Authenticated as|Connected via Socket Mode|integrations started|Watch-modus: poller"

function Test-ClusterHealthy {
  param([string]$SinceIso, [int]$TimeoutSeconds)

  $services = @(docker compose config --services | Where-Object { $_ })
  $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
  $stableSamples = 0
  $readySeen = $false

  while ((Get-Date) -lt $deadline) {
    Start-Sleep -Seconds 5

    $allRunning = $true
    foreach ($svc in $services) {
      $cid = @(docker compose ps -q $svc | Where-Object { $_ }) | Select-Object -First 1
      if (-not $cid) { $allRunning = $false; continue }
      $state = (docker inspect --format "{{.State.Status}} {{.RestartCount}}" $cid).Trim()
      $status, $restarts = $state -split " "
      if ([int]$restarts -gt 0 -or $status -in @("exited", "dead")) {
        Write-Warning "Tjenesten '$svc' er '$status' (restarts: $restarts)."
        return $false
      }
      if ($status -ne "running") { $allRunning = $false }
    }
    $stableSamples = $allRunning ? $stableSamples + 1 : 0

    if (-not $readySeen) {
      $logs = docker compose logs --no-color --since $SinceIso 2>$null
      if ($logs -match $readyPattern) { $readySeen = $true }
    }
    if ($readySeen -and $stableSamples -ge 3) { return $true }
  }

  if ($stableSamples -ge 3) {
    Write-Warning "Fant ingen klar-melding i loggene innen fristen, men alle containerne kjører stabilt — regner klyngen som frisk."
    return $true
  }
  return $false
}

function Start-ClusterIfDown {
  $services = @(docker compose config --services | Where-Object { $_ })
  $running = @(docker compose ps --status running --services | Where-Object { $_ })
  $down = @($services | Where-Object { $running -notcontains $_ })
  if ($down.Count -eq 0) { return }

  Write-Step "Klyngen kjører ikke (mangler: $($down -join ', ')) — starter..."
  docker compose up -d
  if ($LASTEXITCODE -ne 0) { throw "Klarte ikke starte klyngen — se docker compose logs." }
}

# Gjenskaper containere med oppdatert config: env_file leses kun når en
# container OPPRETTES, så en redigert .env krever recreate — restart holder
# ikke. `docker compose up -d` gjenskaper bare tjenester med endret config og
# rører hverken images, git-tilstand eller :rollback-taggene.
function Invoke-EnvReload {
  Push-Location $repoRoot
  try {
    Write-Step "Laster .env-config på nytt (docker compose up -d)..."
    $before = @(docker compose ps -q | Where-Object { $_ } | Sort-Object) -join ","
    $since = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    docker compose up -d
    if ($LASTEXITCODE -ne 0) {
      Write-Warning "docker compose up feilet — se docker compose logs."
      return
    }
    $after = @(docker compose ps -q | Where-Object { $_ } | Sort-Object) -join ","
    if ($after -eq $before) {
      Write-Step "Ingen config-endringer — containerne er uendret."
      return
    }
    if (Test-ClusterHealthy -SinceIso $since -TimeoutSeconds $HealthTimeoutSeconds) {
      Write-Step "Config lastet på nytt og klyngen verifisert."
    } else {
      Write-Warning "Helsesjekk feilet etter config-reload — sjekk docker compose ps / logs."
    }
  } finally {
    Pop-Location
  }
}

# Venter i inntil $Seconds, men reagerer underveis på hurtigtaster når en
# interaktiv konsoll finnes. Uten interaktiv konsoll (redirigert stdin,
# tjeneste) kaster KeyAvailable — da degraderer ventingen til ren sleep,
# som før. Returnerer "reload", "quit" eller "timeout".
function Wait-WithHotkeys([int]$Seconds) {
  $deadline = (Get-Date).AddSeconds($Seconds)
  while ((Get-Date) -lt $deadline) {
    try {
      while ([Console]::KeyAvailable) {
        $key = [Console]::ReadKey($true)
        if ($key.Key -eq [ConsoleKey]::R) { return "reload" }
        if ($key.Key -eq [ConsoleKey]::Q) { return "quit" }
      }
    } catch {
      $rest = [int][Math]::Ceiling(($deadline - (Get-Date)).TotalSeconds)
      if ($rest -gt 0) { Start-Sleep -Seconds $rest }
      return "timeout"
    }
    Start-Sleep -Seconds 1
  }
  return "timeout"
}

function Restore-Previous {
  param([string]$OldSha, [bool]$WasClean, [string[]]$Images)

  Write-Step "Ruller tilbake til $($OldSha.Substring(0, 7))..."
  if ($WasClean) { git reset --hard $OldSha | Out-Null }
  foreach ($img in $Images) {
    $rollbackTag = "$($img.Split(':')[0]):rollback"
    docker image inspect $rollbackTag *> $null
    if ($LASTEXITCODE -eq 0) { docker tag $rollbackTag $img }
  }
  docker compose up -d --no-build --force-recreate
  if ($LASTEXITCODE -ne 0) {
    Write-Warning "Klarte ikke starte forrige versjon — manuell inngripen trengs (docker compose ps / logs)."
  }
}

function Invoke-UpdatePass {
  Push-Location $repoRoot
  try {
    $currentBranch = (git rev-parse --abbrev-ref HEAD).Trim()
    if ($currentBranch -ne $Branch) {
      throw "Arbeidskopien står på '$currentBranch', ikke deploy-branchen '$Branch'. Bytt branch eller angi -Branch."
    }

    git fetch origin $Branch --quiet
    if ($LASTEXITCODE -ne 0) { throw "git fetch feilet." }

    $localSha = (git rev-parse HEAD).Trim()
    $remoteSha = (git rev-parse "origin/$Branch").Trim()
    if ($localSha -eq $remoteSha) {
      Write-Step "Allerede på siste versjon ($($localSha.Substring(0, 7)))."
      # Skriptet er også drifts-inngangen: sørg for at klyngen faktisk kjører
      # (første oppstart, etter reboot med stoppede containere, o.l.).
      Start-ClusterIfDown
      return
    }

    $wasClean = -not (git status --porcelain)
    if (-not $wasClean -and -not $Force) {
      throw "Arbeidskopien har ukommitterte endringer — avbryter. (-Force for å overstyre; da gjøres ingen automatisk git-tilbakerulling.)"
    }

    Write-Step "Ny versjon på origin/${Branch}: $($localSha.Substring(0, 7)) -> $($remoteSha.Substring(0, 7))"

    # 1. Behold kjørende images som :rollback før noe annet skjer.
    $images = @(docker compose config --images | Where-Object { $_ } | Sort-Object -Unique)
    foreach ($img in $images) {
      docker image inspect $img *> $null
      if ($LASTEXITCODE -eq 0) { docker tag $img "$($img.Split(':')[0]):rollback" }
    }

    git pull --ff-only origin $Branch --quiet
    if ($LASTEXITCODE -ne 0) { throw "git pull --ff-only feilet — løs manuelt (divergert historikk?)." }

    # 2. Bygg de nye imagene mens den gamle klyngen fortsatt kjører.
    Write-Step "Bygger nye images (gammel klynge kjører fortsatt)..."
    docker compose build
    if ($LASTEXITCODE -ne 0) {
      if ($wasClean) { git reset --hard $localSha | Out-Null }
      throw "Bygging feilet — kjørende klynge er urørt$(if ($wasClean) { ', koden er rullet tilbake' })."
    }

    # 3. Bytt over og verifiser.
    Write-Step "Bytter til ny versjon..."
    $since = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    docker compose up -d
    if ($LASTEXITCODE -ne 0) {
      Restore-Previous -OldSha $localSha -WasClean $wasClean -Images $images
      throw "docker compose up feilet — rullet tilbake til forrige versjon."
    }

    if (Test-ClusterHealthy -SinceIso $since -TimeoutSeconds $HealthTimeoutSeconds) {
      Write-Step "Oppgradert til $($remoteSha.Substring(0, 7)) og verifisert."
    } else {
      # 4. Helsesjekk feilet: tilbake til forrige versjon.
      Restore-Previous -OldSha $localSha -WasClean $wasClean -Images $images
      throw "Helsesjekk feilet etter oppgradering — rullet tilbake til $($localSha.Substring(0, 7)). Se docker compose logs."
    }
  } finally {
    Pop-Location
  }
}

if ($WatchSeconds -gt 0) {
  Write-Step "Watch-modus: sjekker origin/$Branch hvert ${WatchSeconds}. sekund (R = last .env-config på nytt, Q eller Ctrl+C = avslutt)."
  $quit = $false
  while (-not $quit) {
    try { Invoke-UpdatePass } catch { Write-Warning $_.Exception.Message }
    switch (Wait-WithHotkeys $WatchSeconds) {
      "reload" { try { Invoke-EnvReload } catch { Write-Warning $_.Exception.Message } }
      "quit"   {
        Write-Step "Stopper klyngen (docker compose down)..."
        docker compose down 2>$null
        if ($LASTEXITCODE -ne 0) { Write-Warning "docker compose down feilet — sjekk at Docker er tilgjengelig." }
        Write-Step "Avslutter watch-modus."
        $quit = $true
      }
    }
  }
} else {
  Invoke-UpdatePass
}
