$ErrorActionPreference = "Stop"

$Repository = if ($env:AGENT_GITHUB_REPOSITORY) { $env:AGENT_GITHUB_REPOSITORY } else { "digdir/digdir-agents" }
$Version = $env:AGENT_VERSION
$InstallMode = if ($env:AGENT_INSTALL_MODE) { $env:AGENT_INSTALL_MODE } else { "managed" }
$InstallRoot = if ($env:AGENT_INSTALL_ROOT) { $env:AGENT_INSTALL_ROOT } else { Join-Path $env:LOCALAPPDATA "Agent" }
$BinDirectory = if ($env:AGENT_INSTALL_DIR) { $env:AGENT_INSTALL_DIR } else { Join-Path $InstallRoot "bin" }
$AgentHome = if ($env:AGENT_HOME) { $env:AGENT_HOME } else { Join-Path $env:USERPROFILE ".agent" }
$LocalArchive = $env:AGENT_LOCAL_ARCHIVE
if ($InstallMode -notin @("managed", "standalone")) {
    throw 'AGENT_INSTALL_MODE must be "managed" or "standalone"'
}
$InstallRoot = [IO.Path]::GetFullPath($InstallRoot)
$BinDirectory = [IO.Path]::GetFullPath($BinDirectory)
$AgentHome = [IO.Path]::GetFullPath($AgentHome)
if ($LocalArchive) { $LocalArchive = [IO.Path]::GetFullPath($LocalArchive) }
$JournalPath = Join-Path $InstallRoot "update.json"

# Hands over to `agentctl setup`, which creates a first agent, after a first managed install. An
# upgrade, whose agents exist already, starts it only with AGENT_SETUP=1, and AGENT_SETUP=0 never
# does. A standalone install is for images and CI jobs, which get no daemon either, so it never
# starts setup. Without a console to ask at, setup runs only with AGENT_SETUP_ARGS, which must then
# answer every question. A release without setup, or a setup that does not finish, leaves the
# installation as it is.
function Invoke-Setup($Agentctl, $PreviousRelease) {
    if ($env:AGENT_SETUP -eq "0") { return }
    if ($PreviousRelease -and $env:AGENT_SETUP -ne "1") { return }
    & $Agentctl setup --help *> $null
    if ($LASTEXITCODE -ne 0) { return }
    $SetupArguments = @()
    if ($env:AGENT_SETUP_ARGS) { $SetupArguments = $env:AGENT_SETUP_ARGS -split '\s+' | Where-Object { $_ } }
    if ([Environment]::UserInteractive -and -not [Console]::IsInputRedirected) {
        & $Agentctl --home $AgentHome setup @SetupArguments
    } elseif ($SetupArguments.Count -gt 0) {
        & $Agentctl --home $AgentHome setup --non-interactive @SetupArguments
    } else {
        Write-Host "Run ``agentctl setup`` to create your first agent."
        return
    }
    if ($LASTEXITCODE -ne 0) { Write-Warning "agentctl setup did not finish; run ``agentctl setup`` to try again" }
}

function Invoke-Completion($Journal) {
    $Target = $Journal.targetRelease
    $TargetVersion = $Journal.targetVersion
    if (-not $Target -or -not $TargetVersion) {
        throw "The agentctl update journal does not name a usable staged release: $JournalPath"
    }
    $Agentctl = Join-Path $Target "agentctl.exe"
    if (-not (Test-Path $Agentctl -PathType Leaf)) {
        throw "The agentctl update journal does not name a usable staged release: $JournalPath"
    }
    $Arguments = @(
        "--home", $AgentHome, "self", "__complete-update",
        "--install-root", $InstallRoot, "--bin-directory", $BinDirectory,
        "--target-release", $Target,
        "--target-version", $TargetVersion, "--repository", $Repository
    )
    if ($Journal.previousRelease) { $Arguments += @("--previous-release", $Journal.previousRelease) }
    & $Agentctl @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Target agentctl updater exited with code $LASTEXITCODE" }
}

if ($InstallMode -eq "managed" -and (Test-Path $JournalPath -PathType Leaf)) {
    $Journal = Get-Content $JournalPath -Raw | ConvertFrom-Json
    if ($Journal.phase -ne "complete") {
        Invoke-Completion $Journal
        Write-Host "Installed agentctl and agentd to $BinDirectory"
        exit 0
    }
}

if ($LocalArchive -and -not $Version) { throw "AGENT_VERSION is required when AGENT_LOCAL_ARCHIVE is set" }
if (-not $Version) {
    $Page = 1
    do {
        $Releases = Invoke-RestMethod "https://api.github.com/repos/$Repository/releases?per_page=100&page=$Page"
        $Release = $Releases | Where-Object { $_.tag_name -like "agentctl/v*" } | Select-Object -First 1
        $Page++
    } while (-not $Release -and $Releases.Count -eq 100)
    if (-not $Release) { throw "Could not resolve the latest agentctl release" }
    $Version = $Release.tag_name.Substring("agentctl/".Length)
}
if (-not $Version.StartsWith("v")) { $Version = "v$Version" }

$Architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
$Platform = switch ($Architecture) {
    "X64" { "windows-x86_64" }
    "Arm64" { "windows-aarch64" }
    default { throw "Unsupported Windows architecture: $Architecture" }
}
$Temporary = Join-Path ([System.IO.Path]::GetTempPath()) ("agentctl-install-" + [guid]::NewGuid())
$SourceRelease = Join-Path $Temporary "release"
New-Item -ItemType Directory -Path $Temporary | Out-Null
try {
    $ReleasesDirectory = Join-Path $InstallRoot "releases"
    $Target = Join-Path $ReleasesDirectory "$Version-$Platform"
    if ($InstallMode -eq "standalone" -or -not (Test-Path $Target -PathType Container)) {
        if ($LocalArchive) {
            $Archive = Split-Path $LocalArchive -Leaf
            Copy-Item $LocalArchive (Join-Path $Temporary $Archive)
            $Checksum = if ($env:AGENT_LOCAL_ARCHIVE_SHA256) { $env:AGENT_LOCAL_ARCHIVE_SHA256 } else { "$LocalArchive.sha256" }
        } else {
            $Archive = "agent-$Platform.tar.gz"
            $Base = "https://github.com/$Repository/releases/download/agentctl/$Version"
            Invoke-WebRequest "$Base/$Archive" -OutFile (Join-Path $Temporary $Archive)
            $Checksum = Join-Path $Temporary "$Archive.sha256"
            Invoke-WebRequest "$Base/$Archive.sha256" -OutFile $Checksum
        }
        $Expected = (Get-Content $Checksum -Raw).Split(' ')[0].Trim().ToLowerInvariant()
        $Actual = (Get-FileHash (Join-Path $Temporary $Archive) -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($Actual -ne $Expected) { throw "Agent archive checksum mismatch" }
        New-Item -ItemType Directory -Path $SourceRelease | Out-Null
        Push-Location $SourceRelease
        try {
            tar -xzf "../$Archive"
            if ($LASTEXITCODE -ne 0) { throw "Failed to extract agentctl archive" }
        } finally {
            Pop-Location
        }
        if ($InstallMode -eq "standalone") {
            New-Item -ItemType Directory -Force -Path $BinDirectory | Out-Null
            Copy-Item -Force (Join-Path $SourceRelease "agentctl.exe") (Join-Path $BinDirectory "agentctl.exe")
            Copy-Item -Force (Join-Path $SourceRelease "agentd.exe") (Join-Path $BinDirectory "agentd.exe")
            Write-Host "Installed standalone agentctl and agentd to $BinDirectory"
            exit 0
        }
        & (Join-Path $SourceRelease "agentctl.exe") --home $AgentHome self __publish-release `
            --install-root $InstallRoot --bin-directory $BinDirectory `
            --source-release $SourceRelease --target-version $Version
        if ($LASTEXITCODE -ne 0) { throw "Target agentctl publisher exited with code $LASTEXITCODE" }
    }

    $Previous = $null
    $Current = Join-Path $InstallRoot "current"
    if (Test-Path $Current -PathType Leaf) {
        $Candidate = (Get-Content $Current -Raw).Trim()
        if ($Candidate -and (Test-Path -LiteralPath $Candidate -PathType Container)) { $Previous = $Candidate }
    }
    $Journal = [pscustomobject]@{
        targetRelease = $Target
        targetVersion = $Version
        previousRelease = $Previous
    }
    Invoke-Completion $Journal
} finally {
    if (Test-Path $Temporary) { Remove-Item -Recurse -Force $Temporary }
}

$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (($UserPath -split ';') -notcontains $BinDirectory) {
    [Environment]::SetEnvironmentVariable("Path", (($UserPath.TrimEnd(';') + ';' + $BinDirectory).TrimStart(';')), "User")
}
Write-Host "Installed agentctl and agentd to $BinDirectory"
Invoke-Setup (Join-Path $BinDirectory "agentctl.exe") $Previous
