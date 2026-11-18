# SPDX-License-Identifier: Apache-2.0
#
# Installs Ophio on Windows with one command, in an ordinary PowerShell
# (not as administrator):
#
#   irm https://raw.githubusercontent.com/ophio-app/ophio/main/scripts/get-ophio.ps1 | iex
#
# It downloads the newest Windows release from Ophio's GitHub releases, checks
# it against the release's published SHA256SUMS, and runs the same
# `ophio.exe install` the Windows guide uses. A first install then starts
# `ophio.exe setup`, which starts Ophio, connects your phone and checks what
# works. Nothing is installed if the checksum does not match. It installs for
# you alone, under %LOCALAPPDATA%\Programs\Ophio, and changes no Windows
# security setting.
#
# Settings, as environment variables set before running it:
#   $env:OPHIO_VERSION = '0.3.0'   install that release instead of the newest
#   $env:OPHIO_NO_SETUP = '1'      don't start setup after a first install
#   $env:OPHIO_RELEASES            where releases are downloaded from (for
#                                  mirrors and tests)
#
# Everything runs inside one script block that starts on the last line, so a
# download that breaks off part way runs nothing. It never calls `exit`, which
# would close the PowerShell window it was pasted into.

& {
    Set-StrictMode -Version 2.0
    $ErrorActionPreference = 'Stop'
    # The product name, spelled without non-ASCII text in this file, which
    # Windows PowerShell 5.1 would misread when the file is saved and run.
    $product = "$([char]0xD8)phio"
    $guide = 'https://github.com/ophio-app/ophio/blob/main/packaging/windows/INSTALL.md'
    $warnings = "$guide#windows-warnings-and-how-to-continue"

    function Say([string] $text) { Write-Host $text }
    function Stop-Install([string] $text) {
        Write-Host "$product was not installed: $text" -ForegroundColor Red
        throw [System.OperationCanceledException]::new('ophio-get')
    }

    function Read-InstallState([string] $program) {
        # A leftover launcher can be blocked, broken, or return no valid status.
        # In that case, keep the first-install path so we wait and offer setup.
        try {
            $ErrorActionPreference = 'Continue'
            $text = (& $program update --json 2>$null) -join "`n"
            if ($LASTEXITCODE -eq 0 -and $text) { return ($text | ConvertFrom-Json -ErrorAction Stop) }
        } catch {
            return $null
        } finally {
            $ErrorActionPreference = 'Stop'
        }
        return $null
    }

    function Get-Ophio {
        $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
        $principal = [Security.Principal.WindowsPrincipal]::new($identity)
        if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
            Stop-Install "open an ordinary PowerShell, not one run as administrator, and run this again. $product and the agents it starts run as you, and would otherwise run as administrator too."
        }
        # Windows' own architecture, not this PowerShell's: a 32-bit or
        # emulated PowerShell reports its own. (Windows PowerShell 5.1 has no
        # RuntimeInformation.OSArchitecture to ask.)
        $arch = (Get-ItemProperty -Path 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment' -Name PROCESSOR_ARCHITECTURE).PROCESSOR_ARCHITECTURE
        if ("$arch" -ne 'AMD64') {
            Stop-Install "there is no $product download for Windows on $arch yet, only for x64 (Intel and AMD) PCs."
        }

        # Smart App Control blocks programs that aren't signed, whatever way
        # they were downloaded. This only reads the setting.
        $policy = Get-ItemProperty -Path 'HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy' -Name VerifiedAndReputablePolicyState -ErrorAction SilentlyContinue
        $sac = if ($policy) { $policy.VerifiedAndReputablePolicyState } else { 0 }
        if ($sac -eq 1) {
            Stop-Install "Smart App Control is on, and it blocks $product because the Windows download isn't signed yet. Nothing was downloaded. The Windows guide explains your choices: $warnings"
        }

        $version = "$env:OPHIO_VERSION".Trim()
        if ($version.StartsWith('v')) { $version = $version.Substring(1) }
        if ($version -and $version -notmatch '^[0-9][0-9A-Za-z.+-]*$') {
            Stop-Install "$version isn't a version number, such as 0.3.0."
        }
        $releases = "$env:OPHIO_RELEASES".Trim().TrimEnd('/')
        if (-not $releases) { $releases = 'https://github.com/ophio-app/ophio/releases' }

        # Windows PowerShell 5.1 may not offer TLS 1.2 by default; GitHub needs it.
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

        $work = Join-Path ([IO.Path]::GetTempPath()) ("ophio-get-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
        New-Item -ItemType Directory -Path $work | Out-Null
        try {
            if ($version) {
                $where = "$releases/download/v$version"
                $which = "release $version"
            } else {
                $where = "$releases/latest/download"
                $which = 'newest release'
            }
            Say "Finding $product's $which..."
            $sums = Join-Path $work 'SHA256SUMS'
            try {
                Invoke-WebRequest -UseBasicParsing -Uri "$where/SHA256SUMS" -OutFile $sums
            } catch {
                Stop-Install "couldn't download the $which's SHA256SUMS from $where. Check this PC's internet connection, or that the release exists."
            }
            $pattern = '^([0-9a-f]{64})  (ophio-([0-9][0-9A-Za-z.+-]*)-x86_64-windows\.zip)$'
            $lines = @(Get-Content -LiteralPath $sums | Where-Object { $_ -cmatch $pattern })
            if ($lines.Count -eq 0) {
                Stop-Install "$product's $which has no Windows download. The release page lists what each release has: $releases"
            }
            if ($lines.Count -ne 1) {
                Stop-Install "the release's SHA256SUMS lists more than one Windows download."
            }
            $null = $lines[0] -cmatch $pattern
            $expected = $Matches[1]
            $name = $Matches[2]
            $found = $Matches[3]
            $folder = $name.Substring(0, $name.Length - 4)

            $installed = Join-Path $env:LOCALAPPDATA 'Programs\Ophio\ophio.exe'
            $first = $true
            if (Test-Path -LiteralPath $installed) {
                $state = Read-InstallState $installed
                $now = if ($state) { $state.installed } else { $null }
                # The launcher can exist before the first successful swap.
                $first = -not ($now -and $now.version -and $now.installed_at -gt 0)
                if (-not $first -and $now.version -eq $found) {
                    Say "$product $found is already installed, so there is nothing to download."
                    Say "To run setup again, run: & '$installed' setup"
                    return
                }
            }

            Say "Downloading $name..."
            $zip = Join-Path $work $name
            $progress = $ProgressPreference
            # The progress bar makes Windows PowerShell 5.1 download many times slower.
            $ProgressPreference = 'SilentlyContinue'
            try {
                Invoke-WebRequest -UseBasicParsing -Uri "$releases/download/v$found/$name" -OutFile $zip
            } catch {
                Stop-Install "couldn't download $name from $releases/download/v$found."
            } finally {
                $ProgressPreference = $progress
            }

            $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLowerInvariant()
            if ($actual -ne $expected) {
                Stop-Install "$name doesn't match the release's SHA256SUMS (expected $expected, got $actual). It may have been damaged or changed on the way. Nothing was installed. Try again later, or download it from the release page and check it by hand."
            }
            Say "Checked $name against the release's SHA256SUMS."

            # The Windows guide says to unblock a downloaded archive before
            # extracting it. A download made here normally carries no mark, so
            # this usually changes nothing; it only ever runs on the checked file.
            Unblock-File -LiteralPath $zip

            # Extracted outside the install folder and outside Ophio's Library,
            # as the guide asks, and kept until the install has finished.
            $downloads = Join-Path $env:LOCALAPPDATA 'Ophio-downloads'
            $unpacked = Join-Path $downloads $folder
            if (Test-Path -LiteralPath $unpacked) { Remove-Item -LiteralPath $unpacked -Recurse -Force }
            New-Item -ItemType Directory -Path $downloads -Force | Out-Null
            Expand-Archive -LiteralPath $zip -DestinationPath $downloads
            $program = Join-Path $unpacked 'ophio.exe'
            if (-not (Test-Path -LiteralPath $program)) {
                Stop-Install "$name doesn't contain ophio.exe as it should."
            }

            # Windows retains the previous outcome even after a new install is
            # queued. Read it with the checked download, even if no launcher is
            # left, so a re-run cannot mistake that failure for this attempt's.
            $before = Read-InstallState $program
            $previousId = if ($before -and $before.last_update) { $before.last_update.id } else { $null }
            $startedAt = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
            Say "Installing $product $found for $env:USERNAME..."
            # Its own messages are for running it by hand (wait, then run setup),
            # which this script does for you, so they are shown only if it fails.
            $ErrorActionPreference = 'Continue'
            try {
                $said = @(& $program install --no-setup 2>&1 | ForEach-Object { "$_" })
                $code = $LASTEXITCODE
            } catch {
                $said = @("$_")
                $code = -1
            } finally {
                $ErrorActionPreference = 'Stop'
            }
            if ($code -ne 0) {
                $said | ForEach-Object { Write-Host $_ }
                if (($said -join ' ') -match 'Application Control|blocked|0x800711C7') {
                    Stop-Install "Windows blocked ophio.exe from running. This is usually Smart App Control or a policy set by whoever manages this PC. The Windows guide explains your choices: $warnings"
                }
                Stop-Install "ophio.exe install stopped with the error above. The Windows guide has more help: $guide"
            }

            if (-not $first) {
                Say ''
                Say "The update to $product $found is queued. $product installs it once nothing has run for a minute, and restarts on it."
                Say "To see how it is going, run: & '$installed' update"
                Say "You can delete $unpacked once that says $product $found is installed."
                return
            }

            # A first install finishes in the background; wait for it.
            Say 'Finishing the install...'
            $deadline = (Get-Date).AddMinutes(5)
            $done = $false
            while ((Get-Date) -lt $deadline) {
                Start-Sleep -Seconds 2
                # Windows PowerShell 5.1 turns a native program's error output
                # into a terminating error when errors stop the script.
                $ErrorActionPreference = 'Continue'
                $text = (& $program update --json 2>$null) -join "`n"
                $ErrorActionPreference = 'Stop'
                if ($LASTEXITCODE -ne 0 -or -not $text) { continue }
                $state = $text | ConvertFrom-Json
                $now = $state.installed
                if ($now -and $now.version -eq $found -and $now.installed_at -gt 0) { $done = $true; break }
                $last = $state.last_update
                if ($last -and $last.id -ne $previousId -and $last.at -ge $startedAt -and
                    $last.release.version -eq $found -and $last.result -ne 'installed') {
                    Stop-Install "the install of $product $found ended without installing it: $($last.message)"
                }
            }
            if (-not $done) {
                Say "The install is taking longer than expected. Check it with: & '$program' update"
                Say "When that says Installed, run: & '$installed' setup"
                return
            }
            Say "Installed $product $found in $(Split-Path -Parent $installed)."
            Remove-Item -LiteralPath $unpacked -Recurse -Force -ErrorAction SilentlyContinue

            $later = "To run setup again later, open PowerShell and run: & '$installed' setup"
            if ("$env:OPHIO_NO_SETUP" -eq '1') { Say $later; return }
            Say "Starting setup now. It starts $product, connects your phone and checks what works."
            Say ''
            # Setup's own hints name .\ophio.exe, which is right in its folder.
            Push-Location (Split-Path -Parent $installed)
            try { & $installed setup } finally { Pop-Location }
            Say ''
            Say $later
        } finally {
            Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
        }
    }

    try {
        Get-Ophio
    } catch [System.OperationCanceledException] {
        # Already explained.
    }
}
