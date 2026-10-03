$script:RivetColorEnabled = [Environment]::GetEnvironmentVariable('NO_COLOR') -eq $null -and
    -not [Console]::IsOutputRedirected

function Write-RivetColor([string]$Message, [ConsoleColor]$Color) {
    if ($script:RivetColorEnabled) { Write-Host $Message -ForegroundColor $Color }
    else { Write-Host $Message }
}

function Write-RivetHeader {
    Write-Host ''
    Write-RivetColor 'Rivet setup' Cyan
}

function Write-RivetOk([string]$Message) {
    Write-RivetColor "  ✓ $Message" Green
}

function Write-RivetNote([string]$Message) {
    Write-RivetColor "  $Message" DarkGray
}
