<#
.SYNOPSIS
    Renders `installer/ProjectNeb.iss` for one version and checkout.

.DESCRIPTION
    Mirrors the three substitutions `neb-cli release` performs in
    rust/crates/neb-cli/src/commands/release/stage.rs:

      * `#define AppVersion`  -> the release version
      * `#define Root`        -> the absolute path of this checkout
      * `OutputBaseFilename`  -> ProjectNeb-<version>-<channel>-setup

    The committed template is never modified. Inno Setup writes the installer
    into `<Root>\dist`.

.PARAMETER RepoRoot
    Repository checkout that contains `installer/ProjectNeb.iss`.

.PARAMETER Version
    Version without the `v` prefix, for example `0.5.0`.

.PARAMETER Channel
    Release channel used in the installer file name.

.PARAMETER Output
    Path of the rendered .iss to write. Parent directories are created.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$RepoRoot,

    [Parameter(Mandatory = $true)]
    [string]$Version,

    [string]$Channel = 'beta',

    [Parameter(Mandatory = $true)]
    [string]$Output
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = (Resolve-Path -LiteralPath $RepoRoot).Path
$templatePath = Join-Path $root 'installer\ProjectNeb.iss'

if ($Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+$') {
    throw "version '$Version' must be MAJOR.MINOR.PATCH"
}

if ($Channel -notmatch '^[A-Za-z0-9_-]+$') {
    throw "channel '$Channel' must be letters, digits, '-' or '_'"
}

if (-not (Test-Path -LiteralPath $templatePath -PathType Leaf)) {
    throw "installer template is missing: '$templatePath'"
}

if ($root.Contains('"')) {
    throw "repository path '$root' contains a quote, which Inno Setup cannot use"
}

$text = [IO.File]::ReadAllText($templatePath, [Text.Encoding]::UTF8)
$script = $text

$substitutions = @(
    @{
        Name = 'AppVersion'
        Pattern = '(?m)^\s*#define\s+AppVersion\s+"[^"]*"'
        Replacement = '#define AppVersion "' + $Version + '"'
    },
    @{
        Name = 'Root'
        Pattern = '(?m)^\s*#define\s+Root\s+"[^"]*"'
        Replacement = '#define Root "' + $root + '"'
    },
    @{
        Name = 'OutputBaseFilename'
        Pattern = '(?m)^\s*OutputBaseFilename=.*$'
        Replacement = 'OutputBaseFilename=ProjectNeb-' + $Version + '-' + $Channel + '-setup'
    }
)

foreach ($substitution in $substitutions) {
    $found = [regex]::Matches($script, $substitution.Pattern)

    if ($found.Count -ne 1) {
        throw ("installer/ProjectNeb.iss must define {0} exactly once (found {1})" -f $substitution.Name, $found.Count)
    }

    $replacement = $substitution.Replacement
    $evaluator = [System.Text.RegularExpressions.MatchEvaluator]{
        param($match)
        return $replacement
    }
    $script = [regex]::Replace($script, $substitution.Pattern, $evaluator)
}

$outputPath = [IO.Path]::GetFullPath($Output)
$outputDirectory = Split-Path -Parent $outputPath
if ($outputDirectory -and -not (Test-Path -LiteralPath $outputDirectory)) {
    New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
}

[IO.File]::WriteAllText($outputPath, $script, (New-Object System.Text.UTF8Encoding($false)))

Write-Host ("rendered installer script -> {0}" -f $outputPath)
Write-Host ("  AppVersion         {0}" -f $Version)
Write-Host ("  Root               {0}" -f $root)
Write-Host ("  OutputBaseFilename ProjectNeb-{0}-{1}-setup" -f $Version, $Channel)
