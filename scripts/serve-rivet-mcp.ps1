$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$env:CONTROL_PLANE_API_KEY = $null
$env:OPENAI_API_KEY = $null
$env:OPENAI_ADMIN_KEY = $null

$defaultProjectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$projectRoot = if ($env:RIVET_PROJECT_DIR) { $env:RIVET_PROJECT_DIR } else { $defaultProjectRoot }
$rivet = Join-Path $projectRoot 'bin\rivet.exe'
$config = Join-Path $projectRoot 'rivet.toml'
& $rivet serve --config $config
exit $LASTEXITCODE
