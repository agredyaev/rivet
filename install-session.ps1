$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest

$workspaceRoot = (Get-Location).Path
$allowedCommands = @('git=git', 'cargo=cargo', 'uv=uv', 'make=make')
$allowedGitSubcommands = @('status', 'diff', 'log', 'show', 'add', 'commit')
$commandsAllowAnyArgs = @('cargo', 'uv', 'make')
$installArgs = @('-Root', $workspaceRoot)

Write-Host ''
Write-Host 'Rivet session scope' -ForegroundColor Cyan
Write-Host "  Workspace: $workspaceRoot"
Write-Host '  Commands added for this session:'
foreach ($subcommand in $allowedGitSubcommands) {
    Write-Host "    ✓ git $subcommand" -ForegroundColor Green
}
foreach ($command in $commandsAllowAnyArgs) {
    Write-Host "    ✓ $command (any arguments)" -ForegroundColor Green
}
Write-Host '  Scope applies only to this tunnel session.'
Write-Host ''

foreach ($command in $allowedCommands) {
    $installArgs += @('-AllowCommand', $command)
}
foreach ($subcommand in $allowedGitSubcommands) {
    $installArgs += @('-AllowSubcommand', "git=$subcommand")
}
foreach ($command in $commandsAllowAnyArgs) {
    $installArgs += @('-AllowAnyArgs', $command)
}

$installer = Invoke-RestMethod 'https://raw.githubusercontent.com/agredyaev/rivet/main/install.ps1'
& ([scriptblock]::Create($installer)) @installArgs
