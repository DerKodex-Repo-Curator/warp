[CmdletBinding()]
param(
    [ValidateSet('enabled', 'disabled')]
    [string]$Policy = 'enabled',
    [string]$ManifestPath = 'crates/warp_tui/Cargo.toml',
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $env:RUNNER_TEMP "warp-windows-ci-$($env:GITHUB_RUN_ID)-$($env:GITHUB_RUN_ATTEMPT)"
}

$beforeHash = (Get-FileHash -LiteralPath $ManifestPath -Algorithm SHA256).Hash
$text = Get-Content -LiteralPath $ManifestPath -Raw
$newline = if ($text.Contains("`r`n")) { "`r`n" } else { "`n" }
$targets = @(
    @{ name = 'warp-tui-oss'; source = 'oss' },
    @{ name = 'warp-tui'; source = 'local' },
    @{ name = 'warp-tui-dev'; source = 'dev' },
    @{ name = 'warp-tui-preview'; source = 'preview' },
    @{ name = 'warp-tui-stable'; source = 'stable' }
)

# Apply the manifest change only after rust-cache has restored the same inputs for both variants.
foreach ($target in $targets) {
    $pattern = '(?m)^name = "{0}"\r?\npath = "src/bin/{1}\.rs"(?=\r?$)(?!\r?\ntest\s*=)'
    $pattern = $pattern -f [regex]::Escape($target.name), $target.source
    if ([regex]::Matches($text, $pattern).Count -ne 1) {
        throw "Expected exactly one unmodified target entry for $($target.name)."
    }
    if ($Policy -eq 'disabled') {
        $text = [regex]::Replace($text, $pattern, {
                param($match)
                $match.Value + $newline + 'test = false'
            })
    }
}

if ($Policy -eq 'disabled') {
    [IO.File]::WriteAllText((Resolve-Path -LiteralPath $ManifestPath).Path, $text, [Text.UTF8Encoding]::new($false))
}
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
@{
    policy = $Policy
    target_names = @($targets.name)
    before_manifest_sha256 = $beforeHash
    after_manifest_sha256 = (Get-FileHash -LiteralPath $ManifestPath -Algorithm SHA256).Hash
} | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'tui-harness-policy.json') -Encoding utf8
