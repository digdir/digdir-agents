#!/usr/bin/env bash
# Exercises standalone installation and the managed installation and upgrade lifecycle.
set -euo pipefail

old_version="v0.0.1-dev.upgrade-smoke"
target_version="v0.1.0-preview.2.smoke"
# Agent homes stay under a short temporary path, which keeps the socket paths
# in them short.
smoke_root="$(mktemp -d "${TMPDIR:-/tmp}/au.XXXXXXXX")"
export AGENT_SMOKE_ID="${smoke_root##*/}"
# Passed to Cargo explicitly, so that the binaries are where this script looks.
target_directory="${CARGO_TARGET_DIR:-${CARGO_BUILD_TARGET_DIR:-$(git rev-parse --show-toplevel)/target}}"
# Git for Windows reports paths as D:/..., which tar reads as a remote host.
if command -v cygpath > /dev/null; then
  target_directory="$(cygpath -u "${target_directory}")"
fi
# The fixtures are built in the workspace's own target directory, so that only
# agent recompiles for each version. That leaves its agentctl and agentd built
# as the target version, until the next build without AGENT_VERSION.
binary_directory="${target_directory}/debug"
# Binaries, archives and installations are kept beside the target directory:
# CI runners and sandboxes may keep /tmp on a small tmpfs, and the copies of the
# dev-profile binaries do not fit there.
files_root="${target_directory}/installation-test-${AGENT_SMOKE_ID}"
target_binaries="${files_root}/target-binaries"
old_archive="${files_root}/old.tar.gz"
target_archive="${files_root}/target.tar.gz"
export AGENT_HOME="${smoke_root}/home"

cleanup() {
  status=$?
  log="${AGENT_HOME}/agentd.log"
  if [ "${RUNNER_OS:-}" = "Windows" ]; then
    log="$(cygpath -u "${AGENT_HOME}")/agentd.log"
  fi
  if [ "${status}" -ne 0 ] && [ -f "${log}" ]; then
    printf '%s\n' 'agentd.log:' >&2
    cat "${log}" >&2
  fi
  if [ "${RUNNER_OS:-}" = "Windows" ]; then
    # shellcheck disable=SC2016 # PowerShell expands its own environment variables.
    pwsh -NoProfile -Command '
      Get-CimInstance Win32_Process |
        Where-Object {
          $_.Name -eq "agentd.exe" -and
          ($_.CommandLine -like "*$env:AGENT_SMOKE_ID*" -or $_.ExecutablePath -like "*$env:AGENT_SMOKE_ID*")
        } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
    ' 2>/dev/null || true
  else
    pkill -f "agentd.*--home ${AGENT_HOME}" 2>/dev/null || true
  fi
  rm -rf -- "${smoke_root}" "${files_root}"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "${smoke_root}" "${files_root}"

# Build release fixtures

# --tests resolves the dependencies' features as the workspace's other builds
# do, which --bins alone does not, so no dependency is rebuilt.
AGENT_VERSION="${old_version}" cargo build --locked --target-dir "${target_directory}" -p agent --bins --tests
./agentctl/package.sh "${old_archive}" "${binary_directory}"
AGENT_VERSION="${target_version}" cargo build --locked --target-dir "${target_directory}" -p agent --bins --tests
suffix=""
if [ -f "${binary_directory}/agentctl.exe" ]; then
  suffix=".exe"
fi
mkdir "${target_binaries}"
cp "${binary_directory}/agentctl${suffix}" "${binary_directory}/agentd${suffix}" "${target_binaries}/"
# Binaries built on a developer's Mac carry extended attributes, which those built on a CI runner do not. tar on
# macOS stores them as extra archive entries unless package.sh prevents it, and the updater refuses such an archive.
if [ "$(uname -s)" = "Darwin" ]; then
  xattr -w no.digdir.agentctl.installation-test 1 "${target_binaries}/agentctl" "${target_binaries}/agentd"
fi
./agentctl/package.sh "${target_archive}" "${target_binaries}"

if [ "${RUNNER_OS:-}" = "Windows" ]; then
  # EXPERIMENT 2 for #536: homes under %LOCALAPPDATA%\Temp with Defender real-time protection on.
  export AGENT_SMOKE_BINARIES="$(cygpath -w "${target_binaries}")"
  # shellcheck disable=SC2016 # PowerShell expands its own environment variables.
  pwsh -NoProfile -Command '
    $ErrorActionPreference = "Continue"
    $agentctl = Join-Path $env:AGENT_SMOKE_BINARIES "agentctl.exe"
    function Show-Defender {
      try {
        Get-MpComputerStatus | Select-Object AMRunningMode, AMServiceEnabled, AntivirusEnabled, RealTimeProtectionEnabled, OnAccessProtectionEnabled, BehaviorMonitorEnabled, IoavProtectionEnabled | Format-List | Out-String | Write-Host
        $pref = Get-MpPreference
        Write-Host "DisableRealtimeMonitoring=$($pref.DisableRealtimeMonitoring) ExclusionPath=$($pref.ExclusionPath -join ";") ExclusionProcess=$($pref.ExclusionProcess -join ";")"
      } catch { Write-Host "Defender status unavailable: $_" }
      & fltmc | Write-Host
    }
    function Stop-Agentd {
      Get-CimInstance Win32_Process |
        Where-Object { $_.Name -eq "agentd.exe" -and $_.CommandLine -like "*$env:AGENT_SMOKE_ID*" } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
      Start-Sleep -Seconds 1
    }
    $failed = @()
    function Try-Homes($round) {
      $temp = Join-Path $env:LOCALAPPDATA "Temp"
      $homes = [ordered]@{
        "temp" = Join-Path $temp "h-$env:AGENT_SMOKE_ID-$round"
        "temp-space-nonascii" = Join-Path $temp "h $env:AGENT_SMOKE_ID $([char]0x00F8)-$round"
        "profile" = Join-Path $env:USERPROFILE "h-$env:AGENT_SMOKE_ID-$round"
      }
      foreach ($name in $homes.Keys) {
        $home_ = $homes[$name]
        Write-Host "=== $round/$name : $home_"
        foreach ($attempt in 1, 2) {
          $started = Get-Date
          & $agentctl --home $home_ get agents
          $code = $LASTEXITCODE
          Write-Host "--- $round/$name attempt $attempt exit $code after $(((Get-Date) - $started).TotalSeconds)s"
          if ($code -ne 0) { $script:failed += "$round/$name/$attempt" }
        }
        $log = Join-Path $home_ "agentd.log"
        if (Test-Path $log) { Get-Content $log | Where-Object { $_ -notmatch "sea_orm_migration" } | Select-Object -Last 15 | Write-Host }
        Stop-Agentd
        $socket = Join-Path $home_ "run\agentd.sock"
        if (Test-Path $socket) {
          try { Remove-Item -Force $socket -ErrorAction Stop; Write-Host "--- $round/$name socket deleted after kill" }
          catch { Write-Host "--- $round/$name socket NOT deletable after kill: $_"; $script:failed += "$round/$name/delete" }
        }
      }
    }
    Show-Defender
    Try-Homes "as-is"
    try {
      Set-MpPreference -DisableRealtimeMonitoring $false -DisableBehaviorMonitoring $false -DisableIOAVProtection $false -ErrorAction Stop
      foreach ($path in (Get-MpPreference).ExclusionPath) { if ($path) { Remove-MpPreference -ExclusionPath $path } }
      foreach ($process in (Get-MpPreference).ExclusionProcess) { if ($process) { Remove-MpPreference -ExclusionProcess $process } }
      Start-Sleep -Seconds 5
    } catch { Write-Host "could not turn on Defender: $_" }
    Show-Defender
    Try-Homes "realtime"
    Stop-Agentd
    if ($failed.Count -gt 0) { throw "EXPERIMENT: failed: $($failed -join ", ")" }
    Write-Host "EXPERIMENT: all passed"
  '

  # Standalone installation

  standalone_root="${files_root}/standalone"
  mkdir -p "${standalone_root}/managed"
  printf '{ "phase": "prepared" }\n' > "${standalone_root}/managed/update.json"
  export AGENT_INSTALL_MODE=standalone
  export AGENT_INSTALL_ROOT="$(cygpath -w "${standalone_root}/managed")"
  export AGENT_INSTALL_DIR="$(cygpath -w "${standalone_root}/bin")"
  export AGENT_HOME="$(cygpath -w "${standalone_root}/home")"
  export AGENT_VERSION="${target_version}"
  export AGENT_LOCAL_ARCHIVE="$(cygpath -w "${target_archive}")"
  pwsh -NoProfile -File "$(cygpath -w agentctl/install.ps1)"
  # shellcheck disable=SC2016 # PowerShell expands its own environment variables.
  pwsh -NoProfile -Command '
    $agentctl = Join-Path $env:AGENT_INSTALL_DIR "agentctl.exe"
    $agentd = Join-Path $env:AGENT_INSTALL_DIR "agentd.exe"
    if (-not (Test-Path $agentctl -PathType Leaf) -or -not (Test-Path $agentd -PathType Leaf)) {
      throw "standalone installation did not copy both binaries"
    }
    $actual = (& $agentctl --version | Out-String).Trim()
    if ($actual -ne "agentctl $env:AGENT_VERSION") {
      throw "standalone agentctl reports $actual"
    }
    if (Test-Path (Join-Path $env:AGENT_INSTALL_ROOT "releases")) {
      throw "standalone installation created a managed release tree"
    }
  '
  test "$(cat "${standalone_root}/managed/update.json")" = '{ "phase": "prepared" }'
  test ! -e "${standalone_root}/home"
  unset AGENT_INSTALL_MODE

  # Managed installation and self-update

  AGENT_INSTALL_ROOT="$(cygpath -w "${files_root}/install")"
  AGENT_INSTALL_DIR="$(cygpath -w "${files_root}/bin")"
  AGENT_HOME="$(cygpath -w "${smoke_root}/home")"
  export AGENT_VERSION="${old_version}"
  AGENT_LOCAL_ARCHIVE="$(cygpath -w "${old_archive}")"
  export AGENT_INSTALL_ROOT AGENT_INSTALL_DIR AGENT_HOME AGENT_LOCAL_ARCHIVE
  pwsh -NoProfile -File "$(cygpath -w agentctl/install.ps1)"
  export AGENT_SMOKE_BIN="${AGENT_INSTALL_DIR}"
  AGENT_LOCAL_ARCHIVE="$(cygpath -w "${target_archive}")"
  export AGENT_LOCAL_ARCHIVE
  export AGENT_TARGET_VERSION="${target_version}"
  unset AGENT_INSTALL_ROOT AGENT_INSTALL_DIR
  # shellcheck disable=SC2016 # PowerShell expands its own environment variables.
  pwsh -NoProfile -Command '
    $agentctl = Join-Path $env:AGENT_SMOKE_BIN "agentctl.cmd"
    & $agentctl --home $env:AGENT_HOME self update --version $env:AGENT_TARGET_VERSION
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $actual = (& $agentctl --version | Out-String).Trim()
    if ($actual -ne "agentctl $env:AGENT_TARGET_VERSION") {
      throw "updated agentctl reports $actual"
    }
    & $agentctl --home $env:AGENT_HOME self update --version $env:AGENT_TARGET_VERSION
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
  '
  AGENT_INSTALL_ROOT="$(cygpath -w "${files_root}/install")"
  export AGENT_INSTALL_DIR="${AGENT_SMOKE_BIN}"
  AGENT_LOCAL_ARCHIVE="$(cygpath -w "${old_archive}")"
  export AGENT_VERSION="${old_version}"
  export AGENT_INSTALL_ROOT AGENT_LOCAL_ARCHIVE
  pwsh -NoProfile -File "$(cygpath -w agentctl/install.ps1)"
  # shellcheck disable=SC2016 # PowerShell expands its own environment variables.
  pwsh -NoProfile -Command '
    $agentctl = Join-Path $env:AGENT_SMOKE_BIN "agentctl.cmd"
    $actual = (& $agentctl --version | Out-String).Trim()
    if ($actual -ne "agentctl $env:AGENT_VERSION") {
      throw "installer did not replace a newer release with development build $actual"
    }
  '

  # Completed journals are history, even when their releases have been pruned.
  # shellcheck disable=SC2016 # PowerShell expands its own environment variables.
  pwsh -NoProfile -Command '
    $journalPath = Join-Path $env:AGENT_INSTALL_ROOT "update.json"
    $journal = Get-Content $journalPath -Raw | ConvertFrom-Json
    $journal.previousRelease = Join-Path $env:AGENT_INSTALL_ROOT "releases/missing-previous"
    $journal.targetRelease = Join-Path $env:AGENT_INSTALL_ROOT "releases/missing-target"
    $journal | ConvertTo-Json | Set-Content $journalPath
    $agentctl = Join-Path $env:AGENT_SMOKE_BIN "agentctl.cmd"
    & $agentctl --home $env:AGENT_HOME self update --check --version $env:AGENT_VERSION
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    & $agentctl --home $env:AGENT_HOME get agents
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    Join-Path $env:AGENT_INSTALL_ROOT "releases/missing-current" |
      Set-Content (Join-Path $env:AGENT_INSTALL_ROOT "current")
  '
  # The installer repairs a dangling current pointer without retaining it as
  # the previous release in the next update journal.
  pwsh -NoProfile -File "$(cygpath -w agentctl/install.ps1)"
  # shellcheck disable=SC2016 # PowerShell expands its own environment variables.
  pwsh -NoProfile -Command '
    $journal = Get-Content (Join-Path $env:AGENT_INSTALL_ROOT "update.json") -Raw | ConvertFrom-Json
    if ($journal.phase -ne "complete" -or $journal.previousRelease) {
      throw "installer did not finish recovery from missing releases"
    }
    $agentctl = Join-Path $env:AGENT_SMOKE_BIN "agentctl.cmd"
    $actual = (& $agentctl --version | Out-String).Trim()
    if ($actual -ne "agentctl $env:AGENT_VERSION") { throw "recovered launcher reports $actual" }
    & $agentctl --home $env:AGENT_HOME get agents
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
  '
else
  # Standalone installation

  standalone_root="${files_root}/standalone"
  mkdir -p "${standalone_root}/managed"
  printf '{ "phase": "prepared" }\n' > "${standalone_root}/managed/update.json"
  AGENT_INSTALL_MODE=standalone \
    AGENT_INSTALL_ROOT="${standalone_root}/managed" \
    AGENT_INSTALL_DIR="${standalone_root}/bin" \
    AGENT_HOME="${standalone_root}/home" \
    AGENT_VERSION="${target_version}" \
    AGENT_LOCAL_ARCHIVE="${target_archive}" \
    ./agentctl/install.sh
  test -x "${standalone_root}/bin/agentctl"
  test -x "${standalone_root}/bin/agentd"
  test "$("${standalone_root}/bin/agentctl" --version)" = "agentctl ${target_version}"
  test "$(find "${standalone_root}/bin" -type f | wc -l | tr -d ' ')" = 2
  test ! -e "${standalone_root}/managed/releases"
  test ! -e "${standalone_root}/home"
  test "$(cat "${standalone_root}/managed/update.json")" = '{ "phase": "prepared" }'

  # Managed installation and self-update

  export AGENT_INSTALL_ROOT="${files_root}/install"
  export AGENT_INSTALL_DIR="${files_root}/bin"
  export AGENT_HOME="${smoke_root}/home"
  export AGENT_VERSION="${old_version}"
  export AGENT_LOCAL_ARCHIVE="${old_archive}"
  ./agentctl/install.sh
  agentctl="${AGENT_INSTALL_DIR}/agentctl"
  export AGENT_LOCAL_ARCHIVE="${target_archive}"
  unset AGENT_INSTALL_ROOT AGENT_INSTALL_DIR
  "${agentctl}" --home "${AGENT_HOME}" self update --version "${target_version}"
  test "$("${agentctl}" --version)" = "agentctl ${target_version}"
  "${agentctl}" --home "${AGENT_HOME}" self update --version "${target_version}"
  export AGENT_INSTALL_ROOT="${files_root}/install"
  export AGENT_INSTALL_DIR="${files_root}/bin"
  export AGENT_LOCAL_ARCHIVE="${old_archive}"
  export AGENT_VERSION="${old_version}"
  ./agentctl/install.sh
  test "$("${agentctl}" --version)" = "agentctl ${old_version}"

  # Completed journals and dangling current links must not block recovery.
  cat > "${AGENT_INSTALL_ROOT}/update.json" <<EOF
{
  "formatVersion": 1,
  "previousRelease": "${AGENT_INSTALL_ROOT}/releases/missing-previous",
  "targetRelease": "${AGENT_INSTALL_ROOT}/releases/missing-target",
  "targetVersion": "${old_version}",
  "phase": "complete"
}
EOF
  "${agentctl}" --home "${AGENT_HOME}" self update --check --version "${old_version}"
  "${agentctl}" --home "${AGENT_HOME}" get agents
  ln -sfn "${AGENT_INSTALL_ROOT}/releases/missing-current" "${AGENT_INSTALL_ROOT}/current"
  ./agentctl/install.sh
  test "$("${agentctl}" --version)" = "agentctl ${old_version}"
  grep -q '^  "phase": "complete"$' "${AGENT_INSTALL_ROOT}/update.json"
  grep -q '^  "previousRelease": null,$' "${AGENT_INSTALL_ROOT}/update.json"
  "${agentctl}" --home "${AGENT_HOME}" get agents

  # The installer installs a release again when it is installed but does not start, as
  # happens when macOS refuses to run the file.
  release="$(readlink "${AGENT_INSTALL_ROOT}/current")"
  rm "${release}/agentctl"
  printf '#!/bin/sh\nkill -KILL $$\n' > "${release}/agentctl"
  chmod 0755 "${release}/agentctl"
  ./agentctl/install.sh
  test "$("${agentctl}" --version)" = "agentctl ${old_version}"
  "${agentctl}" --home "${AGENT_HOME}" get agents
fi
