[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('Run', 'Publish')]
    [string]$Mode,
    [ValidateSet('off', 'warp-libtest', 'warp-libtest-ir')]
    [string]$CompilerProfile = 'off',
    [string]$OutputDirectory,
    [ValidateRange(1, 2147483648)]
    [long]$MaxTraceBytes = 2147483648
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$collector = Join-Path $PSScriptRoot 'measure_ci_performance.ps1'
if ($Mode -eq 'Run' -and $CompilerProfile -eq 'off') {
    & $collector -Mode Run -Phase compile
    exit $LASTEXITCODE
}
if ($CompilerProfile -eq 'off') {
    throw 'Publishing requires an enabled compiler profile.'
}
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $prefix = if ($CompilerProfile -eq 'warp-libtest-ir') { 'warp-libtest-ir' } else { 'warp-libtest-profile' }
    $OutputDirectory = Join-Path $env:RUNNER_TEMP "$prefix-$($env:GITHUB_RUN_ID)-$($env:GITHUB_RUN_ATTEMPT)"
}
$capture = Join-Path $OutputDirectory 'capture'
$work = Join-Path $OutputDirectory 'work'

if ($Mode -eq 'Run') {
    foreach ($name in @('RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'RUSTC_BOOTSTRAP', 'WARP_CI_RUSTC_PROFILE_DIR', 'WARP_CI_RUSTC_SOURCE', 'WARP_CI_DIAGNOSTIC_MODE')) {
        if (-not [string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($name))) {
            throw "Unexpected existing $name configuration."
        }
    }
    if (Test-Path -LiteralPath $OutputDirectory) {
        throw 'Compiler profile directory already exists.'
    }
    $version = & rustc --version
    if ($LASTEXITCODE -ne 0 -or $version -notmatch '^rustc 1\.92\.0 ') {
        throw 'Compiler profiling requires the pinned Rust 1.92.0 compiler.'
    }
    New-Item -ItemType Directory -Path $work -Force | Out-Null
    @{
        profile = $CompilerProfile
        rustc = $version
        events = $(if ($CompilerProfile -eq 'warp-libtest-ir') { 'none' } else { 'default' })
        counter = $(if ($CompilerProfile -eq 'warp-libtest-ir') { 'ir-size' } else { 'wall-time' })
        bootstrap_scope = $(if ($CompilerProfile -eq 'warp-libtest-ir') { 'none' } else { 'selected-compiler-child' })
    } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputDirectory 'setup.json') -Encoding utf8
    $wrapper = Join-Path $work 'rustc-profile-wrapper.exe'
    & rustc --edition 2024 --crate-name warp_ci_rustc_profile_wrapper (Join-Path $PSScriptRoot 'rustc_profile_wrapper.rs') -o $wrapper
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    if ($CompilerProfile -eq 'warp-libtest-ir') {
        $tool = Join-Path $work 'normalizer'
        New-Item -ItemType Directory -Path (Join-Path $tool 'src') -Force | Out-Null
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'ir_name_normalizer.rs') -Destination (Join-Path $tool 'src/main.rs')
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'ir_name_normalizer_tests.rs') -Destination (Join-Path $tool 'src/ir_name_normalizer_tests.rs')
        @'
[package]
name = "warp_ir_name_normalizer"
version = "0.1.0"
edition = "2024"
[dependencies]
rustc-demangle = "=0.1.27"
serde_json = "=1.0.150"
[workspace]
'@ | Set-Content -LiteralPath (Join-Path $tool 'Cargo.toml') -Encoding utf8
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot '../../Cargo.lock') -Destination (Join-Path $tool 'Cargo.lock')
        & cargo build --release --offline --manifest-path (Join-Path $tool 'Cargo.toml') --target-dir (Join-Path $work 'normalizer-target')
        if ($LASTEXITCODE -ne 0) {
            exit $LASTEXITCODE
        }
    }
    $previousWrapper = $env:RUSTC_WRAPPER
    $previousDirectory = $env:WARP_CI_RUSTC_PROFILE_DIR
    $previousSource = $env:WARP_CI_RUSTC_SOURCE
    $previousMode = $env:WARP_CI_DIAGNOSTIC_MODE
    $expectedSource = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../../app/src/lib.rs')).Path
    $code = 1
    try {
        $env:RUSTC_WRAPPER = $wrapper
        $env:WARP_CI_RUSTC_PROFILE_DIR = $capture
        $env:WARP_CI_RUSTC_SOURCE = $expectedSource
        $env:WARP_CI_DIAGNOSTIC_MODE = if ($CompilerProfile -eq 'warp-libtest-ir') { 'ir-inventory' } else { 'profile' }
        & $collector -Mode Run -Phase compile
        $code = $LASTEXITCODE
    } finally {
        $env:RUSTC_WRAPPER = $previousWrapper
        $env:WARP_CI_RUSTC_PROFILE_DIR = $previousDirectory
        $env:WARP_CI_RUSTC_SOURCE = $previousSource
        $env:WARP_CI_DIAGNOSTIC_MODE = $previousMode
    }
    exit $code
}

if ($CompilerProfile -eq 'warp-libtest-ir') {
    $binary = if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) { 'warp_ir_name_normalizer.exe' } else { 'warp_ir_name_normalizer' }
    $normalizer = Join-Path $work "normalizer-target/release/$binary"
    $python = if (Get-Command python -ErrorAction SilentlyContinue) { 'python' } else { 'python3' }
    & $python (Join-Path $PSScriptRoot 'ir_inventory.py') $capture (Join-Path $OutputDirectory 'artifact') $normalizer
    exit $LASTEXITCODE
}
$publish = Join-Path $OutputDirectory 'artifact'
if (Test-Path -LiteralPath $publish) {
    throw 'Compiler profile publication directory already exists.'
}
New-Item -ItemType Directory -Path $publish -Force | Out-Null
$issues = @()
$invocation = $null
$passes = @()
foreach ($file in @(
        @{ path = (Join-Path $OutputDirectory 'setup.json'); name = 'setup.json' },
        @{ path = (Join-Path $capture 'invocation.json'); name = 'invocation.json' }
    )) {
    if (Test-Path -LiteralPath $file.path) {
        try {
            $record = Get-Content -LiteralPath $file.path -Raw | ConvertFrom-Json
            if ($file.name -eq 'invocation.json') {
                $invocation = $record
            }
            $record | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $publish $file.name) -Encoding utf8
        } catch {
            $issues += 'invalid-metadata'
        }
    } else {
        $issues += 'missing-metadata'
    }
}
if ($null -eq $invocation -or $invocation.crate -ne 'warp' -or $invocation.source -ne 'app/src/lib.rs' -or -not $invocation.libtest -or
    $invocation.bootstrap_scope -ne 'selected-compiler-child' -or $invocation.events -ne 'default' -or
    $invocation.counter -ne 'wall-time') {
    $issues += 'unexpected-target-metadata'
}
if ($null -ne $invocation -and $invocation.compiler_exit_code -ne 0) {
    $issues += 'compiler-failed'
}
if ($null -eq $invocation -or -not $invocation.timing_capture_succeeded) {
    $issues += 'timing-capture-failed'
}
if (-not (Test-Path -LiteralPath (Join-Path $capture 'warp-libtest.started')) -or
    (Test-Path -LiteralPath (Join-Path $capture 'duplicate-match'))) {
    $issues += 'target-match-marker-invalid'
}
try {
    foreach ($line in Get-Content -LiteralPath (Join-Path $capture 'time-passes.jsonl')) {
        $record = $line | ConvertFrom-Json
        $fields = @($record.PSObject.Properties.Name)
        $unexpectedFields = @(Compare-Object -ReferenceObject @('pass', 'time', 'rss_start', 'rss_end') -DifferenceObject $fields -CaseSensitive)
        if ($unexpectedFields.Count -ne 0 -or $record.pass -isnot [string] -or $record.pass.Length -gt 1024 -or
            [double]::IsNaN([double]$record.time) -or [double]::IsInfinity([double]$record.time) -or
            [double]$record.time -lt 0) {
            throw 'Invalid time-passes record.'
        }
        foreach ($rss in @($record.rss_start, $record.rss_end)) {
            if ($null -ne $rss -and [string]$rss -notmatch '^\d+$') {
                throw 'Invalid memory counter.'
            }
        }
        $passes += @{
            pass = $record.pass
            time = [double]$record.time
            rss_start = $record.rss_start
            rss_end = $record.rss_end
        }
    }
    if (@($passes | Where-Object { $_.pass -eq 'total' }).Count -ne 1) {
        throw 'Missing compiler total timing.'
    }
    $passes | ForEach-Object { $_ | ConvertTo-Json -Compress } |
        Set-Content -LiteralPath (Join-Path $publish 'time-passes.jsonl') -Encoding utf8
} catch {
    $issues += 'invalid-time-passes'
}
$traces = @(Get-ChildItem -LiteralPath (Join-Path $capture 'raw') -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Extension -in @('.mm_profdata', '.rustc_profile') })
$rawBytes = [long](($traces | Measure-Object -Property Length -Sum).Sum)
if ($traces.Count -ne 1 -or $rawBytes -eq 0) {
    $issues += 'missing-or-multiple-traces'
} elseif ($null -eq $invocation -or $traces[0].BaseName -ne ('warp-{0:D7}' -f [int]$invocation.pid)) {
    $issues += 'trace-pid-mismatch'
}
$included = $false
if ($rawBytes -gt $MaxTraceBytes) {
    $issues += 'trace-limit-exceeded'
} elseif ($traces.Count -eq 1) {
    Copy-Item -LiteralPath $traces[0].FullName -Destination $publish
    $included = $true
}
@{
    complete = $issues.Count -eq 0
    issues = @($issues)
    trace_count = $traces.Count
    raw_trace_bytes = $rawBytes
    maximum_published_trace_bytes = $MaxTraceBytes
    raw_trace_included = $included
    traces = @($traces | Select-Object Name, Length)
    time_pass_records = $passes.Count
    diagnostic_only = $true
} | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $publish 'inventory.json') -Encoding utf8
if ($issues.Count -gt 0) {
    throw 'Compiler profile is incomplete; inspect the published inventory.'
}
