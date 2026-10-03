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
$previousUiState = $env:RIVET_UI_STARTED
if ($Help) {
    Write-Host "Usage: invoke install.ps1 with optional -Root PATH -AllowCommand NAME=EXECUTABLE -AllowSubcommand NAME=VALUE"
    Write-Host "Downloads the latest verified Rivet release, installs it under $installRoot, and starts one tunnel session."
    exit 0
}
if (-not $env:LOCALAPPDATA) { throw 'LOCALAPPDATA is not set.' }

Write-Host ''
Write-Host 'Rivet setup' -ForegroundColor Cyan
Write-Host '  Detecting Windows x64 release...' -ForegroundColor Yellow
$releaseUri = "https://api.github.com/repos/$repo/releases/latest"
$headers = @{ 'User-Agent' = 'Rivet Installer' }
$release = Invoke-RestMethod -Uri $releaseUri -Headers $headers
$tag = [string]$release.tag_name
if ($tag -notmatch '^v?[A-Za-z0-9._-]+$') { throw 'GitHub returned an invalid release tag.' }
$asset = 'rivet-windows-x64.zip'
$releaseBase = "https://github.com/$repo/releases/download/$tag"
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
    $launcher = Join-Path $versionDir 'start-rivet.ps1'
    if (-not (Test-Path -LiteralPath (Join-Path $versionDir 'bin\rivet.exe') -PathType Leaf) -or
        -not (Test-Path -LiteralPath $launcher -PathType Leaf)) {
        throw 'Release archive is missing the Rivet launcher or binary.'
    }
    Write-Host "  ✓ Installed in $versionDir" -ForegroundColor Green
    Write-Host '  Starting Rivet with the scope for this session...' -ForegroundColor Yellow
    $env:RIVET_UI_STARTED = '1'
    & $launcher -Root $Root -AllowCommand $AllowCommand -AllowSubcommand $AllowSubcommand -AllowAnyArgs $AllowAnyArgs
    if ($LASTEXITCODE -and $LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
finally {
    if ($null -eq $previousUiState) { Remove-Item Env:RIVET_UI_STARTED -ErrorAction SilentlyContinue }
    else { $env:RIVET_UI_STARTED = $previousUiState }
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}
