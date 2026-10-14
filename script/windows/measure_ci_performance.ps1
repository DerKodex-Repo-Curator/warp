[Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSUseCompatibleCommands', '', Justification = 'This Windows CI diagnostic runs on PowerShell 7 with CIM and performance-counter cmdlets.')]
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('Start', 'Collect', 'Phase', 'Stop')]
    [string]$Mode,
    [Parameter(Mandatory)]
    [string]$OutputDirectory,
    [ValidateSet('compile', 'after-compile', 'unit-tests')]
    [string]$Phase = 'compile',
    [ValidateRange(1, 60)]
    [int]$IntervalSeconds = 5,
    [ValidateRange(1, 2400)]
    [int]$MaxDurationSeconds = 2100
)

$ErrorActionPreference = 'Stop'

function Write-DiagnosticRecord {
    param([string]$File, [object]$Record)
    $Record | ConvertTo-Json -Depth 8 -Compress | Add-Content -LiteralPath $File -Encoding utf8
}

function Write-DiagnosticFailure {
    param([string]$Stage, [object]$Failure)
    Write-DiagnosticRecord (Join-Path $OutputDirectory 'errors.jsonl') @{
        utc = [DateTime]::UtcNow.ToString('o')
        stage = $Stage
        exception_type = $Failure.Exception.GetType().FullName
    }
}

function Get-ToolVersion {
    param([string]$Tool, [string[]]$Arguments)
    try {
        $result = & $Tool @Arguments 2>$null
        if ($LASTEXITCODE -eq 0) {
            return ($result -join "`n")
        }
    } catch {
        Write-DiagnosticFailure 'tool-version' $_
    }
    return $null
}

function Get-CounterValue {
    param([object[]]$Samples, [string]$Suffix)
    $sample = $Samples | Where-Object { $_.Path.EndsWith($Suffix, [StringComparison]::OrdinalIgnoreCase) } | Select-Object -First 1
    if ($null -ne $sample -and -not [double]::IsNaN($sample.CookedValue) -and -not [double]::IsInfinity($sample.CookedValue)) {
        return $sample.CookedValue
    }
    return $null
}

try {
    $phaseFile = Join-Path $OutputDirectory 'phase.txt'
    $stopFile = Join-Path $OutputDirectory 'stop'
    $completedFile = Join-Path $OutputDirectory 'completed'

    switch ($Mode) {
        'Start' {
            if (Test-Path -LiteralPath $OutputDirectory) {
                throw 'Diagnostic directory already exists.'
            }
            New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
            Set-Content -LiteralPath $phaseFile -Value 'compile' -Encoding utf8
            $metadata = @{
                utc = [DateTime]::UtcNow.ToString('o')
                logical_processors = [Environment]::ProcessorCount
                source_sha = Get-ToolVersion 'git' @('rev-parse', 'HEAD')
                rustc = Get-ToolVersion 'rustc' @('--version')
                cargo = Get-ToolVersion 'cargo' @('--version')
                nextest = Get-ToolVersion 'cargo' @('nextest', '--version')
                sample_interval_seconds = $IntervalSeconds
                maximum_duration_seconds = $MaxDurationSeconds
                powershell = $PSVersionTable.PSVersion.ToString()
            }
            try {
                $system = Get-CimInstance Win32_ComputerSystem -OperationTimeoutSec 5
                $os = Get-CimInstance Win32_OperatingSystem -OperationTimeoutSec 5
                $metadata.physical_memory_bytes = $system.TotalPhysicalMemory
                $metadata.os = $os.Caption
                $metadata.os_version = $os.Version
                $metadata.processors = @(Get-CimInstance Win32_Processor -OperationTimeoutSec 5 |
                        Select-Object Name, NumberOfCores, NumberOfLogicalProcessors, MaxClockSpeed)
            } catch {
                Write-DiagnosticFailure 'hardware' $_
            }
            $metadata | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'metadata.json') -Encoding utf8
            Write-DiagnosticRecord (Join-Path $OutputDirectory 'phases.jsonl') @{ utc = [DateTime]::UtcNow.ToString('o'); phase = 'compile' }

            # Encoding the invocation preserves paths with spaces without collecting any environment.
            $scriptPath = $PSCommandPath.Replace("'", "''")
            $directory = $OutputDirectory.Replace("'", "''")
            $invocation = "& '$scriptPath' -Mode Collect -OutputDirectory '$directory' -IntervalSeconds $IntervalSeconds -MaxDurationSeconds $MaxDurationSeconds"
            $encodedInvocation = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
            $executable = (Get-Process -Id $PID).Path
            $collector = Start-Process -FilePath $executable -ArgumentList @('-NoLogo', '-NoProfile', '-EncodedCommand', $encodedInvocation) -WorkingDirectory (Get-Location).Path -PassThru -RedirectStandardOutput (Join-Path $OutputDirectory 'collector.stdout') -RedirectStandardError (Join-Path $OutputDirectory 'collector.stderr')
            @{ pid = $collector.Id; started_utc = $collector.StartTime.ToUniversalTime().ToString('o') } |
                ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputDirectory 'collector.json') -Encoding utf8
        }
        'Collect' {
            $clock = [Diagnostics.Stopwatch]::StartNew()
            $lastPhase = 'compile'
            $counterPaths = @(
                '\Processor(_Total)\% Processor Time',
                '\Memory\Available Bytes',
                '\Memory\Committed Bytes',
                '\Memory\Commit Limit',
                '\Memory\Pages Input/sec',
                '\PhysicalDisk(_Total)\Disk Read Bytes/sec',
                '\PhysicalDisk(_Total)\Disk Write Bytes/sec',
                '\PhysicalDisk(_Total)\Avg. Disk Queue Length'
            )
            try {
                while ($clock.Elapsed.TotalSeconds -lt $MaxDurationSeconds -and -not (Test-Path -LiteralPath $stopFile)) {
                    $sampleStart = $clock.Elapsed.TotalSeconds
                    $row = @{
                        utc = [DateTime]::UtcNow.ToString('o')
                        elapsed_seconds = $sampleStart
                    }
                    try {
                        $currentPhase = Get-Content -LiteralPath $phaseFile -Raw
                        if (-not [string]::IsNullOrWhiteSpace($currentPhase)) {
                            $lastPhase = $currentPhase.Trim()
                        }
                    } catch {
                        Write-DiagnosticFailure 'phase-marker' $_
                    }
                    $row.phase = $lastPhase
                    try {
                        $counters = @(Get-Counter -Counter $counterPaths -MaxSamples 1 -ErrorAction Stop).CounterSamples
                        $row.cpu_percent = Get-CounterValue $counters '\processor(_total)\% processor time'
                        $row.available_memory_bytes = Get-CounterValue $counters '\memory\available bytes'
                        $row.committed_memory_bytes = Get-CounterValue $counters '\memory\committed bytes'
                        $row.commit_limit_bytes = Get-CounterValue $counters '\memory\commit limit'
                        $row.pages_input_per_second = Get-CounterValue $counters '\memory\pages input/sec'
                        $row.disk_read_bytes_per_second = Get-CounterValue $counters '\physicaldisk(_total)\disk read bytes/sec'
                        $row.disk_write_bytes_per_second = Get-CounterValue $counters '\physicaldisk(_total)\disk write bytes/sec'
                        $row.disk_queue_length = Get-CounterValue $counters '\physicaldisk(_total)\avg. disk queue length'
                    } catch {
                        Write-DiagnosticFailure 'counters' $_
                    }
                    $row.processes = @(Get-Process -Name cargo, rustc, link, pwsh, nsc, git -ErrorAction SilentlyContinue | ForEach-Object {
                            try {
                                @{
                                    pid = $_.Id
                                    name = $_.ProcessName
                                    started_utc = $_.StartTime.ToUniversalTime().ToString('o')
                                    cpu_seconds = $_.CPU
                                    working_set_bytes = $_.WorkingSet64
                                    private_memory_bytes = $_.PrivateMemorySize64
                                }
                            } catch {
                                # A process can exit between enumeration and reading its counters.
                                $null = $_
                            }
                        })
                    try {
                        $row.disks = @(Get-CimInstance Win32_LogicalDisk -Filter 'DriveType=3' -OperationTimeoutSec 5 |
                                Select-Object DeviceID, Size, FreeSpace)
                    } catch {
                        Write-DiagnosticFailure 'disk-space' $_
                    }
                    Write-DiagnosticRecord (Join-Path $OutputDirectory 'samples.jsonl') $row
                    $remaining = $IntervalSeconds - ($clock.Elapsed.TotalSeconds - $sampleStart)
                    if ($remaining -gt 0) {
                        Start-Sleep -Milliseconds ([int]($remaining * 1000))
                    }
                }
            } finally {
                Set-Content -LiteralPath $completedFile -Value ([DateTime]::UtcNow.ToString('o')) -Encoding utf8
            }
        }
        'Phase' {
            if (Test-Path -LiteralPath $phaseFile) {
                Set-Content -LiteralPath $phaseFile -Value $Phase -Encoding utf8
                Write-DiagnosticRecord (Join-Path $OutputDirectory 'phases.jsonl') @{ utc = [DateTime]::UtcNow.ToString('o'); phase = $Phase }
                if ($Phase -eq 'after-compile' -and (Test-Path -LiteralPath 'target/cargo-timings/cargo-timing.html')) {
                    Copy-Item -LiteralPath 'target/cargo-timings/cargo-timing.html' -Destination (Join-Path $OutputDirectory 'cargo-timing.html')
                }
            }
        }
        'Stop' {
            if (Test-Path -LiteralPath $phaseFile) {
                Write-DiagnosticRecord (Join-Path $OutputDirectory 'phases.jsonl') @{ utc = [DateTime]::UtcNow.ToString('o'); phase = 'finished' }
                Set-Content -LiteralPath $stopFile -Value 'stop' -Encoding utf8
                $deadline = [DateTime]::UtcNow.AddSeconds(10)
                while (-not (Test-Path -LiteralPath $completedFile) -and [DateTime]::UtcNow -lt $deadline) {
                    Start-Sleep -Milliseconds 100
                }
            }
        }
    }
} catch {
    Write-Warning "Windows CI diagnostics unavailable ($($_.Exception.GetType().FullName))."
}

# Diagnostic errors must never replace the build or test result.
exit 0
