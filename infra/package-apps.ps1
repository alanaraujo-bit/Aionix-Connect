<#
.SYNOPSIS
    Packages the desktop apps for a real Aionix server (not the local dev stack).

.DESCRIPTION
    Creates dist\Aionix-Connect-<name>\ with:
      Atendido\  "Aionix Connect.exe" + aionix-host.json (server URL) + LEIA-ME.txt
                 → zipped to dist\Aionix-Connect-<name>-Atendido.zip for other computers
      Tecnico\   "Aionix Connect Tecnico.exe" + "Aionix Connect Tecnico.cmd" (starts it
                 against the server)
    and Start Menu shortcuts on this computer (folder "Aionix Connect (<name>)").

    The host (computer being supported) enrolls as Quick Support on first start and shows
    its 9-digit ID; the technician signs in with an account created on the server
    (aionix-admin create-user / add-member).

.EXAMPLE
    cargo build --release -p aionix-desktop --bins
    .\infra\package-apps.ps1 -Server http://192.168.18.110 -Name servidor
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $Server,
    [string] $Name = 'servidor',
    [string] $EnvironmentLabel,
    [switch] $NoShortcuts
)

$ErrorActionPreference = 'Stop'
$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Bin = Join-Path $Repo 'target\release'
$Server = $Server.TrimEnd('/')
if ($Name -notmatch '^[A-Za-z0-9-]+$') { throw "Name inválido: $Name" }
if (-not $EnvironmentLabel) { $EnvironmentLabel = "Servidor $Server · teste" }
foreach ($e in 'aionix-host', 'aionix-technician') {
    if (-not (Test-Path (Join-Path $Bin "$e.exe"))) { throw "$e.exe não encontrado em $Bin (cargo build --release -p aionix-desktop --bins)" }
}

# Fail early if the server is not reachable from here.
try { $info = Invoke-RestMethod "$Server/v1/server/info" -TimeoutSec 10 }
catch { throw "servidor $Server não respondeu em /v1/server/info: $($_.Exception.Message)" }
Write-Host "Servidor: $($info.name) $($info.version) · relay $($info.relay_urls -join ', ')"

$Out = Join-Path $Repo "dist\Aionix-Connect-$Name"
Remove-Item $Out -Recurse -Force -ErrorAction SilentlyContinue
$Host_ = Join-Path $Out 'Atendido'
$Tech = Join-Path $Out 'Tecnico'
New-Item -ItemType Directory -Force $Host_, $Tech | Out-Null

# --- computer being supported -----------------------------------------------------------
Copy-Item (Join-Path $Bin 'aionix-host.exe') (Join-Path $Host_ 'Aionix Connect.exe')
@{ server = $Server; environment_label = $EnvironmentLabel } | ConvertTo-Json |
    Set-Content -Encoding utf8 (Join-Path $Host_ 'aionix-host.json')
@"
Aionix Connect — computador atendido
====================================

1. Extraia esta pasta em qualquer lugar e abra "Aionix Connect.exe".
   O Windows pode avisar "O Windows protegeu o computador" (programa de teste sem
   assinatura): "Mais informações" → "Executar assim mesmo".
2. Informe ao técnico o ID de 9 dígitos exibido na janela.
3. Quando aparecer "Um técnico está solicitando acesso a este computador", confira o nome
   e clique em Permitir (ou Recusar).
4. Para terminar: "Encerrar sessão" ou feche o programa.

Ninguém acessa este computador sem você clicar em Permitir. Vídeo e comandos viajam
criptografados de ponta a ponta.

Servidor: $Server
"@ | Set-Content -Encoding utf8 (Join-Path $Host_ 'LEIA-ME.txt')

# --- technician -------------------------------------------------------------------------
Copy-Item (Join-Path $Bin 'aionix-technician.exe') (Join-Path $Tech 'Aionix Connect Tecnico.exe')
$techData = "%LOCALAPPDATA%\Aionix Connect\$Name\technician"
@"
@echo off
rem Abre o Aionix Connect Tecnico conectado a $Server
start "" "%~dp0Aionix Connect Tecnico.exe" --server $Server --data-dir "$techData" --environment-label "$EnvironmentLabel"
"@ | Set-Content -Encoding ascii (Join-Path $Tech 'Aionix Connect Tecnico.cmd')

$zip = Join-Path $Repo "dist\Aionix-Connect-$Name-Atendido.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path $Host_ -DestinationPath $zip
$commit = (& git -C $Repo rev-parse --short HEAD 2>$null)
Set-Content (Join-Path $Out 'BUILD.txt') "commit: $commit`r`nservidor: $Server`r`ngerado em: $(Get-Date -Format 'yyyy-MM-dd HH:mm')`r`n"

if (-not $NoShortcuts) {
    $shell = New-Object -ComObject WScript.Shell
    $menu = Join-Path ([Environment]::GetFolderPath('Programs')) "Aionix Connect ($Name)"
    New-Item -ItemType Directory -Force $menu | Out-Null
    $data = Join-Path $env:LOCALAPPDATA "Aionix Connect\$Name"
    $lnk = $shell.CreateShortcut((Join-Path $menu 'Aionix Connect Tecnico.lnk'))
    $lnk.TargetPath = Join-Path $Tech 'Aionix Connect Tecnico.exe'
    $lnk.Arguments = "--server $Server --data-dir `"$data\technician`" --environment-label `"$EnvironmentLabel`""
    $lnk.WorkingDirectory = $Tech
    $lnk.IconLocation = (Join-Path $Tech 'Aionix Connect Tecnico.exe') + ',0'
    $lnk.Save()
    $lnk = $shell.CreateShortcut((Join-Path $menu 'Aionix Connect (atendido).lnk'))
    $lnk.TargetPath = Join-Path $Host_ 'Aionix Connect.exe'
    $lnk.Arguments = "--data-dir `"$data\host`""
    $lnk.WorkingDirectory = $Host_
    $lnk.IconLocation = (Join-Path $Host_ 'Aionix Connect.exe') + ',0'
    $lnk.Save()
    Write-Host "Atalhos: $menu"
}
Write-Host "Pacote: $Out" -ForegroundColor Green
Write-Host "Zip para outros computadores: $zip" -ForegroundColor Green
