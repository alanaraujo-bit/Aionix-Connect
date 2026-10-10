<#
.SYNOPSIS
    Deploys Aionix Connect Server from this Windows workstation to the Ubuntu server over SSH.

.DESCRIPTION
    1. verifies SSH connectivity (key-based, non-interactive)
    2. verifies the target OS (Ubuntu 24.04)
    3. verifies Docker Engine + Compose plugin and the /opt/aionix-connect layout
    4. packages the committed source (git archive HEAD) and uploads it as a release
    5. on the server (infra/scripts/deploy-remote.sh): keeps or generates configuration,
       builds the image, backs up, optionally restores a staging backup, runs
       migrations, starts the stack, waits for health checks and prints diagnostics

    The server must have been prepared once with infra/scripts/bootstrap-ubuntu.sh.
    When the server has the ops layer (infra/ops/install-ops.sh), every remote step that
    changes something runs through `aionix-run`: it appears live in the tmux session
    aionix-setup and in /var/log/aionix-ops/ops.log (disable with -NoAudit).
    Nothing is deleted on the server; previous releases stay in releases/.

.EXAMPLE
    # First deployment (generates config with a real domain and automatic HTTPS)
    .\infra\deploy-server.ps1 -Server aionix@192.168.1.10 -Domain connect.example.com -AcmeEmail ops@example.com

.EXAMPLE
    # Move the Windows staging data (database + signing key) to the new server
    .\infra\local\staging.ps1 -Action Backup
    .\infra\deploy-server.ps1 -Server aionix@192.168.1.10 -Domain connect.example.com -AcmeEmail ops@example.com `
        -RestoreBackup .\infra\local\.staging\backups\aionix-backup-20261007T120000Z.tar.gz

.EXAMPLE
    # Show every step without touching anything (works without a reachable server)
    .\infra\deploy-server.ps1 -Server aionix@server.lan -Domain connect.example.com -DryRun
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $Server,
    [int] $Port = 22,
    [string] $IdentityFile,
    [string] $RemoteRoot = '/opt/aionix-connect',
    [string] $Version,
    [string] $Domain,
    [string] $AcmeEmail,
    [string] $Site,
    [string] $PublicUrl,
    [string] $RestoreBackup,
    [switch] $SkipBackup,
    [switch] $AllowOtherOs,
    [switch] $AllowDirty,
    [int] $HealthTimeoutSeconds = 300,
    [switch] $DryRun,
    [switch] $NoAudit,
    # Publish Caddy (80/443) on this IPv4 only (e.g. the LAN address); kept in .env.
    [string] $BindAddress
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

function Write-Step([string] $Text) { Write-Host "`n==> $Text" -ForegroundColor Cyan }
function Fail([string] $Text) { Write-Host "ERRO: $Text" -ForegroundColor Red; exit 1 }

# Arguments forwarded to the server are restricted to a safe character set, so they
# can be placed on the remote command line without shell injection.
function Assert-Safe([string] $Name, [string] $Value) {
    if ($Value -and $Value -notmatch '^[A-Za-z0-9._:/@+=-]+$') { Fail "$Name contém caracteres não permitidos: '$Value'" }
}
foreach ($p in @{ Server = $Server; RemoteRoot = $RemoteRoot; Version = $Version; Domain = $Domain;
                  AcmeEmail = $AcmeEmail; Site = $Site; PublicUrl = $PublicUrl; BindAddress = $BindAddress }.GetEnumerator()) {
    Assert-Safe $p.Key $p.Value
}

$SshBase = @('-p', "$Port", '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', '-o', 'StrictHostKeyChecking=accept-new')
$ScpBase = @('-P', "$Port", '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', '-o', 'StrictHostKeyChecking=accept-new')
if ($IdentityFile) { $SshBase += @('-i', $IdentityFile); $ScpBase += @('-i', $IdentityFile) }

# Set after the SSH check when the server has aionix-run (ops audit log).
$Audit = $false

function Invoke-Ssh([string] $Command, [switch] $Capture, [string] $Label) {
    if ($Label -and $Audit -and -not $DryRun) {
        # The command travels on stdin (no quoting through the remote shell); aionix-run
        # runs it as this SSH user and records it in the audit log.
        $Command | & ssh @SshBase $Server "aionix-run --user 'Deploy ${Version}: $Label'"
        if ($LASTEXITCODE -ne 0) { throw "comando remoto falhou ($LASTEXITCODE): $Label" }
        return
    }
    if ($DryRun) {
        Write-Host "  [dry-run] ssh $Server $Command" -ForegroundColor DarkGray
        if ($Capture) { return '' } else { return }
    }
    if ($Capture) {
        $out = & ssh @SshBase $Server $Command 2>&1
        if ($LASTEXITCODE -ne 0) { throw "ssh falhou ($LASTEXITCODE): $out" }
        return ($out -join "`n")
    }
    & ssh @SshBase $Server $Command
    if ($LASTEXITCODE -ne 0) { throw "comando remoto falhou ($LASTEXITCODE): $Command" }
}

function Send-File([string] $Local, [string] $Remote) {
    if ($DryRun) { Write-Host "  [dry-run] scp $Local ${Server}:$Remote" -ForegroundColor DarkGray; return }
    & scp @ScpBase $Local "${Server}:$Remote"
    if ($LASTEXITCODE -ne 0) { throw "falha no envio de $Local" }
}

# --- 0. Local preflight ---------------------------------------------------------------
Write-Step 'Verificações locais'
foreach ($tool in 'ssh', 'scp', 'git') {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { Fail "$tool não encontrado no PATH" }
}
Push-Location $RepoRoot
try {
    $dirty = (& git status --porcelain)
    if ($dirty -and -not $AllowDirty) {
        Fail 'há alterações não commitadas; o deploy usa o último commit (use -AllowDirty para ignorar)'
    }
    if (-not $Version) {
        $sha = (& git rev-parse --short=12 HEAD).Trim()
        $Version = "$(Get-Date -AsUTC -Format 'yyyyMMdd.HHmm')-$sha"
    }
} finally { Pop-Location }
Assert-Safe 'Version' $Version
if ($RestoreBackup -and -not (Test-Path $RestoreBackup)) { Fail "backup não encontrado: $RestoreBackup" }
Write-Host "  versão: $Version"
Write-Host "  destino: $Server ($RemoteRoot)"

try {
    # --- 1. SSH ---------------------------------------------------------------------------
    Write-Step 'Conectividade SSH'
    $hello = Invoke-Ssh 'echo aionix-ok' -Capture
    if (-not $DryRun -and $hello -notmatch 'aionix-ok') { Fail "resposta inesperada do SSH: $hello" }
    Write-Host '  OK'
    if (-not $NoAudit -and -not $DryRun) {
        $Audit = (Invoke-Ssh 'command -v aionix-run >/dev/null && sudo -n true 2>/dev/null && echo audit-on || echo audit-off' -Capture) -match 'audit-on'
        Write-Host ("  log de auditoria (aionix-run): " + $(if ($Audit) { 'ativo' } else { 'indisponível' }))
    }

    # --- 2. OS ----------------------------------------------------------------------------
    Write-Step 'Sistema operacional do servidor'
    $os = Invoke-Ssh '. /etc/os-release && echo "$ID $VERSION_ID $PRETTY_NAME"' -Capture
    if (-not $DryRun) {
        Write-Host "  $os"
        if ($os -notmatch '^ubuntu 24\.04') {
            if ($AllowOtherOs) { Write-Warning 'sistema diferente de Ubuntu 24.04 (prosseguindo por -AllowOtherOs)' }
            else { Fail 'o servidor deve ser Ubuntu 24.04 (use -AllowOtherOs para forçar)' }
        }
    }

    # --- 3. Docker + layout ----------------------------------------------------------------
    Write-Step 'Docker e estrutura de diretórios'
    $docker = Invoke-Ssh "docker version --format '{{.Server.Version}}' && docker compose version --short && test -d $RemoteRoot/releases && test -w $RemoteRoot/releases && echo layout-ok" -Capture
    if (-not $DryRun) {
        if ($docker -notmatch 'layout-ok') { Fail "Docker ou $RemoteRoot ausente/sem permissão: execute infra/scripts/bootstrap-ubuntu.sh no servidor.`n$docker" }
        Write-Host ($docker -replace 'layout-ok', '')
    }

    # --- 4. Package + upload ------------------------------------------------------------------
    Write-Step 'Empacotando e enviando a versão'
    $tmp = Join-Path ([IO.Path]::GetTempPath()) "aionix-$Version.tar.gz"
    if (-not $DryRun) {
        Push-Location $RepoRoot
        try {
            & git archive --format=tar.gz --prefix=src/ -o $tmp HEAD
            if ($LASTEXITCODE -ne 0) { Fail 'git archive falhou' }
        } finally { Pop-Location }
        Write-Host ("  pacote: {0:N1} MB" -f ((Get-Item $tmp).Length / 1MB))
    }
    $remoteRelease = "$RemoteRoot/releases/$Version"
    Invoke-Ssh "mkdir -p $remoteRelease" -Label 'criar diretório da versão'
    Send-File $tmp "$remoteRelease/source.tar.gz"
    Invoke-Ssh "ls -l $remoteRelease/source.tar.gz && sha256sum $remoteRelease/source.tar.gz && tar -xzf $remoteRelease/source.tar.gz -C $remoteRelease && rm -f $remoteRelease/source.tar.gz" -Label 'extrair código-fonte enviado'
    if (-not $DryRun) { Remove-Item $tmp -ErrorAction SilentlyContinue }

    $remoteArgs = @('--version', $Version, '--health-timeout', "$HealthTimeoutSeconds")
    if ($Domain) { $remoteArgs += @('--domain', $Domain) }
    if ($AcmeEmail) { $remoteArgs += @('--email', $AcmeEmail) }
    if ($Site) { $remoteArgs += @('--site', $Site) }
    if ($PublicUrl) { $remoteArgs += @('--public-url', $PublicUrl) }
    if ($SkipBackup) { $remoteArgs += '--skip-backup' }
    if ($BindAddress) { $remoteArgs += @('--bind', $BindAddress) }
    if ($RestoreBackup) {
        Write-Step 'Enviando backup para restauração'
        $name = Split-Path $RestoreBackup -Leaf
        Assert-Safe 'RestoreBackup' $name
        Invoke-Ssh "mkdir -p $RemoteRoot/backups/incoming" -Label 'preparar recebimento do backup'
        Send-File $RestoreBackup "$RemoteRoot/backups/incoming/$name"
        $remoteArgs += @('--restore', "$RemoteRoot/backups/incoming/$name")
    }

    # --- 5. Remote deployment -----------------------------------------------------------------
    Write-Step 'Executando o deploy no servidor'
    Invoke-Ssh ("bash $remoteRelease/src/infra/scripts/deploy-remote.sh " + ($remoteArgs -join ' ')) -Label 'deploy-remote.sh (config, build, migrações, start, health)'

    Write-Step 'Deploy concluído'
    Write-Host "  versão $Version ativa em $Server"
    Write-Host "  diagnóstico a qualquer momento: ssh $Server bash $RemoteRoot/scripts/aionix-diagnostico.sh"
    if ($DryRun) { Write-Host '  (dry-run: nada foi executado)' -ForegroundColor Yellow }
}
catch {
    Write-Host "`nFALHA: $($_.Exception.Message)" -ForegroundColor Red
    Write-Host 'Nenhum dado foi apagado. Versões anteriores permanecem em releases/ e os backups em backups/.'
    exit 1
}
