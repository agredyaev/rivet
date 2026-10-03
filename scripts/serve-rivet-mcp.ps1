param(
    [string[]]$Root = @(),
    [string[]]$AllowCommand = @(),
    [string[]]$AllowSubcommand = @(),
    [string[]]$AllowAnyArgs = @()
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$env:CONTROL_PLANE_API_KEY = $null
$env:OPENAI_API_KEY = $null
$env:OPENAI_ADMIN_KEY = $null

$defaultProjectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$projectRoot = if ($env:RIVET_PROJECT_DIR) { $env:RIVET_PROJECT_DIR } else { $defaultProjectRoot }
$rivet = Join-Path $projectRoot 'bin\rivet.exe'
$config = Join-Path $projectRoot 'rivet.toml'
$RivetArgs = @()
foreach ($value in $Root) { $RivetArgs += @('--root', $value) }
foreach ($value in $AllowCommand) { $RivetArgs += @('--allow-command', $value) }
foreach ($value in $AllowSubcommand) { $RivetArgs += @('--allow-subcommand', $value) }
foreach ($value in $AllowAnyArgs) { $RivetArgs += @('--allow-any-args', $value) }
& $rivet serve --config $config @RivetArgs
exit $LASTEXITCODE
