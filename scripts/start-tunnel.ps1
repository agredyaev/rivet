param(
    [string[]]$Root = @(),
    [string[]]$AllowCommand = @(),
    [string[]]$AllowSubcommand = @(),
    [string[]]$AllowAnyArgs = @()
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$defaultProjectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$projectRoot = if ($env:RIVET_PROJECT_DIR) { $env:RIVET_PROJECT_DIR } else { $defaultProjectRoot }
$rivet = Join-Path $projectRoot 'bin\rivet.exe'
$config = Join-Path $projectRoot 'rivet.toml'
$tunnelClient = Join-Path $projectRoot 'bin\tunnel-client.exe'
$profileDir = Join-Path $projectRoot 'tunnel-client-profiles'
$profileFile = Join-Path $profileDir 'rivet.yaml'
$mcpLauncher = Join-Path $PSScriptRoot 'serve-rivet-mcp.ps1'
$tunnelSync = Join-Path $PSScriptRoot 'sync-tunnel-client.ps1'
$uiScript = Join-Path $PSScriptRoot 'ui.ps1'
if (Test-Path -LiteralPath $uiScript -PathType Leaf) { . $uiScript }

foreach ($path in @($rivet, $config, $mcpLauncher, $tunnelSync)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Required file not found: $path" }
}
if ($Root.Count -eq 0) { throw 'At least one -Root path is required.' }

$rivetArgs = @()
$canonicalRoots = @()
foreach ($rootPath in $Root) {
    $resolvedRoot = (Get-Item -LiteralPath $rootPath -ErrorAction Stop).FullName
    if (-not (Test-Path -LiteralPath $resolvedRoot -PathType Container)) { throw "Workspace directory not found: $resolvedRoot" }
    $canonicalRoots += $resolvedRoot
    $rivetArgs += @('--root', $resolvedRoot)
}
foreach ($value in $AllowCommand) { $rivetArgs += @('--allow-command', $value) }
foreach ($value in $AllowSubcommand) { $rivetArgs += @('--allow-subcommand', $value) }
foreach ($value in $AllowAnyArgs) { $rivetArgs += @('--allow-any-args', $value) }

Write-RivetNote 'Checking Rivet configuration and workspace access...'
& $rivet config-check --config $config @rivetArgs | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Rivet config-check failed.' }
Write-RivetOk 'Configuration is valid'
& $rivet doctor --config $config @rivetArgs | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Rivet doctor failed.' }
Write-RivetOk 'Workspace access is ready'

Write-RivetNote 'Preparing the official OpenAI tunnel client...'
& $tunnelSync -InstallDir (Join-Path $projectRoot 'bin')
if (-not (Test-Path -LiteralPath $tunnelClient -PathType Leaf)) { throw 'tunnel-client was not installed successfully.' }
Write-RivetOk 'tunnel-client is ready'

Write-Host '  OpenAI tunnel: https://platform.openai.com/settings/organization/tunnels'
$tunnelIdSecure = Read-Host '  Tunnel ID (input hidden)' -AsSecureString
$tunnelIdPointer = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($tunnelIdSecure)
try { $tunnelId = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($tunnelIdPointer) }
finally {
    [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($tunnelIdPointer)
    $tunnelIdSecure.Dispose()
}
if ($tunnelId -notmatch '^tunnel_[0-9a-fA-F]{32}$') { throw 'Expected tunnel_ followed by 32 hexadecimal characters.' }

$quotedLauncher = $mcpLauncher.Replace('"', '\"')
$mcpCommand = 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "{0}"' -f $quotedLauncher
function Quote-McpArgument([string]$Value) {
    '"{0}"' -f $Value.Replace('\\', '\\\\').Replace('"', '\"')
}
if ($canonicalRoots.Count -gt 0) {
    $mcpCommand += ' -Root ' + (($canonicalRoots | ForEach-Object { Quote-McpArgument $_ }) -join ' ')
}
if ($AllowCommand.Count -gt 0) {
    $mcpCommand += ' -AllowCommand ' + (($AllowCommand | ForEach-Object { Quote-McpArgument $_ }) -join ' ')
}
if ($AllowSubcommand.Count -gt 0) {
    $mcpCommand += ' -AllowSubcommand ' + (($AllowSubcommand | ForEach-Object { Quote-McpArgument $_ }) -join ' ')
}
if ($AllowAnyArgs.Count -gt 0) {
    $mcpCommand += ' -AllowAnyArgs ' + (($AllowAnyArgs | ForEach-Object { Quote-McpArgument $_ }) -join ' ')
}
New-Item -ItemType Directory -Force -Path $profileDir | Out-Null
$env:TUNNEL_CLIENT_PROFILE_DIR = $profileDir
& $tunnelClient init --force `
    --sample sample_mcp_stdio_local `
    --profile rivet `
    --tunnel-id $tunnelId `
    --mcp-command $mcpCommand `
    --control-plane-api-key-ref 'env:CONTROL_PLANE_API_KEY'
$tunnelId = $null
$mcpCommand = $null
if ($LASTEXITCODE -ne 0) { throw 'Could not create the session tunnel profile.' }

Write-RivetNote 'Checking credentials and starting the tunnel...'
Write-Host '  Runtime API key: https://platform.openai.com/settings/organization/api-keys'
$secureKey = Read-Host '  Runtime API key (input hidden)' -AsSecureString
$keyPointer = [IntPtr]::Zero
$apiKey = $null
try {
    $keyPointer = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secureKey)
    $apiKey = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($keyPointer)
    if ([string]::IsNullOrWhiteSpace($apiKey)) { throw 'Runtime API key cannot be empty.' }
    $env:CONTROL_PLANE_API_KEY = $apiKey
    $apiKey = $null

    & $tunnelClient doctor --profile rivet --explain
    if ($LASTEXITCODE -ne 0) { throw 'Tunnel credentials or scope validation failed.' }
    Write-RivetOk 'Credentials accepted'
    Write-RivetNote 'Tunnel is running. Keep this terminal open; press Ctrl+C to stop it.'
    & $tunnelClient run --profile rivet
    if ($LASTEXITCODE -ne 0) { throw 'tunnel-client exited with an error.' }
}
finally {
    if ($keyPointer -ne [IntPtr]::Zero) { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($keyPointer) }
    $env:CONTROL_PLANE_API_KEY = $null
    $env:TUNNEL_CLIENT_PROFILE_DIR = $null
    $secureKey.Dispose()
    $apiKey = $null
    Remove-Item -LiteralPath $profileFile -Force -ErrorAction SilentlyContinue
}
