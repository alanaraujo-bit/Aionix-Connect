<#
.SYNOPSIS
    TEMPORARY internet access to the local development server, for testing with a
    computer on another network (e.g. a friend's PC), plus the package to install there.

.DESCRIPTION
    No router configuration: a Cloudflare "quick tunnel" (cloudflared, no account) gives
    the local server a temporary https://<random>.trycloudflare.com address.

      internet ── Cloudflare ── cloudflared ── Caddy :18088 ─┬─ /v1/*       → aionix-api       :18080
                                                             ├─ /v1/signal  → aionix-signaling :18081
                                                             └─ /relay      → aionix-relay     :13340

    Media is end-to-end encrypted between the two applications (QUIC/TLS keyed by the
    device identities); the tunnel and the relay only see ciphertext.

    The API announces the public address as the relay, so both ends reach each other
    through it. The address changes every time the tunnel restarts: the client package
    is regenerated each time (dist\Aionix-Connect-Cliente.zip).

    While it runs, the development server is reachable from the internet. Stop it with
    -Action Down when the test is over.

.EXAMPLE
    .\infra\local\public-tunnel.ps1 -Action Up -BinDir dist\aionix-connect-dev
    .\infra\local\public-tunnel.ps1 -Action Down
#>
[CmdletBinding()]
param(
    [ValidateSet('Up', 'Down', 'Status')]
    [string] $Action = 'Status',
    [string] $BinDir
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$State = Join-Path $Repo '.data\native'
$Logs = Join-Path $State 'logs'
$Dev = Join-Path $Repo '.data\dev'
$UrlFile = Join-Path $Dev 'public-url.txt'
$Tools = Join-Path $Repo '.devtools'
$Bin = if ($BinDir) { (Resolve-Path $BinDir).Path } else { Join-Path $Repo 'target\release' }
$Native = Join-Path $PSScriptRoot 'native-stack.ps1'

function Write-Step([string] $t) { Write-Host "==> $t" -ForegroundColor Cyan }

function Test-Port([int] $Port) {
    $c = [Net.Sockets.TcpClient]::new()
    try { $c.ConnectAsync('127.0.0.1', $Port).Wait(300) -and $c.Connected } catch { $false } finally { $c.Dispose() }
}

. (Join-Path $PSScriptRoot 'lib.ps1')

function Stop-Named([string] $Name) { Stop-Detached -Name $Name -State $State }

function Start-Hidden([string] $Name, [string] $Exe, [string[]] $Arguments) {
    Start-Detached -Name $Name -Exe $Exe -Arguments $Arguments -State $State -Logs $Logs
}

function Get-Status([string] $Url) {
    try { (Invoke-WebRequest $Url -UseBasicParsing -TimeoutSec 8).StatusCode } catch { 0 }
}

function Start-Tunnel {
    New-Item -ItemType Directory -Force $Logs, $Dev | Out-Null
    foreach ($t in 'cloudflared.exe', 'caddy\caddy.exe') {
        if (-not (Test-Path (Join-Path $Tools $t))) { throw "$t não encontrado em $Tools" }
    }
    Write-Step 'Servidor local'
    & pwsh -NoProfile -File $Native -Action Up -BinDir $Bin
    if ($LASTEXITCODE -ne 0) { throw 'native-stack Up falhou' }

    if (-not (Test-Port 18088)) {
        Write-Step 'Caddy (endereço único na porta 18088)'
        Start-Hidden 'caddy' (Join-Path $Tools 'caddy\caddy.exe') @('run', '--config', "`"$(Join-Path $PSScriptRoot 'Caddyfile.native')`"", '--adapter', 'caddyfile') | Out-Null
        $deadline = (Get-Date).AddSeconds(20)
        while (-not (Test-Port 18088)) { if ((Get-Date) -gt $deadline) { throw 'Caddy não iniciou' }; Start-Sleep -Milliseconds 300 }
    }
    if ((Get-Status 'http://127.0.0.1:18088/healthz') -ne 200) { throw 'Caddy não encaminha para a API' }

    Write-Step 'Túnel Cloudflare (temporário)'
    Stop-Named 'cloudflared'
    $log = Join-Path $Logs 'cloudflared.log'
    Remove-Item $log -ErrorAction SilentlyContinue
    Start-Hidden 'cloudflared' (Join-Path $Tools 'cloudflared.exe') @('tunnel', '--no-autoupdate', '--url', 'http://127.0.0.1:18088') | Out-Null
    $url = $null
    $deadline = (Get-Date).AddSeconds(90)
    while (-not $url) {
        if ((Get-Date) -gt $deadline) { throw "o túnel não informou um endereço (veja $log)" }
        Start-Sleep -Seconds 1
        $m = Select-String -Path $log -Pattern 'https://[a-z0-9-]+\.trycloudflare\.com' -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($m) { $url = $m.Matches[0].Value }
    }
    Write-Host "  endereço público: $url"
    $deadline = (Get-Date).AddSeconds(120)
    while ((Get-Status "$url/healthz") -ne 200) {
        if ((Get-Date) -gt $deadline) { throw "o endereço público não respondeu ($url)" }
        Start-Sleep -Seconds 2
    }

    Write-Step 'API anunciando o relay público'
    Stop-Named 'aionix-api'
    & pwsh -NoProfile -File $Native -Action Up -BinDir $Bin -RelayUrl $url
    if ($LASTEXITCODE -ne 0) { throw 'reinício da API falhou' }
    $deadline = (Get-Date).AddSeconds(60)
    while ((Get-Status "$url/v1/server/info") -ne 200) {
        if ((Get-Date) -gt $deadline) { throw 'a API não voltou pelo endereço público' }
        Start-Sleep -Seconds 2
    }
    $info = Invoke-RestMethod "$url/v1/server/info"
    if ($info.relay_urls -notcontains $url) { throw "a API anuncia $($info.relay_urls -join ', ') em vez de $url" }
    Set-Content $UrlFile $url

    Write-Step 'Pacote para o computador atendido'
    $pkg = Join-Path $Repo 'dist\Aionix Connect'
    Remove-Item $pkg -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $pkg | Out-Null
    Copy-Item (Join-Path $Bin 'aionix-host.exe') (Join-Path $pkg 'Aionix Connect.exe')
    @{ server = $url; environment_label = 'Teste com servidor temporário (desenvolvimento)' } | ConvertTo-Json |
        Set-Content -Encoding utf8 (Join-Path $pkg 'aionix-host.json')
    $readme = @"
Aionix Connect — teste de acesso remoto
=======================================

1. Extraia esta pasta em qualquer lugar (por exemplo, na Área de Trabalho).
2. Abra "Aionix Connect.exe".
   O Windows pode avisar "O Windows protegeu o computador" porque este programa de
   teste não é assinado: clique em "Mais informações" e depois em "Executar assim mesmo".
   Se o Windows perguntar sobre o Firewall, tanto faz: Permitir deixa a conexão mais
   direta; Cancelar também funciona (o tráfego passa pelo relay, criptografado).
3. Informe ao técnico o ID de 9 dígitos exibido na janela.
4. Quando aparecer "Um técnico está solicitando acesso a este computador",
   confira o nome do técnico e clique em Permitir (ou Recusar).
5. Para terminar, clique em "Encerrar sessão" ou feche o programa.

Ninguém acessa este computador sem você clicar em Permitir. O vídeo e os comandos
viajam criptografados de ponta a ponta. Para remover, apague a pasta e
%LOCALAPPDATA%\Aionix Connect.

Servidor deste teste: $url (temporário)
"@
    Set-Content -Encoding utf8 (Join-Path $pkg 'LEIA-ME.txt') $readme
    $zip = Join-Path $Repo 'dist\Aionix-Connect-Cliente.zip'
    Remove-Item $zip -ErrorAction SilentlyContinue
    Compress-Archive -Path $pkg -DestinationPath $zip
    Write-Host "`nEndereço público: $url" -ForegroundColor Green
    Write-Host "Pacote para o seu amigo: $zip" -ForegroundColor Green
}

try {
    switch ($Action) {
        'Up' { Start-Tunnel }
        'Down' {
            Stop-Named 'cloudflared'; Stop-Named 'caddy'
            Remove-Item $UrlFile -ErrorAction SilentlyContinue
            Write-Host 'túnel encerrado (o servidor local continua rodando; native-stack.ps1 -Action Down para parar tudo)'
        }
        'Status' {
            if (Test-Path $UrlFile) { $u = Get-Content $UrlFile; Write-Host "$u/healthz -> $(Get-Status "$u/healthz")" } else { Write-Host 'túnel não iniciado' }
        }
    }
}
catch {
    Write-Host "ERRO: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
