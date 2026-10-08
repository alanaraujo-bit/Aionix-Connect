<#
.SYNOPSIS
    Runs the full Aionix Connect Server stack on this Windows workstation (Docker Desktop /
    WSL2) as the temporary DEVELOPMENT / STAGING server.

.DESCRIPTION
    Uses exactly the production Compose file (infra/compose/docker-compose.yml); only the
    env file differs. State lives in infra/local/.staging (gitignored):
      config/.env, config/secrets/token-signing.key, data/postgres, data/caddy, logs/, backups/

    Actions:
      Init     create config + secrets (random passwords, new signing key); never overwrites
      Up       build the image, derive the public key, run migrations, start, wait for health
      Status   container health + readiness of each service
      Down     stop the stack (data is kept)
      Backup   archive identical in format to infra/scripts/backup.sh (database + key + env),
               restorable on Ubuntu with restore.sh / deploy-server.ps1 -RestoreBackup
      Restore  restore such an archive here (-BackupFile). The signing key is always
               restored (enrolled devices pinned its public key); -WithConfig also
               restores .env (same-host disaster recovery). A safety backup comes first.
      Destroy  disaster simulation for restore tests: removes the containers and the
               database volume and moves config/secrets aside (asks unless -Yes)
      Logs     follow the logs of the server services
      Admin    run aionix-admin with the remaining arguments

.EXAMPLE
    .\infra\local\staging.ps1 -Action Init
    .\infra\local\staging.ps1 -Action Up
    .\infra\local\staging.ps1 -Action Admin create-org --name "Suporte Exemplo"
#>
[CmdletBinding()]
param(
    [ValidateSet('Init', 'Up', 'Status', 'Down', 'Backup', 'Restore', 'Destroy', 'Logs', 'Admin')]
    [string] $Action = 'Status',
    [int] $HttpPort = 8080,
    [string] $PublicHost,
    [string] $BackupFile,
    [switch] $WithConfig,
    [switch] $Yes,
    [Parameter(ValueFromRemainingArguments)] [string[]] $Rest
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$State = Join-Path $PSScriptRoot '.staging'
$Config = Join-Path $State 'config'
$EnvFile = Join-Path $Config '.env'
$KeyFile = Join-Path $Config 'secrets\token-signing.key'
$ComposeFile = Join-Path $Repo 'infra\compose\docker-compose.yml'

function Fwd([string] $p) { $p -replace '\\', '/' }
function Write-Step([string] $t) { Write-Host "`n==> $t" -ForegroundColor Cyan }

function New-Secret {
    $bytes = [byte[]]::new(32)
    [System.Security.Cryptography.RandomNumberGenerator]::Fill($bytes)
    [Convert]::ToBase64String($bytes).TrimEnd('=').Replace('+', '-').Replace('/', '_')
}

function Write-Lf([string] $Path, [string] $Text) {
    [IO.File]::WriteAllText($Path, ($Text -replace "`r`n", "`n"), [Text.UTF8Encoding]::new($false))
}

function Get-EnvValue([string] $Key) {
    $line = Get-Content $EnvFile | Where-Object { $_ -match "^$Key=" } | Select-Object -Last 1
    if ($line) { $line.Substring($Key.Length + 1) } else { '' }
}

function Set-EnvValue([string] $Key, [string] $Value) {
    $lines = @(Get-Content $EnvFile | Where-Object { $_ -notmatch "^$Key=" })
    $lines += "$Key=$Value"
    Write-Lf $EnvFile (($lines -join "`n") + "`n")
}

function Invoke-Compose {
    param([Parameter(ValueFromRemainingArguments)] [string[]] $ComposeArgs)
    & docker compose -f $ComposeFile --env-file $EnvFile @ComposeArgs
    if ($LASTEXITCODE -ne 0) { throw "docker compose $($ComposeArgs -join ' ') falhou ($LASTEXITCODE)" }
}

function Assert-Docker {
    if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
        throw 'Docker não encontrado. Instale o Docker Desktop (backend WSL2) e reabra o terminal.'
    }
    & docker info --format '{{.ServerVersion}}' *> $null
    if ($LASTEXITCODE -ne 0) { throw 'Docker Desktop não está em execução.' }
}

function Initialize-Staging {
    Write-Step 'Inicializando configuração de staging'
    foreach ($d in 'config\secrets', 'data\postgres', 'data\caddy', 'data\prometheus', 'logs\caddy', 'backups') {
        New-Item -ItemType Directory -Force (Join-Path $State $d) | Out-Null
    }
    if (-not (Test-Path $KeyFile)) {
        Write-Lf $KeyFile ((New-Secret) + "`n")
        Write-Host "  chave de assinatura criada: $KeyFile"
    } else { Write-Host '  chave de assinatura existente mantida' }
    if (Test-Path $EnvFile) { Write-Host "  $EnvFile já existe; mantido"; return }
    if (-not $PublicHost) {
        $PublicHost = (Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue |
            Where-Object { $_.PrefixOrigin -in 'Dhcp', 'Manual' -and $_.IPAddress -notlike '169.254*' } |
            Sort-Object InterfaceMetric | Select-Object -First 1).IPAddress
        if (-not $PublicHost) { $PublicHost = 'localhost' }
    }
    $envText = @"
# Aionix Connect — DEVELOPMENT / STAGING on Windows Docker Desktop (not production).
AIONIX_VERSION=staging
AIONIX_SOURCE_DIR=$(Fwd $Repo)
AIONIX_CONFIG_DIR=$(Fwd $Config)
AIONIX_DATA_DIR=$(Fwd (Join-Path $State 'data'))
AIONIX_POSTGRES_DATA=aionix-postgres
AIONIX_LOG_DIR=$(Fwd (Join-Path $State 'logs'))
AIONIX_SITE_ADDRESS=:80
ACME_EMAIL=
AIONIX_HTTP_PORT=$HttpPort
AIONIX_HTTPS_PORT=8443
AIONIX_RELAY_URLS=http://${PublicHost}:$HttpPort
AIONIX_SERVER_NAME=Aionix Connect (staging)
POSTGRES_PASSWORD=$(New-Secret)
REDIS_PASSWORD=$(New-Secret)
AIONIX_TOKEN_PUBLIC_KEYS=
AIONIX_TOKEN_PREVIOUS_PUBLIC_KEYS=
AIONIX_REDIS_PREFIX=aionix
AIONIX_ALLOW_QUICK_SUPPORT=true
AIONIX_OFFER_TIMEOUT_SECS=15
AIONIX_RELAY_CLIENT_RX_BYTES_PER_SEC=0
AIONIX_RELAY_CONNECT_LIMIT_PER_MINUTE=120
AIONIX_RELAY_MAX_CONNECTIONS=10000
AIONIX_LOG=info,sqlx=warn
AIONIX_TRUSTED_PROXIES=172.16.0.0/12,10.0.0.0/8,192.168.0.0/16,127.0.0.1/32
AIONIX_BACKUP_KEEP=14
AIONIX_BACKUP_PASSPHRASE_FILE=
"@
    Write-Lf $EnvFile $envText
    Write-Host "  configuração criada: $EnvFile"
    Write-Host "  endpoint público: http://${PublicHost}:$HttpPort (API, signaling e relay)"
}

function Start-Staging {
    Assert-Docker
    if (-not (Test-Path $EnvFile)) { Initialize-Staging }
    Write-Step 'Build da imagem do servidor'
    $env:DOCKER_BUILDKIT = '1'
    Invoke-Compose build aionix-api
    if (-not (Get-EnvValue 'AIONIX_TOKEN_PUBLIC_KEYS')) {
        Write-Step 'Derivando a chave pública de assinatura'
        $pk = (& docker compose -f $ComposeFile --env-file $EnvFile --profile tools run --rm --no-deps -T aionix-admin `
                aionix-admin public-key --key-file /run/secrets/token_signing_key | Select-Object -Last 1).Trim()
        if (-not $pk) { throw 'não foi possível derivar a chave pública' }
        Set-EnvValue 'AIONIX_TOKEN_PUBLIC_KEYS' $pk
        Write-Host "  AIONIX_TOKEN_PUBLIC_KEYS=$pk"
    }
    Write-Step 'Migrações'
    Invoke-Compose up -d postgres redis
    Invoke-Compose run --rm aionix-migrate
    Write-Step 'Iniciando serviços'
    Invoke-Compose up -d --remove-orphans
    Write-Step 'Aguardando health checks'
    $deadline = (Get-Date).AddMinutes(5)
    do {
        Start-Sleep -Seconds 3
        $states = & docker compose -f $ComposeFile --env-file $EnvFile ps --format '{{.Service}}={{.Health}}'
        $healthy = @('aionix-api', 'aionix-signaling', 'aionix-relay', 'postgres', 'redis') |
            Where-Object { $states -contains "$_=healthy" }
    } until ($healthy.Count -eq 5 -or (Get-Date) -gt $deadline)
    $states | ForEach-Object { Write-Host "  $_" }
    if ($healthy.Count -ne 5) { throw 'serviços não ficaram saudáveis em 5 minutos (veja: staging.ps1 -Action Logs)' }
    $port = Get-EnvValue 'AIONIX_HTTP_PORT'
    $r = Invoke-WebRequest -Uri "http://127.0.0.1:$port/healthz" -UseBasicParsing -TimeoutSec 10
    Write-Host "  GET /healthz via Caddy: $($r.StatusCode)"
    Write-Host "`nStaging ativo em http://127.0.0.1:$port (DESENVOLVIMENTO/STAGING, não produção)." -ForegroundColor Green
}

function Show-Status {
    Assert-Docker
    Invoke-Compose ps
    foreach ($probe in @(('aionix-api', 'http://127.0.0.1:8080/readyz'), ('aionix-signaling', 'http://127.0.0.1:8081/readyz'), ('aionix-relay', 'http://127.0.0.1:9090/readyz'))) {
        & docker compose -f $ComposeFile --env-file $EnvFile exec -T $probe[0] aionix-healthcheck $probe[1] --print
        Write-Host ("{0,-18} {1}" -f $probe[0], $(if ($LASTEXITCODE -eq 0) { 'pronto' } else { 'NÃO pronto' }))
    }
    & docker compose -f $ComposeFile --env-file $EnvFile exec -T aionix-api aionix-admin status
}

function Backup-Staging {
    Assert-Docker
    $ts = (Get-Date).ToUniversalTime().ToString("yyyyMMdd'T'HHmmss'Z'")
    $name = "aionix-backup-$ts"
    $work = Join-Path ([IO.Path]::GetTempPath()) $name
    $out = Join-Path $work $name
    New-Item -ItemType Directory -Force (Join-Path $out 'config\secrets') | Out-Null
    try {
        Write-Step 'Exportando PostgreSQL'
        # Binary dump copied with `docker compose cp` (PowerShell pipes would corrupt it).
        Invoke-Compose exec -T postgres sh -c 'pg_dump -U aionix -d aionix --format=custom --no-owner --no-privileges -f /tmp/aionix.dump'
        Invoke-Compose cp postgres:/tmp/aionix.dump (Join-Path $out 'postgres.dump')
        Invoke-Compose exec -T postgres rm -f /tmp/aionix.dump
        $migrations = (& docker compose -f $ComposeFile --env-file $EnvFile exec -T postgres psql -U aionix -d aionix -At -c "SELECT string_agg(version::text, ',' ORDER BY version) FROM _sqlx_migrations WHERE success").Trim()
        Copy-Item $EnvFile (Join-Path $out 'config\.env')
        Copy-Item $KeyFile (Join-Path $out 'config\secrets\token-signing.key')
        $files = Get-ChildItem $out -Recurse -File | Sort-Object FullName
        $lines = @('format=aionix-backup/1', "created_at=$ts", "host=$env:COMPUTERNAME", "version=$(Get-EnvValue 'AIONIX_VERSION')", "migrations=$migrations")
        foreach ($f in $files) {
            $rel = './' + ($f.FullName.Substring($out.Length + 1) -replace '\\', '/')
            $lines += "$((Get-FileHash $f.FullName -Algorithm SHA256).Hash.ToLower())  $rel"
        }
        Write-Lf (Join-Path $out 'MANIFEST') (($lines -join "`n") + "`n")
        $dest = Join-Path $State 'backups'
        New-Item -ItemType Directory -Force $dest | Out-Null
        $archive = Join-Path $dest "$name.tar.gz"
        & tar.exe -C $work -czf $archive $name
        if ($LASTEXITCODE -ne 0) { throw 'tar falhou' }
        $script:LastBackup = $archive
        Write-Host "`nBackup: $archive" -ForegroundColor Green
        Write-Host 'Contém a chave de assinatura e as senhas: guarde-o com cuidado.'
        Write-Host 'Migrar para o Ubuntu: infra\deploy-server.ps1 -Server usuario@host -Domain ... -RestoreBackup <arquivo>'
    } finally {
        Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    }
}

function Test-PostgresRunning {
    if (-not (Test-Path $EnvFile)) { return $false }
    $running = & docker compose -f $ComposeFile --env-file $EnvFile ps --status running --services 2>$null
    return ($running -contains 'postgres')
}

function Confirm-Action([string] $Question) {
    if ($Yes) { return }
    $answer = Read-Host "$Question [s/N]"
    if ($answer -ne 's') { throw 'cancelado' }
}

function Restore-Staging {
    Assert-Docker
    if (-not $BackupFile -or -not (Test-Path $BackupFile)) { throw "arquivo de backup não encontrado: $BackupFile" }
    if ($BackupFile -like '*.enc') { throw 'backups criptografados: restaure no Ubuntu (restore.sh) ou decifre antes' }
    $work = Join-Path $State "restore-work-$([Guid]::NewGuid().ToString('N').Substring(0, 8))"
    New-Item -ItemType Directory -Force $work | Out-Null
    try {
        Write-Step 'Verificando o arquivo de backup'
        & tar.exe -C $work -xzf $BackupFile
        if ($LASTEXITCODE -ne 0) { throw 'não foi possível extrair o arquivo' }
        $dir = Get-ChildItem $work -Directory | Where-Object Name -like 'aionix-backup-*' | Select-Object -First 1
        if (-not $dir -or -not (Test-Path (Join-Path $dir.FullName 'MANIFEST'))) { throw 'arquivo não é um backup Aionix (MANIFEST ausente)' }
        $manifest = Get-Content (Join-Path $dir.FullName 'MANIFEST')
        if ($manifest -notcontains 'format=aionix-backup/1') { throw 'formato de backup desconhecido' }
        foreach ($line in $manifest | Where-Object { $_ -match '^[0-9a-f]{64}  ' }) {
            $hash, $rel = $line -split '  ', 2
            $path = Join-Path $dir.FullName ($rel.TrimStart('.', '/') -replace '/', '\')
            if (-not (Test-Path $path)) { throw "arquivo ausente no backup: $rel" }
            if ((Get-FileHash $path -Algorithm SHA256).Hash.ToLower() -ne $hash) { throw "falha de integridade: $rel" }
        }
        $manifest | Where-Object { $_ -match '^(created_at|host|version|migrations)=' } | ForEach-Object { Write-Host "  $_" }
        Confirm-Action 'O banco de dados atual será SUBSTITUÍDO. Continuar?'

        if (Test-PostgresRunning) {
            Write-Step 'Backup de segurança do estado atual'
            try { Backup-Staging } catch { Write-Host "  aviso: backup de segurança falhou: $($_.Exception.Message)" -ForegroundColor Yellow }
        }
        foreach ($d in 'config\secrets', 'backups') { New-Item -ItemType Directory -Force (Join-Path $State $d) | Out-Null }
        $stamp = Get-Date -Format 'yyyyMMddHHmmss'
        if ($WithConfig -or -not (Test-Path $EnvFile)) {
            if (Test-Path $EnvFile) { Copy-Item $EnvFile "$EnvFile.before-restore.$stamp" }
            Copy-Item (Join-Path $dir.FullName 'config\.env') $EnvFile -Force
            # Paths in the archive belong to the host that made it; keep this host's.
            Set-EnvValue 'AIONIX_SOURCE_DIR' (Fwd $Repo)
            Set-EnvValue 'AIONIX_CONFIG_DIR' (Fwd $Config)
            Set-EnvValue 'AIONIX_DATA_DIR' (Fwd (Join-Path $State 'data'))
            Set-EnvValue 'AIONIX_LOG_DIR' (Fwd (Join-Path $State 'logs'))
            Write-Host '  config/.env restaurado'
        }
        if (Test-Path $KeyFile) { Move-Item $KeyFile "$KeyFile.before-restore.$stamp" }
        Copy-Item (Join-Path $dir.FullName 'config\secrets\token-signing.key') $KeyFile
        # The public key in .env must match the restored signing key.
        Set-EnvValue 'AIONIX_TOKEN_PUBLIC_KEYS' ''
        Write-Host '  chave de assinatura restaurada'
        foreach ($d in 'data\caddy', 'data\prometheus', 'logs\caddy') { New-Item -ItemType Directory -Force (Join-Path $State $d) | Out-Null }

        Write-Step 'Parando serviços da aplicação'
        & docker compose -f $ComposeFile --env-file $EnvFile stop caddy aionix-api aionix-signaling aionix-relay *> $null
        Invoke-Compose up -d postgres redis
        $deadline = (Get-Date).AddMinutes(2)
        do {
            Start-Sleep -Seconds 2
            & docker compose -f $ComposeFile --env-file $EnvFile exec -T postgres pg_isready -U aionix -d postgres -h 127.0.0.1 *> $null
        } until ($LASTEXITCODE -eq 0 -or (Get-Date) -gt $deadline)

        Write-Step 'Restaurando PostgreSQL'
        Invoke-Compose exec -T postgres psql -U aionix -d postgres -v ON_ERROR_STOP=1 -c 'DROP DATABASE IF EXISTS aionix WITH (FORCE)' -c 'CREATE DATABASE aionix OWNER aionix'
        Invoke-Compose cp (Join-Path $dir.FullName 'postgres.dump') postgres:/tmp/aionix-restore.dump
        Invoke-Compose exec -T postgres pg_restore -U aionix -d aionix --no-owner --no-privileges --exit-on-error /tmp/aionix-restore.dump
        Invoke-Compose exec -T postgres rm -f /tmp/aionix-restore.dump
        Write-Host "`nBanco restaurado; iniciando a pilha..." -ForegroundColor Green
    } finally {
        Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    }
    Start-Staging
}

function Destroy-Staging {
    Assert-Docker
    Confirm-Action 'Remover containers, o volume do PostgreSQL e afastar config/segredos (simulação de desastre)?'
    if (Test-Path $EnvFile) {
        Write-Step 'Removendo containers e o volume do banco'
        & docker compose -f $ComposeFile --env-file $EnvFile --profile tools --profile monitoring down -v --remove-orphans
    }
    $aside = Join-Path $State "destroyed-$(Get-Date -Format 'yyyyMMddHHmmss')"
    if (Test-Path $Config) {
        Move-Item $Config $aside
        Write-Host "  config e segredos afastados para $aside"
    }
    Write-Host 'Instância destruída (os backups em .staging\backups foram mantidos).' -ForegroundColor Yellow
}

try {
    switch ($Action) {
        'Init' { Initialize-Staging }
        'Up' { Start-Staging }
        'Status' { Show-Status }
        'Down' { Assert-Docker; Invoke-Compose down }
        'Backup' { Backup-Staging }
        'Restore' { Restore-Staging }
        'Destroy' { Destroy-Staging }
        'Logs' { Assert-Docker; Invoke-Compose logs -f --tail 100 aionix-api aionix-signaling aionix-relay }
        'Admin' { Assert-Docker; Invoke-Compose --profile tools run --rm aionix-admin aionix-admin @Rest }
    }
}
catch {
    Write-Host "ERRO: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
