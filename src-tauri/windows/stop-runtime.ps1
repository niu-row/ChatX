param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string] $InstallDir,

    [ValidateNotNullOrEmpty()]
    [string] $MainBinaryName = 'chatx-desktop.exe',

    [ValidateRange(1, 120)]
    [int] $TimeoutSeconds = 20,

    [switch] $SkipDesktop
)
$ErrorActionPreference = 'Stop'

function Get-OwnedRuntimeProcesses {
    param([string[]] $ExecutablePaths)

    $names = @($ExecutablePaths | ForEach-Object { [IO.Path]::GetFileName($_) } | Select-Object -Unique)
    $filter = ($names | ForEach-Object { "Name = '$($_.Replace("'", "''"))'" }) -join ' OR '
    $owned = @()
    foreach ($candidate in @(Get-CimInstance -ClassName Win32_Process -Filter $filter -ErrorAction Stop)) {
        $processPath = [string] $candidate.ExecutablePath
        if (-not $processPath) { continue }

        $matched = $false
        foreach ($target in $ExecutablePaths) {
            if ([string]::Equals($processPath, $target, [StringComparison]::OrdinalIgnoreCase)) {
                $matched = $true
                break
            }
        }
        if (-not $matched) { continue }

        try {
            $process = Get-Process -Id ([int] $candidate.ProcessId) -ErrorAction Stop
            $process | Add-Member -NotePropertyName ChatXPath -NotePropertyValue $processPath -Force
            $owned += $process
        } catch {
            continue
        }
    }
    return $owned
}

try {
    $directory = [IO.Path]::GetFullPath($InstallDir)
    $desktopTarget = [IO.Path]::Combine($directory, $MainBinaryName)
    $processTargets = @(
        [IO.Path]::Combine($directory, 'node.exe'),
        [IO.Path]::Combine($directory, 'tunnel-client.exe')
    )
    $filesToUnlock = @(
        [IO.Path]::Combine($directory, 'node.exe'),
        [IO.Path]::Combine($directory, 'tunnel-client.exe'),
        [IO.Path]::Combine($directory, 'desktop-commander-launcher.mjs'),
        [IO.Path]::Combine($directory, 'runtime-manifest.json'),
        [IO.Path]::Combine($directory, 'DesktopCommander-LICENSE.txt')
    )
    if (-not $SkipDesktop) {
        $processTargets = @($desktopTarget) + $processTargets
        $filesToUnlock = @($desktopTarget) + $filesToUnlock
    }

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $cleanPasses = 0
    $locked = @()
    do {
        $running = @(Get-OwnedRuntimeProcesses -ExecutablePaths $processTargets | Sort-Object @{ Expression = {
            if ([string]::Equals($_.ChatXPath, $desktopTarget, [StringComparison]::OrdinalIgnoreCase)) { 0 } else { 1 }
        }})
        foreach ($process in $running) {
            try {
                $null = $process.Handle
                if (-not $process.HasExited) { $process.Kill() }
                if (-not $process.WaitForExit(5000)) {
                    throw "Process did not exit: $($process.Id) ($($process.ChatXPath))"
                }
            } catch {
                if (-not $process.HasExited) { throw }
            } finally {
                $process.Dispose()
            }
        }

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
    $processSummary = @($remaining | ForEach-Object { "$($_.Id):$($_.ChatXPath)" })
    foreach ($process in $remaining) { $process.Dispose() }
    $details = @()
    if ($locked.Count -gt 0) { $details += "locked files: $($locked -join ', ')" }
    if ($processSummary.Count -gt 0) { $details += "running processes: $($processSummary -join ', ')" }
    throw "ChatX runtime remains in use after cleanup ($($details -join '; '))."
} catch {
    Write-Output $_.Exception.Message
    exit 1
}
