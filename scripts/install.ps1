# Install tk via irm | iex, per ADR-0013.
#
# Usage:
#     irm https://raw.githubusercontent.com/lithammer/tk/main/scripts/install.ps1 | iex
#
# Environment variables:
#     TK_VERSION       Release tag to install (e.g. v0.0.1). Defaults to latest.
#     TK_INSTALL_DIR   Destination directory. Defaults to %LOCALAPPDATA%\tk\bin.
#
# This is the Windows counterpart to scripts/install.sh. It keeps the same
# trust model: the trust root is TLS + GitHub Releases, verification is a
# smoke `--version` run of the freshly downloaded binary, and — for the same
# trust-root reasoning in ADR-0013 — it ships NO checksum or signature
# (a checksum served from the same origin only catches corruption, not
# tampering). Windows-specific behaviour the POSIX script has no equivalent
# for: clearing the downloaded file's Mark-of-the-Web and adding the install
# directory to the User PATH.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Repo = 'lithammer/tk'

# Only the x86_64 Windows asset is published (release.yml builds
# x86_64-pc-windows-gnu); it runs natively on AMD64 and under emulation on
# ARM64, so the asset is fetched unconditionally.
$Asset = 'tk-x86_64-pc-windows-gnu.exe'

# Wrapped in a function so error paths can `throw` instead of `exit`: under
# `irm | iex` the script shares the caller's scope, and `exit` there would
# terminate the user's whole PowerShell session.
function Invoke-Install {
    # TLS 1.2 for Windows PowerShell 5.1; PowerShell 7+ already negotiates it.
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    } catch {
        # Already enabled or not settable in this host; the download will fail
        # loudly below if the protocol is genuinely unavailable.
    }

    # --- 1. Resolve and validate inputs ----------------------------------
    $version = $env:TK_VERSION
    if ($version) {
        # TK_VERSION lands inside the asset URL; without validation a value
        # like '../../../attacker/evil-tk/releases/download/v1' redirects the
        # download to another repo while TLS still passes (host stays
        # github.com). Reject anything outside the safe release-tag set.
        # Mirrors the equivalent guard in install.sh.
        if ($version -notmatch '^[A-Za-z0-9._+-]+$') {
            throw "tk: TK_VERSION must match [A-Za-z0-9._+-] (got: $version)"
        }
    }

    $destDir = $env:TK_INSTALL_DIR
    if (-not $destDir) {
        $destDir = Join-Path $env:LOCALAPPDATA 'tk\bin'
    }

    if ($version) {
        $url = "https://github.com/$Repo/releases/download/$version/$Asset"
    } else {
        $url = "https://github.com/$Repo/releases/latest/download/$Asset"
    }

    # --- 2. Stage download next to the destination -----------------------
    # Stage inside the destination directory so the final Move-Item is a
    # same-volume rename, matching install.sh's atomic-placement property.
    New-Item -ItemType Directory -Path $destDir -Force | Out-Null
    $stagingDir = Join-Path $destDir (".tk." + [System.IO.Path]::GetRandomFileName())
    New-Item -ItemType Directory -Path $stagingDir -Force | Out-Null
    $staging = Join-Path $stagingDir 'tk.exe'

    try {
        # Invoke-WebRequest is an order of magnitude slower with the progress
        # bar rendering on every chunk; silence it for the download only.
        $ProgressPreference = 'SilentlyContinue'
        try {
            Invoke-WebRequest -Uri $url -OutFile $staging
        } catch {
            throw "tk: failed to download $url`n    $($_.Exception.Message)"
        }

        # Clear the Mark-of-the-Web BEFORE the smoke run; otherwise executing a
        # freshly downloaded binary can be blocked or trigger a SmartScreen
        # prompt.
        Unblock-File -Path $staging -ErrorAction SilentlyContinue

        # Verification is a successful `--version`, not a checksum (ADR-0013).
        $newVersion = ''
        try {
            $newVersion = (& $staging --version 2>&1 | Out-String).Trim()
        } catch {
            $newVersion = ''
        }
        if (-not $newVersion) {
            throw "tk: downloaded binary failed verification ($staging --version)"
        }

        # --- 3. Detect a prior install, then place atomically ------------
        $binary = Join-Path $destDir 'tk.exe'
        $oldVersion = ''
        if (Test-Path $binary) {
            try {
                $oldVersion = (& $binary --version 2>&1 | Out-String).Trim()
            } catch {
                $oldVersion = ''
            }
        }
        Move-Item -Path $staging -Destination $binary -Force
    } finally {
        if (Test-Path $stagingDir) {
            Remove-Item -Recurse -Force $stagingDir
        }
    }

    # --- 4. Success line (mirrors install.sh wording) --------------------
    # $newVersion / $oldVersion are the `tk --version` output, which clap
    # prefixes with the binary name ("tk v0.1.2 (<triple>)"). Don't prepend
    # another literal "tk" or the line reads "Installed tk tk v...".
    if (-not $oldVersion) {
        Write-Host "Installed $newVersion at $binary"
    } elseif ($oldVersion -eq $newVersion) {
        Write-Host "Reinstalled $newVersion at $binary"
    } else {
        Write-Host "Upgraded $oldVersion -> $newVersion at $binary"
    }

    # --- 5. PATH advice / setup (User scope, idempotent) ----------------
    # Read the persisted User PATH, NOT $env:Path: the live process value
    # merges Machine and session entries, and writing that back to the User
    # scope would permanently bake them in — a classic destructive-installer
    # bug. SetEnvironmentVariable does not update the live session, so the
    # message tells the user to restart their terminal.
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = @()
    if ($userPath) {
        $entries = $userPath -split ';' | Where-Object { $_ -ne '' }
    }
    if ($entries -notcontains $destDir) {
        $newUserPath = (@($entries) + $destDir) -join ';'
        [Environment]::SetEnvironmentVariable('Path', $newUserPath, 'User')
        Write-Host "tk: added $destDir to your User PATH"
        Write-Host "    restart your terminal for the change to take effect"
    }
}

Invoke-Install
