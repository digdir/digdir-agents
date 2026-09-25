<#
.SYNOPSIS
  Kjører hele pipelinen i forgrunnen for utvikling: logger i konsollen,
  Ctrl+C stopper alt, og ingenting starter igjen av seg selv.

.DESCRIPTION
  Setter RESTART_POLICY=no slik at compose-filenes
  `restart: ${RESTART_POLICY:-unless-stopped}` ikke gir restart-policy på
  dev-containerne. Alle ekstra argumenter sendes videre til
  `docker compose up` (f.eks. `.\scripts\dev.ps1 --force-recreate`).
#>
$env:RESTART_POLICY = "no"
Push-Location (Split-Path $PSScriptRoot -Parent)
try {
  docker compose up --build @args
} finally {
  Pop-Location
}
