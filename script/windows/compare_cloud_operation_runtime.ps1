[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('Prepare', 'Measure')]
    [string]$Mode,
    [ValidatePattern('^[0-9a-f]{40}$')]
    [string]$Baseline = '210ffd656b98e7d3493fb99ac5024b88ff213b28'
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
& python (Join-Path $PSScriptRoot 'runtime_comparison.py') --mode $Mode --baseline $Baseline
exit $LASTEXITCODE
