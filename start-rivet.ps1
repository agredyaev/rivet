param(
    [string[]]$Root = @(),
    [string[]]$AllowCommand = @(),
    [string[]]$AllowSubcommand = @(),
    [string[]]$AllowAnyArgs = @(),
    [switch]$Help
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$projectRoot = $PSScriptRoot
$rivet = Join-Path $projectRoot 'bin\rivet.exe'
if ($Help) {
    Write-Host 'Usage: .\start-rivet.ps1 [-Root PATH] [-AllowCommand NAME=EXECUTABLE] [-AllowSubcommand NAME=VALUE] [-AllowAnyArgs NAME]'
    exit 0
}
if (-not (Test-Path -LiteralPath $rivet -PathType Leaf) -or -not (Test-Path -LiteralPath (Join-Path $projectRoot 'rivet.toml') -PathType Leaf)) {
    throw 'Extract the Rivet release ZIP and run start-rivet.ps1 from its folder.'
}

$env:RIVET_PROJECT_DIR = $projectRoot
$rivetArgs = @('session')
foreach ($rootPath in $Root) { $rivetArgs += @('--root', $rootPath) }
foreach ($value in $AllowCommand) { $rivetArgs += @('--allow-command', $value) }
foreach ($value in $AllowSubcommand) { $rivetArgs += @('--allow-subcommand', $value) }
foreach ($value in $AllowAnyArgs) { $rivetArgs += @('--allow-any-args', $value) }
& $rivet @rivetArgs
exit $LASTEXITCODE
