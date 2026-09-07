param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string] $InstallDir
)
$ErrorActionPreference = 'Stop'
try {
    $directory = [IO.Path]::GetFullPath($InstallDir)
    $targets = @(
        [IO.Path]::Combine($directory, 'node.exe'),
        [IO.Path]::Combine($directory, 'tunnel-client.exe')
    )
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        # Match full executable paths, never common process names alone.
        # The installer's standard prompt has already stopped the desktop.
        $running = @(Get-Process -Name 'node', 'tunnel-client' -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -and $targets -contains $_.Path })
        foreach ($process in $running) {
            try {
                # Keep a handle while terminating and waiting to avoid PID reuse.
                $null = $process.Handle
                if (-not $process.HasExited) { $process.Kill() }
                if (-not $process.WaitForExit(3000)) {
                    throw "Process did not exit: $($process.Id)"
                }
            } catch {
                if (-not $process.HasExited) { throw }
            } finally {
                $process.Dispose()
            }
        }
        # Catch inaccessible processes and delayed image-handle release as well.
        $locked = @()
        foreach ($target in $targets) {
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
        if ($locked.Count -eq 0) {
            Write-Output 'ChatX runtime files are no longer in use.'
            exit 0
        }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Runtime files remain in use: $($locked -join ', ')"
} catch {
    Write-Output $_.Exception.Message
    exit 1
}
