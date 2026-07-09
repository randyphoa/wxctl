#Requires -Version 5.1
<#
.SYNOPSIS
  wxctl installer for Windows — Declarative CLI for managing IBM product resources.

.DESCRIPTION
  Downloads the latest released wxctl.exe for your architecture, verifies its
  SHA-256 checksum, and installs it to %LOCALAPPDATA%\wxctl\bin. No toolchain
  required. This is the Windows counterpart of install.sh.

  Run directly:
    irm https://raw.githubusercontent.com/randyphoa/wxctl/main/install.ps1 | iex

  Or download and run with options:
    .\install.ps1 -Version v0.1.0 -InstallDir C:\tools\wxctl

.PARAMETER Version
  Release tag to install (e.g. v0.1.0). Default: latest. Env: WXCTL_VERSION.

.PARAMETER InstallDir
  Install directory. Default: %LOCALAPPDATA%\wxctl\bin. Env: WXCTL_INSTALL_DIR.
#>
[CmdletBinding()]
param(
    [string]$Version    = $env:WXCTL_VERSION,
    [string]$InstallDir = $(if ($env:WXCTL_INSTALL_DIR) { $env:WXCTL_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'wxctl\bin' })
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
# PowerShell 5.1 may default to TLS 1.0/1.1, which GitHub rejects.
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo    = 'randyphoa/wxctl'
$Bin     = 'wxctl'
$Headers = @{ 'User-Agent' = 'wxctl-install' }  # GitHub API 403s requests without one

function Info($msg) { Write-Host ":: $msg" -ForegroundColor Blue }
function Fail($msg) { Write-Host "wxctl install error: $msg" -ForegroundColor Red; exit 1 }

# --- detect architecture -> release target triple ---------------------------
# PROCESSOR_ARCHITEW6432 is set when a 32-bit shell runs on 64-bit Windows.
$archRaw = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
switch ($archRaw) {
    'AMD64' { $cpu = 'x86_64' }
    'ARM64' { $cpu = 'aarch64' }
    default { Fail "unsupported architecture $archRaw — download from https://github.com/$Repo/releases" }
}
$target = "$cpu-pc-windows-msvc"

# --- resolve version --------------------------------------------------------
if (-not $Version) {
    try {
        $rel = Invoke-RestMethod -UseBasicParsing -Headers $Headers -Uri "https://api.github.com/repos/$Repo/releases/latest"
        $Version = $rel.tag_name
    } catch {
        $Version = $null
    }
    if (-not $Version) { Fail "could not resolve latest release — pin one: -Version v0.1.0" }
}

$Archive = "$Bin-$Version-$target.zip"
$Base    = "https://github.com/$Repo/releases/download/$Version"
Info "installing $Bin $Version ($target)"

# --- download + verify + extract --------------------------------------------
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("wxctl-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $archivePath = Join-Path $tmp $Archive
    try {
        Invoke-WebRequest -UseBasicParsing -Headers $Headers -Uri "$Base/$Archive" -OutFile $archivePath
    } catch {
        Fail "download failed — no $target build for $Version?"
    }

    # Verify the checksum when SHA256SUMS is published alongside the release.
    $sumsPath = Join-Path $tmp 'SHA256SUMS'
    $haveSums = $true
    try {
        Invoke-WebRequest -UseBasicParsing -Headers $Headers -Uri "$Base/SHA256SUMS" -OutFile $sumsPath
    } catch {
        $haveSums = $false
    }
    if ($haveSums) {
        $line = Get-Content $sumsPath | Where-Object { $_ -match "\s$([regex]::Escape($Archive))$" } | Select-Object -First 1
        if ($line) {
            $want = ($line -split '\s+' | Select-Object -First 1).ToLower()
            $got  = (Get-FileHash -Algorithm SHA256 -Path $archivePath).Hash.ToLower()
            if ($got -ne $want) { Fail "checksum mismatch — refusing to install" }
            Info "checksum verified"
        }
    }

    Expand-Archive -Path $archivePath -DestinationPath $tmp -Force
    $src = Join-Path $tmp "$Bin-$Version-$target\$Bin.exe"
    if (-not (Test-Path -LiteralPath $src)) {
        $found = Get-ChildItem -Path $tmp -Recurse -Filter "$Bin.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($found) { $src = $found.FullName }
    }
    if (-not (Test-Path -LiteralPath $src)) { Fail "binary not found inside $Archive" }

    # --- install ------------------------------------------------------------
    if (-not (Test-Path -LiteralPath $InstallDir)) {
        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    }
    $dest = Join-Path $InstallDir "$Bin.exe"
    try {
        Copy-Item -LiteralPath $src -Destination $dest -Force
    } catch {
        Fail "cannot write to $InstallDir — set WXCTL_INSTALL_DIR to a writable path"
    }

    # Strip the Mark of the Web in case the download propagated it to the exe.
    # Parallels the macOS `xattr -d com.apple.quarantine` line in install.sh.
    Unblock-File -LiteralPath $dest -ErrorAction SilentlyContinue

    Info "installed -> $dest"
} finally {
    Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

# --- PATH check -------------------------------------------------------------
$normalized = $InstallDir.TrimEnd('\')
$onPath = $env:PATH -split ';' | Where-Object { $_ -and ($_.TrimEnd('\') -ieq $normalized) }
if (-not $onPath) {
    Info "add to your PATH (current user, persists across new terminals):"
    Info "  [Environment]::SetEnvironmentVariable('PATH', [Environment]::GetEnvironmentVariable('PATH','User') + ';$InstallDir', 'User')"
    Info "then open a new terminal."
}

Info "done — run '$Bin --help' to get started"
