#requires -Version 7.2
[CmdletBinding()]
param(
    [string[]] $Target,
    [string] $Output,
    [string] $Toolchain,
    [switch] $Native,
    [switch] $ListTargets
)

$ErrorActionPreference = 'Stop'
$arguments = @((Join-Path $PSScriptRoot 'tools/build_release.py'))
foreach ($triple in $Target) { $arguments += @('--target', $triple) }
if ($Output) { $arguments += @('--output', $Output) }
if ($Toolchain) { $arguments += @('--toolchain', $Toolchain) }
if ($Native) { $arguments += '--native' }
if ($ListTargets) { $arguments += '--list-targets' }
& python @arguments
if ($LASTEXITCODE -ne 0) { throw "Release build failed (exit $LASTEXITCODE)" }
