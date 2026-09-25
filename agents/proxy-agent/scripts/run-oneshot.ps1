# Starter en engangs-container som kjører pi med prompten og avsluttes.
# Eksempel:
#   .\scripts\run-oneshot.ps1 "Lag en hello.py i workspace"
param(
    [Parameter(Mandatory)][string]$Prompt
)

$repo = Split-Path $PSScriptRoot -Parent
docker compose --project-directory $repo run --rm pi-agent oneshot $Prompt
