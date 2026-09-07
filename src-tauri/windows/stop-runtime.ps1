param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string] $InstallDir,

    [ValidateNotNullOrEmpty()]
    [string] $MainBinaryName = 'chatx-desktop.exe',

    [ValidateRange(1, 120)]
    [int] $TimeoutSeconds = 20
)
$ErrorActionPreference = 'Stop'

function Get-OwnedRuntimeProcesses {
    param([string[]] $ExecutablePaths)

    $names = @($ExecutablePaths | ForEach-Object { [IO.Path]::GetFileNameWithoutExtension($_) } | Select-Object -Unique)
    $owned = @()
    foreach ($process in @(Get-Process -Name $names -ErrorAction SilentlyContinue)) {
        try {
            $processPath = $process.Path
        } catch {
            $process.Dispose()
            continue
        }
        if (-not $processPath) {
            $process.Dispose()
            continue
        }
        $matched = $false
        foreach ($target in $ExecutablePaths) {
            if ([string]::Equals($processPath, $target, [StringComparison]::OrdinalIgnoreCase)) {
                $matched = $true
                break
            }
        }
        if ($matched) {
            $owned += $process
        } else {
            $process.Dispose()
        }
    }
    return $owned
}

try {
    $directory = [IO.Path]::GetFullPath($InstallDir)
    $desktopTarget = [IO.Path]::Combine($directory, $MainBinaryName)
    $processTargets = @(
        $desktopTarget,
        [IO.Path]::Combine($directory, 'node.exe'),
        [IO.Path]::Combine($directory, 'tunnel-client.exe')
    )
    $filesToUnlock = @(
        $desktopTarget,
        [IO.Path]::Combine($directory, 'node.exe'),
        [IO.Path]::Combine($directory, 'tunnel-client.exe'),
        [IO.Path]::Combine($directory, 'chatgptx-backend.mjs'),
        [IO.Path]::Combine($directory, 'runtime-manifest.json')
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $cleanPasses = 0
    $locked = @()
    do {
        # Stop the desktop first so it cannot restart Node/tunnel-client while
        # the installer is trying to replace their binaries.
        $running = @(Get-OwnedRuntimeProcesses -ExecutablePaths $processTargets | Sort-Object @{ Expression = {
            if ([string]::Equals($_.Path, $desktopTarget, [StringComparison]::OrdinalIgnoreCase)) { 0 } else { 1 }
        }})
        foreach ($process in $running) {
            try {
                $null = $process.Handle
                if (-not $process.HasExited) { $process.Kill() }
                if (-not $process.WaitForExit(5000)) {
                    throw "Process did not exit: $($process.Id) ($($process.Path))"
                }
            } catch {
                if (-not $process.HasExited) { throw }
            } finally {
                $process.Dispose()
            }
        }

        # Verify every runtime file that the installer replaces, not just the
        # Node/tunnel executables. FileShare.None catches delayed image handles,
        # antivirus scans, and other transient holders before NSIS starts copy.
        $locked = @()
        foreach ($target in $filesToUnlock) {
            if (Test-Path -LiteralPath $target) {
                try {
                    $stream = [IO.File]::Open($target, [IO.FileMode]::Open,
                        [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
                    $stream.Dispose()
                } catch {
                    $locked += $target
                }
            }
        }

        $remaining = @(Get-OwnedRuntimeProcesses -ExecutablePaths $processTargets)
        foreach ($process in $remaining) { $process.Dispose() }
        if ($locked.Count -eq 0 -and $remaining.Count -eq 0) {
            $cleanPasses += 1
            if ($cleanPasses -ge 2) {
                Write-Output 'ChatX desktop and runtime files are no longer in use.'
                exit 0
            }
        } else {
            $cleanPasses = 0
        }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)

    $remaining = @(Get-OwnedRuntimeProcesses -ExecutablePaths $processTargets)
    $processSummary = @($remaining | ForEach-Object { "$($_.Id):$($_.Path)" })
    foreach ($process in $remaining) { $process.Dispose() }
    $details = @()
    if ($locked.Count -gt 0) { $details += "locked files: $($locked -join ', ')" }
    if ($processSummary.Count -gt 0) { $details += "running processes: $($processSummary -join ', ')" }
    throw "ChatX runtime remains in use after cleanup ($($details -join '; '))."
} catch {
    Write-Output $_.Exception.Message
    exit 1
}
