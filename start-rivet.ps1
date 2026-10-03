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
$config = Join-Path $projectRoot 'rivet.toml'
$uiScript = Join-Path $projectRoot 'rivet\scripts\ui.ps1'
if (-not (Test-Path -LiteralPath $uiScript -PathType Leaf)) {
    $uiScript = Join-Path $projectRoot 'scripts\ui.ps1'
}
if (Test-Path -LiteralPath $uiScript -PathType Leaf) { . $uiScript }
if ($Help) {
    Write-Host 'Usage: .\start-rivet.ps1 -Root PATH [-Root PATH ...] [-AllowCommand NAME=EXECUTABLE] [-AllowSubcommand NAME=VALUE] [-AllowAnyArgs NAME]'
    exit 0
}
if (-not (Test-Path -LiteralPath $rivet -PathType Leaf) -or -not (Test-Path -LiteralPath $config -PathType Leaf)) {
    throw 'This is not a complete Rivet release package. Extract the platform ZIP and run start-rivet.ps1 from its folder.'
}

if ($env:RIVET_UI_STARTED -ne '1') { Write-RivetHeader }
$env:RIVET_UI_STARTED = $null
if ($Root.Count -eq 0) {
    Write-Host '  Rivet can read and write files only inside the workspace you choose.'
    $defaultRoot = (Get-Location).Path
    $workspaceRoot = Read-Host "  Workspace directory (Enter uses $defaultRoot)"
    if ([string]::IsNullOrWhiteSpace($workspaceRoot)) { $workspaceRoot = $defaultRoot }
    if ($workspaceRoot -eq '~') { $workspaceRoot = $HOME }
    elseif ($workspaceRoot.StartsWith('~\')) { $workspaceRoot = Join-Path $HOME $workspaceRoot.Substring(2) }
    $Root = @($workspaceRoot)
}

$rivetArgs = @()
$resolvedRoots = @()
foreach ($workspaceRoot in $Root) {
    if ($workspaceRoot -eq '~') { $workspaceRoot = $HOME }
    elseif ($workspaceRoot.StartsWith('~\')) { $workspaceRoot = Join-Path $HOME $workspaceRoot.Substring(2) }
    $resolvedRoot = (Get-Item -LiteralPath $workspaceRoot -ErrorAction Stop).FullName
    if (-not (Test-Path -LiteralPath $resolvedRoot -PathType Container)) { throw "Workspace directory not found: $resolvedRoot" }
    $resolvedRoots += $resolvedRoot
    $rivetArgs += @('--root', $resolvedRoot)
    Write-RivetOk "Workspace: $resolvedRoot"
}
foreach ($value in $AllowCommand) { $rivetArgs += @('--allow-command', $value) }
foreach ($value in $AllowSubcommand) { $rivetArgs += @('--allow-subcommand', $value) }
foreach ($value in $AllowAnyArgs) { $rivetArgs += @('--allow-any-args', $value) }

$previousProjectRoot = $env:RIVET_PROJECT_DIR
try {
    $env:RIVET_PROJECT_DIR = $projectRoot
    $tunnelLauncher = Join-Path $projectRoot 'rivet\scripts\start-tunnel.ps1'
    if (-not (Test-Path -LiteralPath $tunnelLauncher -PathType Leaf)) {
        $tunnelLauncher = Join-Path $projectRoot 'scripts\start-tunnel.ps1'
    }
    if (-not (Test-Path -LiteralPath $tunnelLauncher -PathType Leaf)) {
        throw 'Tunnel launcher is missing from this package.'
    }
    & $tunnelLauncher -Root $resolvedRoots -AllowCommand $AllowCommand -AllowSubcommand $AllowSubcommand -AllowAnyArgs $AllowAnyArgs
}
finally {
    $env:RIVET_PROJECT_DIR = $previousProjectRoot
}
