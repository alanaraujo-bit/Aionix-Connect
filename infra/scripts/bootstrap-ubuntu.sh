#!/usr/bin/env bash
# Prepares a fresh Ubuntu Server 24.04.x (bare metal) for Aionix Connect.
#
#   sudo bash bootstrap-ubuntu.sh [--no-upgrade] [--configure-firewall] [--operator USER]
#                                 [--install-backup-cron]
#
# Safe to run more than once. It never edits SSH configuration, never disables
# password/key login and never removes packages. The firewall is only changed with
# --configure-firewall, and then OpenSSH is allowed *before* the firewall is enabled.

set -euo pipefail

UPGRADE=1
FIREWALL=0
BACKUP_CRON=0
OPERATOR="${SUDO_USER:-}"
ROOT=/opt/aionix-connect

while [ $# -gt 0 ]; do
    case "$1" in
        --no-upgrade) UPGRADE=0 ;;
        --configure-firewall) FIREWALL=1 ;;
        --install-backup-cron) BACKUP_CRON=1 ;;
        --operator) OPERATOR="$2"; shift ;;
        -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
        *) echo "opção desconhecida: $1" >&2; exit 2 ;;
    esac
    shift
done

log() { printf '\n==> %s\n' "$*"; }

[ "$(id -u)" -eq 0 ] || { echo "execute como root (sudo)" >&2; exit 1; }
. /etc/os-release
if [ "${ID:-}" != "ubuntu" ] || [ "${VERSION_ID:-}" != "24.04" ]; then
    echo "AVISO: sistema detectado ${PRETTY_NAME:-desconhecido}; este script foi feito para Ubuntu 24.04." >&2
    read -r -p "Continuar mesmo assim? [s/N] " ans
    [ "$ans" = "s" ] || exit 1
fi

export DEBIAN_FRONTEND=noninteractive

log "Atualizando índices de pacotes"
apt-get update -y
if [ "$UPGRADE" -eq 1 ]; then
    log "Aplicando atualizações do sistema"
    apt-get -o Dpkg::Options::=--force-confold upgrade -y
fi

log "Instalando utilitários base"
apt-get install -y ca-certificates curl gnupg openssl jq python3 tar gzip dnsutils iproute2 ufw cron

if ! command -v docker >/dev/null 2>&1 || ! docker compose version >/dev/null 2>&1; then
    log "Instalando Docker Engine + Compose plugin (repositório oficial Docker)"
    install -m 0755 -d /etc/apt/keyrings
    curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
    chmod a+r /etc/apt/keyrings/docker.asc
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/ubuntu ${VERSION_CODENAME} stable" \
        > /etc/apt/sources.list.d/docker.list
    apt-get update -y
    apt-get install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
else
    log "Docker já instalado: $(docker --version)"
fi

if [ ! -f /etc/docker/daemon.json ]; then
    log "Configurando rotação de logs do Docker (/etc/docker/daemon.json)"
    mkdir -p /etc/docker
    cat > /etc/docker/daemon.json <<'JSON'
{
  "log-driver": "json-file",
  "log-opts": { "max-size": "20m", "max-file": "5" },
  "live-restore": true
}
JSON
    systemctl restart docker
else
    log "/etc/docker/daemon.json já existe; mantido sem alterações"
fi
systemctl enable --now docker

log "Criando grupo de sistema 'aionix'"
getent group aionix >/dev/null || groupadd --system aionix
if [ -n "$OPERATOR" ] && id "$OPERATOR" >/dev/null 2>&1; then
    usermod -aG aionix,docker "$OPERATOR"
    echo "Usuário operador '$OPERATOR' adicionado aos grupos aionix e docker (vale no próximo login)."
fi

log "Criando estrutura em $ROOT"
install -d -m 0755 "$ROOT"
install -d -m 0750 -g aionix "$ROOT/compose" "$ROOT/scripts" "$ROOT/releases" "$ROOT/logs" "$ROOT/logs/caddy"
install -d -m 0750 -g aionix "$ROOT/config" "$ROOT/config/secrets"
install -d -m 0750 -g aionix "$ROOT/backups"
install -d -m 0750 "$ROOT/data"
# postgres/caddy/prometheus manage their own ownership inside these directories.
install -d -m 0700 "$ROOT/data/postgres" "$ROOT/data/caddy" "$ROOT/data/prometheus"
if [ -n "$OPERATOR" ]; then
    chown "$OPERATOR":aionix "$ROOT/compose" "$ROOT/scripts" "$ROOT/releases" "$ROOT/logs" \
        "$ROOT/logs/caddy" "$ROOT/config" "$ROOT/config/secrets" "$ROOT/backups"
fi
# The signing key itself will be owned by uid 65532 (the container user), group
# aionix, mode 0440 — see scripts/lib.sh secure_key_file.

if [ "$FIREWALL" -eq 1 ]; then
    log "Configurando UFW (SSH liberado ANTES de ativar)"
    ufw allow OpenSSH
    ufw allow 80/tcp
    ufw allow 443/tcp
    ufw allow 443/udp
    ufw --force enable
    ufw status verbose
else
    cat <<'TXT'

Firewall não alterado. Portas necessárias para o Aionix Connect:
  22/tcp   SSH (mantenha liberado!)
  80/tcp   HTTP (desafio ACME / redirecionamento)
  443/tcp  HTTPS (API, signaling, relay)
  443/udp  HTTP/3 (opcional)
Para aplicar com UFW:  sudo bash bootstrap-ubuntu.sh --configure-firewall --no-upgrade
TXT
fi

if [ "$BACKUP_CRON" -eq 1 ]; then
    log "Agendando backup diário (03:15) em /etc/cron.d/aionix-backup"
    cat > /etc/cron.d/aionix-backup <<CRON
15 3 * * * root /bin/bash $ROOT/scripts/backup.sh >> $ROOT/logs/backup.log 2>&1
CRON
    chmod 0644 /etc/cron.d/aionix-backup
fi

log "Concluído"
echo "Próximo passo: na estação Windows, execute infra/deploy-server.ps1 apontando para este servidor."
