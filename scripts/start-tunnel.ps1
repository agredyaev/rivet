$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$defaultProjectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$projectRoot = if ($env:RIVET_PROJECT_DIR) { $env:RIVET_PROJECT_DIR } else { $defaultProjectRoot }
$rivet = Join-Path $projectRoot 'bin\rivet.exe'
$config = Join-Path $projectRoot 'rivet.toml'
$tunnelClient = Join-Path $projectRoot 'bin\tunnel-client.exe'
$profileDir = Join-Path $projectRoot 'tunnel-client'
$mcpLauncher = Join-Path $PSScriptRoot 'serve-rivet-mcp.ps1'

foreach ($path in @($rivet, $config, $tunnelClient, $mcpLauncher)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Required file not found: $path"
    }
}

& $rivet config-check --config $config
if ($LASTEXITCODE -ne 0) { throw 'Rivet config-check failed.' }
& $rivet doctor --config $config
if ($LASTEXITCODE -ne 0) { throw 'Rivet doctor failed.' }

$tunnelId = Read-Host 'OpenAI tunnel ID'
if ($tunnelId -notmatch '^tunnel_[0-9a-fA-F]{32}$') {
    throw 'Expected tunnel_ followed by 32 hexadecimal characters.'
}

New-Item -ItemType Directory -Force -Path $profileDir | Out-Null
$env:TUNNEL_CLIENT_PROFILE_DIR = $profileDir
$mcpLauncherForCommand = $mcpLauncher.Replace('\', '/')
$mcpCommand = 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "{0}"' -f $mcpLauncherForCommand
& $tunnelClient init `
    --sample sample_mcp_stdio_local `
    --profile rivet `
    --force `
    --tunnel-id $tunnelId `
    --mcp-command $mcpCommand `
    --control-plane-api-key-ref 'env:CONTROL_PLANE_API_KEY'
if ($LASTEXITCODE -ne 0) { throw 'Could not create the tunnel-client profile.' }
$tunnelId = $null
$mcpLauncherForCommand = $null
$mcpCommand = $null

$secureKey = Read-Host 'OpenAI runtime API key (input hidden)' -AsSecureString
$keyPointer = [IntPtr]::Zero
$apiKey = $null
try {
    $keyPointer = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secureKey)
    $apiKey = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($keyPointer)
    if ([string]::IsNullOrWhiteSpace($apiKey)) { throw 'Runtime API key cannot be empty.' }

    $env:CONTROL_PLANE_API_KEY = $apiKey
    $apiKey = $null
    & $tunnelClient doctor --profile rivet --explain
    if ($LASTEXITCODE -ne 0) { throw 'tunnel-client doctor failed.' }

    & $tunnelClient run --profile rivet
    if ($LASTEXITCODE -ne 0) { throw 'tunnel-client exited with an error.' }
}
finally {
    if ($keyPointer -ne [IntPtr]::Zero) {
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($keyPointer)
    }
    $env:CONTROL_PLANE_API_KEY = $null
    $env:TUNNEL_CLIENT_PROFILE_DIR = $null
    $secureKey.Dispose()
    $apiKey = $null
}
