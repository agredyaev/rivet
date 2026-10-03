param([Parameter(Mandatory = $true)][string]$InstallDir)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$clientName = 'tunnel-client.exe'
$clientPath = Join-Path $InstallDir $clientName
$releaseUri = 'https://api.github.com/repos/openai/tunnel-client/releases/latest'
$headers = @{
    Accept = 'application/vnd.github+json'
    'User-Agent' = 'Rivet-Setup'
}

$architecture = switch ([Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString()) {
    'X64' { 'amd64' }
    'Arm64' { 'arm64' }
    default { $_.ToLowerInvariant() }
}
if ($architecture -notin @('amd64', 'arm64')) {
    if ((Test-Path -LiteralPath $clientPath -PathType Leaf) -and
        (Test-Path -LiteralPath (Join-Path $InstallDir 'cloudflared.exe') -PathType Leaf)) {
        Write-Warning "Unsupported architecture for checking tunnel-client updates; using the installed binary."
        return
    }
    throw "Unsupported Windows architecture for tunnel-client: $architecture"
}

try {
    $release = Invoke-RestMethod -Uri $releaseUri -Headers $headers
}
catch {
    if ((Test-Path -LiteralPath $clientPath -PathType Leaf) -and
        (Test-Path -LiteralPath (Join-Path $InstallDir 'cloudflared.exe') -PathType Leaf)) {
        Write-Warning 'Could not check the latest tunnel-client release; using the installed binary.'
        return
    }
    throw 'Could not reach GitHub to download tunnel-client.'
}

$tag = [string]$release.tag_name
if ($tag -notmatch '^v\d+\.\d+\.\d+$' -or $release.prerelease) {
    throw "GitHub returned an unexpected tunnel-client stable release tag: $tag"
}
$version = $tag.Substring(1)
if ((Test-Path -LiteralPath $clientPath -PathType Leaf) -and
    (Test-Path -LiteralPath (Join-Path $InstallDir 'cloudflared.exe') -PathType Leaf)) {
    $installedVersion = (& $clientPath --version 2>$null | Out-String).Trim()
    if ($installedVersion -match "^$([regex]::Escape($version))(\+|\s|$)") {
        Write-Host "tunnel-client $tag is already current."
        return
    }
}

$assetName = "tunnel-client-$tag-windows-$architecture.zip"
$asset = @($release.assets | Where-Object { $_.name -eq $assetName })
$checksumAsset = @($release.assets | Where-Object { $_.name -eq 'SHA256SUMS.txt' })
if ($asset.Count -ne 1 -or $checksumAsset.Count -ne 1) {
    throw "The official release does not contain the expected Windows archive and checksum: $assetName"
}
foreach ($download in @($asset[0], $checksumAsset[0])) {
    if (-not ([uri]$download.browser_download_url).AbsoluteUri.StartsWith('https://github.com/openai/tunnel-client/releases/download/')) {
        throw 'GitHub returned an unexpected release download URL.'
    }
}

$tempDir = Join-Path ([IO.Path]::GetTempPath()) ("rivet-tunnel-client-" + [guid]::NewGuid().ToString('N'))
$stageDir = Join-Path $tempDir 'unpacked'
New-Item -ItemType Directory -Path $stageDir -Force | Out-Null
try {
    $checksumsPath = Join-Path $tempDir 'SHA256SUMS.txt'
    $archivePath = Join-Path $tempDir $assetName
    Invoke-WebRequest -UseBasicParsing -Uri $checksumAsset[0].browser_download_url -OutFile $checksumsPath
    $checksumLine = $null
    foreach ($line in Get-Content -LiteralPath $checksumsPath) {
        if ($line -match '^\s*([0-9a-fA-F]{64})\s+\*?(.+?)\s*$' -and $Matches[2] -eq $assetName) {
            $checksumLine = $line
            break
        }
    }
    if (-not $checksumLine) { throw "Official checksum is missing for $assetName" }
    $expectedHash = [regex]::Match($checksumLine, '[0-9a-fA-F]{64}').Value.ToLowerInvariant()

    Invoke-WebRequest -UseBasicParsing -Uri $asset[0].browser_download_url -OutFile $archivePath
    $actualHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -ne $expectedHash) { throw "SHA-256 verification failed for $assetName" }

    Expand-Archive -LiteralPath $archivePath -DestinationPath $stageDir
    if (-not (Test-Path -LiteralPath (Join-Path $stageDir 'tunnel-client.exe') -PathType Leaf) -or
        -not (Test-Path -LiteralPath (Join-Path $stageDir 'cloudflared.exe') -PathType Leaf)) {
        throw 'The official archive is missing tunnel-client.exe or cloudflared.exe.'
    }
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Get-ChildItem -LiteralPath $stageDir -File | ForEach-Object {
        Move-Item -LiteralPath $_.FullName -Destination (Join-Path $InstallDir $_.Name) -Force
    }
    Write-Host "Installed official tunnel-client $tag for windows-$architecture (SHA-256 verified)."
}
finally {
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}
