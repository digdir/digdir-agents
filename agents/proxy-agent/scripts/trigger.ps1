# Appender et trigger-event til triggers/inbox.jsonl.
# Simulerer det en Slack-/GitHub-webhook-mottaker ville gjort.
# Eksempel:
#   .\scripts\trigger.ps1 -Prompt "Oppsummer README i workspace" -Source slack -Type app_mention
param(
    [Parameter(Mandatory)][string]$Prompt,
    [ValidateSet("slack", "github", "manual")][string]$Source = "manual",
    [string]$Type = "message",
    [string]$Id = "",
    [hashtable]$Payload = @{}
)

if (-not $Id) {
    $Id = "$Source-$([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())"
}

$evt = [ordered]@{
    id          = $Id
    source      = $Source
    type        = $Type
    received_at = (Get-Date).ToUniversalTime().ToString("o")
    prompt      = $Prompt
}
if ($Payload.Count -gt 0) { $evt.payload = $Payload }

$inbox = Join-Path $PSScriptRoot "..\triggers\inbox.jsonl"
$line = $evt | ConvertTo-Json -Compress -Depth 10
Add-Content -Path $inbox -Value $line -Encoding utf8
Write-Host "Event '$Id' lagt i køen: $line"
