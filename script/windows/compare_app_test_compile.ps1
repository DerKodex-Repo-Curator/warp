[CmdletBinding()]
param(
    [ValidatePattern('^[0-9a-f]{40}$')]
    [string]$Baseline = 'c3e20949919991fa9343b22135a848ce74081e0a'
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows') {
    throw 'Compile comparison is restricted to the Windows CI workspace.'
}
$root = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
if ($root -ne (Resolve-Path $env:GITHUB_WORKSPACE).Path -or (Get-Location).Path -ne $root) {
    throw 'Compile comparison requires the checked-out CI workspace.'
}
foreach ($name in @('CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'RUSTC_BOOTSTRAP', 'CARGO_ENCODED_RUSTFLAGS')) {
    if (-not [string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($name))) {
        throw "Unexpected $name configuration."
    }
}
if ($env:CARGO_INCREMENTAL -ne '0' -or $env:RUSTFLAGS -ne '-C debuginfo=line-tables-only --cfg=web_sys_unstable_apis') {
    throw 'Compile comparison requires the normal non-incremental CI flags.'
}
$sources = @('crates/warpui_core/src/core/app.rs', 'crates/warpui_core/src/core/mod_tests.rs')
$scope = @('app', 'crates', 'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo')
$candidate = & git rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $candidate -notmatch '^[0-9a-f]{40}$') {
    throw 'Could not identify the candidate commit.'
}
& git cat-file -e "${Baseline}^{commit}" 2>$null
if ($LASTEXITCODE -ne 0) {
    & git fetch --no-tags --depth=1 origin $Baseline
    if ($LASTEXITCODE -ne 0) {
        throw 'Could not fetch the comparison baseline.'
    }
}
$changed = @(& git diff --name-only $Baseline HEAD -- @scope)
if ($LASTEXITCODE -ne 0 -or @(Compare-Object $sources $changed).Count -ne 0) {
    throw 'Comparison source changes must be limited to the two test-harness files.'
}
$target = Join-Path $root 'target'
$snapshot = Join-Path $env:RUNNER_TEMP "warp-app-test-cache-$($env:GITHUB_RUN_ID)-$($env:GITHUB_RUN_ATTEMPT)"
$measurements = Join-Path $env:RUNNER_TEMP "warp-windows-ci-$($env:GITHUB_RUN_ID)-$($env:GITHUB_RUN_ATTEMPT)"
if (-not (Test-Path -LiteralPath $target -PathType Container) -or (Test-Path -LiteralPath $snapshot) -or (Test-Path -LiteralPath $measurements)) {
    throw 'Comparison requires a restored target and unused diagnostic directories.'
}
if ((Get-Item -LiteralPath $target).Attributes -band [IO.FileAttributes]::ReparsePoint) {
    throw 'Comparison cannot replace a redirected target directory.'
}
$samples = @(
    @{ variant = 'candidate'; source = $candidate; phase = 'compile-candidate-1' },
    @{ variant = 'baseline'; source = $Baseline; phase = 'compile-baseline-1' },
    @{ variant = 'baseline'; source = $Baseline; phase = 'compile-baseline-2' },
    @{ variant = 'candidate'; source = $candidate; phase = 'compile' }
)
Copy-Item -LiteralPath $target -Destination $snapshot -Recurse -Force
try {
    foreach ($sample in $samples) {
        & git restore --source $sample.source --worktree -- @sources
        if ($LASTEXITCODE -ne 0) {
            throw 'Could not prepare comparison sources.'
        }
        & git diff --quiet $sample.source -- @scope
        if ($LASTEXITCODE -ne 0) {
            throw 'Prepared source tree does not match the requested comparison.'
        }
        # Only this ephemeral CI target is replaced; both cases start from the same restored cache.
        Remove-Item -LiteralPath $target -Recurse -Force
        Copy-Item -LiteralPath $snapshot -Destination $target -Recurse -Force
        $output = Join-Path $measurements $sample.phase
        Write-Output "Compile comparison: $($sample.variant), $($sample.phase)"
        & (Join-Path $PSScriptRoot 'measure_ci_performance.ps1') -Mode Run -Phase compile -OutputDirectory $output
        if ($LASTEXITCODE -ne 0) {
            throw 'Comparison compilation failed.'
        }
        $metadataPath = Join-Path $output 'metadata.json'
        $metadata = Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json
        $metadata | Add-Member -NotePropertyName comparison -NotePropertyValue $sample
        $metadata | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $metadataPath -Encoding utf8
    }
} finally {
    & git diff --quiet -- @sources
    if ($LASTEXITCODE -ne 0) {
        & git restore --source $candidate --worktree -- @sources
        if ($LASTEXITCODE -ne 0) {
            throw 'Could not restore candidate sources after comparison.'
        }
    }
    Remove-Item -LiteralPath $snapshot -Recurse -Force
}
