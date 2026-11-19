<#
.SYNOPSIS
    Stages the portable ProjectNeb tree and zips it.

.DESCRIPTION
    Mirrors `neb-cli release` (rust/crates/neb-cli/src/commands/release/stage.rs):
    the same binaries, assets, README, generated launcher/readme and the built
    dashboard under `ui/`. Used by .github/workflows/release.yml so CI and the
    local pipeline ship the same portable archive.

.PARAMETER RepoRoot
    Repository checkout that contains `rust/target/release` and `src/Neb.UI/dist`.

.PARAMETER Version
    Version without the `v` prefix, for example `0.5.0`.

.PARAMETER Channel
    Release channel used in the archive name and the generated readme.

.PARAMETER Output
    Path of the zip to write. Parent directories are created.
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
$release = Join-Path $root 'rust\target\release'
$uiRoot = Join-Path $root 'src\Neb.UI\dist'
$templatePath = Join-Path $PSScriptRoot 'README-PORTABLE.txt'

if ($Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+$') {
    throw "version '$Version' must be MAJOR.MINOR.PATCH"
}

if ($Channel -notmatch '^[A-Za-z0-9_-]+$') {
    throw "channel '$Channel' must be letters, digits, '-' or '_'"
}

if (-not (Test-Path -LiteralPath $templatePath -PathType Leaf)) {
    throw "portable readme template is missing: '$templatePath'"
}

# Archive name -> repository-relative source. The order matches COPIED_FILES in
# stage.rs so the zip listing reads the same as `neb-cli release`.
$copied = @(
    @{ Name = 'ProjectNeb.exe';       Source = Join-Path $release 'ProjectNeb.exe' },
    @{ Name = 'neb-shell.exe';        Source = Join-Path $release 'neb-shell.exe' },
    @{ Name = 'neb-wallpaper.exe';    Source = Join-Path $release 'neb-wallpaper.exe' },
    @{ Name = 'neb-updater.exe';      Source = Join-Path $release 'neb-updater.exe' },
    @{ Name = 'neb-app.exe';          Source = Join-Path $release 'neb-app.exe' },
    @{ Name = 'WebView2Loader.dll';   Source = Join-Path $release 'WebView2Loader.dll' },
    @{ Name = 'neb-zapret.exe';       Source = Join-Path $release 'neb-zapret.exe' },
    @{ Name = 'neb-windhawk.exe';     Source = Join-Path $release 'neb-windhawk.exe' },
    @{ Name = 'neb-discord.exe';      Source = Join-Path $release 'neb-discord.exe' },
    @{ Name = 'neb-wm.exe';           Source = Join-Path $release 'neb-wm.exe' },
    @{ Name = 'neb-cli.exe';          Source = Join-Path $release 'neb-cli.exe' },
    @{ Name = 'neb.ico';              Source = Join-Path $root 'assets\neb.ico' },
    @{ Name = 'logo-mark.png';        Source = Join-Path $root 'assets\logo-mark.png' },
    @{ Name = 'README.md';            Source = Join-Path $root 'docs\beta\README-BETA.md' }
)

foreach ($entry in $copied) {
    if (-not (Test-Path -LiteralPath $entry.Source -PathType Leaf)) {
        throw "missing release input '$($entry.Source)' (run cargo build --workspace --release first)"
    }
}

$uiIndex = Join-Path $uiRoot 'index.html'
if (-not (Test-Path -LiteralPath $uiIndex -PathType Leaf)) {
    throw "the dashboard is not built at '$uiRoot' (run npm ci and npm run build in src/Neb.UI)"
}

$staging = Join-Path ([IO.Path]::GetTempPath()) ('neb-portable-' + [Guid]::NewGuid().ToString('N'))
$entries = New-Object System.Collections.ArrayList

function Write-Utf8NoBom {
    param([string]$Path, [string]$Text)

    $directory = Split-Path -Parent $Path
    if ($directory -and -not (Test-Path -LiteralPath $directory)) {
        New-Item -ItemType Directory -Path $directory -Force | Out-Null
    }

    [IO.File]::WriteAllText($Path, $Text, (New-Object System.Text.UTF8Encoding($false)))
}

try {
    New-Item -ItemType Directory -Path $staging -Force | Out-Null

    foreach ($entry in $copied) {
        $target = Join-Path $staging $entry.Name
        Copy-Item -LiteralPath $entry.Source -Destination $target -Force
        [void]$entries.Add([pscustomobject]@{ Name = $entry.Name; Source = $target })
    }

    # Same launcher as stage.rs: CRLF, no dependencies beyond cmd.exe.
    $launcher = '@echo off' + "`r`n" +
        'setlocal' + "`r`n" +
        'cd /d "%~dp0"' + "`r`n" +
        'rem Starts ProjectNeb; it brings up the main window, panel, dock and live wallpaper.' + "`r`n" +
        'start "" "ProjectNeb.exe"' + "`r`n" +
        'endlocal' + "`r`n"
    $launcherPath = Join-Path $staging 'start-portable.cmd'
    Write-Utf8NoBom -Path $launcherPath -Text $launcher
    [void]$entries.Add([pscustomobject]@{ Name = 'start-portable.cmd'; Source = $launcherPath })

    # The template keeps the UTF-8 readme out of this script; CRLF is what the
    # local pipeline writes.
    $readme = [IO.File]::ReadAllText($templatePath, [Text.Encoding]::UTF8)
    $readme = $readme.Replace('@VERSION@', $Version)
    $readme = ($readme -replace "`r?`n", "`r`n")
    $readmePath = Join-Path $staging 'README-PORTABLE.txt'
    Write-Utf8NoBom -Path $readmePath -Text $readme
    [void]$entries.Add([pscustomobject]@{ Name = 'README-PORTABLE.txt'; Source = $readmePath })

    # The dashboard travels with the portable tree so the main window works offline.
    $uiFiles = Get-ChildItem -LiteralPath $uiRoot -File -Recurse | Sort-Object FullName
    foreach ($file in $uiFiles) {
        $relative = $file.FullName.Substring($uiRoot.Length).TrimStart('\', '/').Replace('\', '/')
        [void]$entries.Add([pscustomobject]@{ Name = "ui/$relative"; Source = $file.FullName })
    }

    $outputPath = [IO.Path]::GetFullPath($Output)
    $outputDirectory = Split-Path -Parent $outputPath
    if ($outputDirectory -and -not (Test-Path -LiteralPath $outputDirectory)) {
        New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
    }

    if (Test-Path -LiteralPath $outputPath) {
        Remove-Item -LiteralPath $outputPath -Force
    }

    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem

    $stream = [IO.File]::Open($outputPath, [IO.FileMode]::Create)
    $archive = New-Object System.IO.Compression.ZipArchive($stream, [IO.Compression.ZipArchiveMode]::Create)

    try {
        foreach ($entry in $entries) {
            $zipEntry = $archive.CreateEntry($entry.Name, [IO.Compression.CompressionLevel]::Optimal)
            $destination = $zipEntry.Open()

            try {
                $source = [IO.File]::OpenRead($entry.Source)

                try {
                    $source.CopyTo($destination)
                }
                finally {
                    $source.Dispose()
                }
            }
            finally {
                $destination.Dispose()
            }
        }
    }
    finally {
        $archive.Dispose()
        $stream.Dispose()
    }

    $hash = (Get-FileHash -LiteralPath $outputPath -Algorithm SHA256).Hash
    Write-Host ("staged {0} entries -> {1}" -f $entries.Count, $outputPath)
    Write-Host ("sha256 {0}" -f $hash)
}
finally {
    if (Test-Path -LiteralPath $staging) {
        Remove-Item -LiteralPath $staging -Recurse -Force
    }
}
