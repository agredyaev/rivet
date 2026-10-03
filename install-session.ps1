$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest

$workspaceRoot = (Get-Location).Path
$allowedCommands = @('git=git', 'cargo=cargo', 'uv=uv', 'make=make')
$allowedGitSubcommands = @('status', 'diff', 'log', 'show', 'add', 'commit')
$commandsAllowAnyArgs = @('cargo', 'uv', 'make')
$installArgs = @('-Root', $workspaceRoot)

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
