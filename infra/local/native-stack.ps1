<#
.SYNOPSIS
    DEVELOPMENT ONLY: runs the Aionix Connect server binaries natively on Windows
    (no containers) against a local PostgreSQL and Redis.

.DESCRIPTION
    The supported staging/production path is Docker (infra/local/staging.ps1). This
    script exists for workstations where Docker is not usable yet: it runs the very same
    aionix-api / aionix-signaling / aionix-relay binaries as plain processes.

    Dependencies (fetched by the dev tooling into .devtools/, gitignored):
      .devtools/pg17/pgsql/bin   official PostgreSQL 17 Windows binaries (EDB zip)
      .devtools/redis3           Microsoft's archived Redis-for-Windows 3.0 build.
                                 It speaks the commands the services use, but it is NOT
                                 the Redis version the product is validated against
                                 (Redis 7.4 in Docker). Use it for development only.

    State lives in .data/ (gitignored): pg17/ (cluster), native/ (signing key, logs, pids).

    Ports: PostgreSQL 55432, Redis 56379 (same as compose.yaml, so the integration tests
    find them), API 18080, signaling 18081, relay 13340 (+ status 19090).

.EXAMPLE
    .\infra\local\native-stack.ps1 -Action Up
    .\infra\local\native-stack.ps1 -Action Status
    .\infra\local\native-stack.ps1 -Action Down
#>
[CmdletBinding()]
param(
    [ValidateSet('Up', 'Down', 'Status', 'Databases')]
    [string] $Action = 'Status',
    [string] $BinDir,
    # Relay URL announced to clients (the public tunnel address when testing over the internet).
    [string] $RelayUrl = 'http://127.0.0.1:13340'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Data = Join-Path $Repo '.data'
$State = Join-Path $Data 'native'
$Logs = Join-Path $State 'logs'
$PgBin = Join-Path $Repo '.devtools\pg17\pgsql\bin'
$RedisDir = Join-Path $Repo '.devtools\redis3'
$Bin = if ($BinDir) { (Resolve-Path $BinDir).Path } else { Join-Path $Repo 'target\release' }
$PgData = Join-Path $Data 'pg17'
$KeyFile = Join-Path $State 'token-signing.key'
$DbUrl = 'postgres://aionix:aionix-dev@127.0.0.1:55432/aionix'
$RedisUrl = 'redis://127.0.0.1:56379'

function Write-Step([string] $t) { Write-Host "==> $t" -ForegroundColor Cyan }

function Test-Port([int] $Port) {
    $c = [Net.Sockets.TcpClient]::new()
    try { $c.ConnectAsync('127.0.0.1', $Port).Wait(300) -and $c.Connected } catch { $false } finally { $c.Dispose() }
}

function Wait-Port([int] $Port, [int] $Seconds, [string] $What) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while (-not (Test-Port $Port)) {
        if ((Get-Date) -gt $deadline) { throw "$What não respondeu na porta $Port" }
        Start-Sleep -Milliseconds 300
    }
}

function Start-Hidden([string] $Name, [string] $Exe, [string[]] $Arguments, [hashtable] $Env) {
    foreach ($k in $Env.Keys) { [Environment]::SetEnvironmentVariable($k, $Env[$k], 'Process') }
    $p = Start-Process -FilePath $Exe -ArgumentList $Arguments -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput (Join-Path $Logs "$Name.out.log") -RedirectStandardError (Join-Path $Logs "$Name.log")
    foreach ($k in $Env.Keys) { [Environment]::SetEnvironmentVariable($k, $null, 'Process') }
    Set-Content -Path (Join-Path $State "$Name.pid") -Value $p.Id
    $p
}

function Stop-Named([string] $Name) {
    $pidFile = Join-Path $State "$Name.pid"
    if (Test-Path $pidFile) {
        $id = [int](Get-Content $pidFile)
        Get-Process -Id $id -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
        Remove-Item $pidFile -Force
    }
}

function Start-Databases {
    New-Item -ItemType Directory -Force $Logs | Out-Null
    if (-not (Test-Path (Join-Path $PgBin 'postgres.exe'))) { throw "PostgreSQL não encontrado em $PgBin" }
    if (-not (Test-Path (Join-Path $PgData 'PG_VERSION'))) {
        Write-Step 'Criando cluster PostgreSQL'
        $pw = Join-Path $State 'pw.tmp'
        [IO.File]::WriteAllText($pw, 'aionix-dev')
        try { & (Join-Path $PgBin 'initdb.exe') -D $PgData -U aionix --pwfile=$pw -A scram-sha-256 -E UTF8 --locale=C | Out-Null }
        finally { Remove-Item $pw -Force }
    }
    if (-not (Test-Port 55432)) {
        Write-Step 'Iniciando PostgreSQL'
        Remove-Item (Join-Path $PgData 'postmaster.pid') -ErrorAction SilentlyContinue
        Start-Hidden 'postgres' (Join-Path $PgBin 'postgres.exe') @('-D', "`"$PgData`"", '-p', '55432', '-c', 'listen_addresses=127.0.0.1', '-c', 'max_connections=300') @{} | Out-Null
        Wait-Port 55432 30 'PostgreSQL'
    }
    if (-not (Test-Port 56379)) {
        Write-Step 'Iniciando Redis (desenvolvimento)'
        Start-Hidden 'redis' (Join-Path $RedisDir 'redis-server.exe') @('--port', '56379', '--bind', '127.0.0.1') @{} | Out-Null
        Wait-Port 56379 15 'Redis'
    }
    $env:PGPASSWORD = 'aionix-dev'
    $exists = & (Join-Path $PgBin 'psql.exe') -h 127.0.0.1 -p 55432 -U aionix -d postgres -Atc "SELECT 1 FROM pg_database WHERE datname='aionix'"
    if (-not $exists) { & (Join-Path $PgBin 'psql.exe') -h 127.0.0.1 -p 55432 -U aionix -d postgres -c 'CREATE DATABASE aionix' | Out-Null }
}

function Start-Stack {
    Start-Databases
    foreach ($b in 'aionix-api', 'aionix-admin', 'aionix-signaling', 'aionix-relay') {
        if (-not (Test-Path (Join-Path $Bin "$b.exe"))) { throw "$b.exe não encontrado em $Bin (compile antes)" }
    }
    $admin = Join-Path $Bin 'aionix-admin.exe'
    $env:AIONIX_DATABASE_URL = $DbUrl
    if (-not (Test-Path $KeyFile)) { & $admin keygen --out $KeyFile | Out-Null }
    Write-Step 'Migrações'
    & $admin migrate
    if ($LASTEXITCODE -ne 0) { throw 'migrações falharam' }
    $pub = (& $admin public-key --key-file $KeyFile | Select-Object -Last 1).Trim()
    # Requests may arrive through the local Caddy (public tunnel): trust its forwarded headers.
    $common = @{ AIONIX_DATABASE_URL = $DbUrl; AIONIX_REDIS_URL = $RedisUrl; AIONIX_REDIS_PREFIX = 'aionix'; AIONIX_LOG = 'info,sqlx=warn'; AIONIX_TRUSTED_PROXIES = '127.0.0.1/32' }
    if (-not (Test-Port 18080)) {
        Write-Step 'Iniciando aionix-api'
        $e = $common.Clone(); $e.AIONIX_API_BIND = '127.0.0.1:18080'; $e.AIONIX_TOKEN_SIGNING_KEY_FILE = $KeyFile
        $e.AIONIX_RELAY_URLS = $RelayUrl; $e.AIONIX_SERVER_NAME = 'Aionix Connect (dev nativo)'; $e.AIONIX_ALLOW_QUICK_SUPPORT = 'true'
        Start-Hidden 'aionix-api' (Join-Path $Bin 'aionix-api.exe') @() $e | Out-Null
    }
    if (-not (Test-Port 18081)) {
        Write-Step 'Iniciando aionix-signaling'
        $e = $common.Clone(); $e.AIONIX_SIGNALING_BIND = '127.0.0.1:18081'; $e.AIONIX_TOKEN_PUBLIC_KEYS = $pub
        Start-Hidden 'aionix-signaling' (Join-Path $Bin 'aionix-signaling.exe') @() $e | Out-Null
    }
    if (-not (Test-Port 13340)) {
        Write-Step 'Iniciando aionix-relay'
        $e = @{ AIONIX_RELAY_BIND = '127.0.0.1:13340'; AIONIX_RELAY_STATUS_BIND = '127.0.0.1:19090'; AIONIX_TOKEN_PUBLIC_KEYS = $pub; AIONIX_REDIS_URL = $RedisUrl; AIONIX_REDIS_PREFIX = 'aionix'; AIONIX_LOG = 'info' }
        Start-Hidden 'aionix-relay' (Join-Path $Bin 'aionix-relay.exe') @() $e | Out-Null
    }
    foreach ($probe in @(@(18080, 'API'), @(18081, 'signaling'), @(19090, 'relay'))) { Wait-Port $probe[0] 30 $probe[1] }
    Show-Status
}

function Show-Status {
    foreach ($u in 'http://127.0.0.1:18080/readyz', 'http://127.0.0.1:18081/readyz', 'http://127.0.0.1:19090/readyz') {
        try { $r = Invoke-WebRequest $u -UseBasicParsing -TimeoutSec 5; Write-Host ("{0,-36} {1}" -f $u, $r.StatusCode) }
        catch { Write-Host ("{0,-36} indisponível" -f $u) -ForegroundColor Yellow }
    }
}

try {
    switch ($Action) {
        'Up' { Start-Stack }
        'Databases' { Start-Databases }
        'Status' { Show-Status }
        'Down' { foreach ($n in 'cloudflared', 'caddy', 'aionix-relay', 'aionix-signaling', 'aionix-api', 'redis', 'postgres') { Stop-Named $n }; Write-Host 'parado' }
    }
}
catch {
    Write-Host "ERRO: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
