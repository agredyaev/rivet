param(
    [string[]]$Root = @(),
    [string[]]$AllowCommand = @(),
    [string[]]$AllowSubcommand = @(),
    [string[]]$AllowAnyArgs = @(),
    [switch]$Help
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest

$repo = 'agredyaev/rivet'
$installRoot = if ($env:RIVET_INSTALL_ROOT) { $env:RIVET_INSTALL_ROOT } else { Join-Path $env:LOCALAPPDATA 'Rivet' }
if ($Help) {
    Write-Host "Usage: install.ps1 [-Help] [-Root PATH] [-AllowCommand NAME=EXECUTABLE] [-AllowSubcommand NAME=VALUE] [-AllowAnyArgs NAME]"
    Write-Host "Downloads the latest verified Rivet release, installs it under $installRoot, and starts one tunnel session."
    exit 0
}
if (-not $env:LOCALAPPDATA) { throw 'LOCALAPPDATA is not set.' }

Write-Host ''
Write-Host 'Rivet setup' -ForegroundColor Cyan
$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
if ($architecture -ne 'X64') { throw "Rivet releases currently support Windows x64 only; detected $architecture." }
Write-Host '  Detected Windows x64' -ForegroundColor Yellow
$headers = @{ 'User-Agent' = 'Rivet Installer' }
$manifestUri = "https://raw.githubusercontent.com/$repo/main/.release-please-manifest.json"
$manifest = Invoke-RestMethod -Uri $manifestUri -Headers $headers
$versionProperty = $manifest.PSObject.Properties | Where-Object { $_.Name -eq '.' } | Select-Object -First 1
$version = if ($versionProperty) { [string]$versionProperty.Value } else { '' }
if ($version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+([.-][A-Za-z0-9.-]+)?$') {
    throw 'Could not determine the release version from .release-please-manifest.json.'
}
$tag = "v$version"
$asset = 'rivet-windows-x64.zip'
$releaseBase = "https://github.com/$repo/releases/download/$tag"
try {
    Invoke-WebRequest -Uri "$releaseBase/$asset" -Method Head -Headers $headers | Out-Null
}
catch {
    throw "Release $tag is not published or is missing $asset. Refusing to install an older release."
}
$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("rivet-install-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tempDir | Out-Null
try {
    $zip = Join-Path $tempDir $asset
    $checksums = Join-Path $tempDir 'SHA256SUMS'
    Invoke-WebRequest -Uri "$releaseBase/$asset" -OutFile $zip -Headers $headers
    Invoke-WebRequest -Uri "$releaseBase/SHA256SUMS" -OutFile $checksums -Headers $headers
    $checksumLine = Get-Content -LiteralPath $checksums | Where-Object { $_ -match "^([0-9a-fA-F]{64})\s+\*?$([regex]::Escape($asset))$" } | Select-Object -First 1
    if (-not $checksumLine) { throw "Release checksum is missing for $asset." }
    $expected = [regex]::Match($checksumLine, '^([0-9a-fA-F]{64})').Groups[1].Value
    $actual = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash
    if ($actual -ine $expected) { throw 'Release archive checksum does not match.' }
    Write-Host '  ✓ Release archive checksum verified' -ForegroundColor Green

    $versionDir = Join-Path $installRoot ($tag + '-' + $actual.Substring(0, 12))
    New-Item -ItemType Directory -Force -Path $versionDir | Out-Null
    Expand-Archive -LiteralPath $zip -DestinationPath $versionDir -Force
    $rivet = Join-Path $versionDir 'bin\rivet.exe'
    if (-not (Test-Path -LiteralPath $rivet -PathType Leaf) -or
        -not (Test-Path -LiteralPath (Join-Path $versionDir 'rivet.toml') -PathType Leaf)) {
        throw 'Release archive is missing the Rivet binary or configuration.'
    }
    Write-Host "  ✓ Installed in $versionDir" -ForegroundColor Green
    Write-Host '  Starting Rivet with the scope for this session...' -ForegroundColor Yellow
    $rivetArgs = @('session')
    foreach ($rootPath in $Root) { $rivetArgs += @('--root', $rootPath) }
    foreach ($value in $AllowCommand) { $rivetArgs += @('--allow-command', $value) }
    foreach ($value in $AllowSubcommand) { $rivetArgs += @('--allow-subcommand', $value) }
    foreach ($value in $AllowAnyArgs) { $rivetArgs += @('--allow-any-args', $value) }
    & $rivet @rivetArgs
    if ($LASTEXITCODE -and $LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
finally {
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}
