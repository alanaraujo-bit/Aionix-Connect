<#
.SYNOPSIS
    Packages the local validation build: copies the release executables to
    dist\aionix-connect-dev\ and creates the "Aionix Connect Dev Launcher" shortcuts.

.DESCRIPTION
    Running the apps from dist\ (not target\) means a rebuild never fails because an
    executable is open, and an open app never runs a half-written binary.

    Shortcuts (no files on the Desktop):
      <repo>\Aionix Connect Dev Launcher.lnk
      Start Menu\Programs\Aionix Connect (desenvolvimento)\Aionix Connect Dev Launcher.lnk

.EXAMPLE
    cargo build --release -p aionix-desktop -p aionix-control-plane -p aionix-signaling -p aionix-relay --bins
    .\infra\local\package-dev.ps1
#>
[CmdletBinding()]
param([switch] $NoShortcuts)

$ErrorActionPreference = 'Stop'
$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Source = Join-Path $Repo 'target\release'
$Dist = Join-Path $Repo 'dist\aionix-connect-dev'
$Exes = 'aionix-launcher', 'aionix-host', 'aionix-technician', 'aionix-api', 'aionix-signaling', 'aionix-relay', 'aionix-admin', 'aionix-healthcheck'

New-Item -ItemType Directory -Force $Dist | Out-Null
foreach ($e in $Exes) {
    $src = Join-Path $Source "$e.exe"
    if (-not (Test-Path $src)) { throw "$e.exe não encontrado em $Source (compile em release antes)" }
    $dst = Join-Path $Dist "$e.exe"
    try { Copy-Item $src $dst -Force }
    catch { throw "não foi possível atualizar $e.exe — feche o Aionix Connect e tente de novo ($($_.Exception.Message))" }
}
$commit = (& git -C $Repo rev-parse --short HEAD 2>$null)
Set-Content (Join-Path $Dist 'BUILD.txt') "Aionix Connect — build de validação local`r`ncommit: $commit`r`ngerado em: $(Get-Date -Format 'yyyy-MM-dd HH:mm')`r`n"
Write-Host "Executáveis em $Dist"

if (-not $NoShortcuts) {
    $shell = New-Object -ComObject WScript.Shell
    $menu = Join-Path ([Environment]::GetFolderPath('Programs')) 'Aionix Connect (desenvolvimento)'
    New-Item -ItemType Directory -Force $menu | Out-Null
    foreach ($lnkPath in (Join-Path $Repo 'Aionix Connect Dev Launcher.lnk'), (Join-Path $menu 'Aionix Connect Dev Launcher.lnk')) {
        $lnk = $shell.CreateShortcut($lnkPath)
        $lnk.TargetPath = Join-Path $Dist 'aionix-launcher.exe'
        $lnk.WorkingDirectory = $Dist
        $lnk.Description = 'Inicia o servidor local e abre o Aionix Connect e o Aionix Connect Técnico'
        $lnk.IconLocation = (Join-Path $Dist 'aionix-launcher.exe') + ',0'
        $lnk.Save()
        Write-Host "Atalho: $lnkPath"
    }
}
