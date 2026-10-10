#!/usr/bin/env bash
# Server-side half of a deployment. Runs from the freshly extracted release:
#   /opt/aionix-connect/releases/<version>/src/infra/scripts/deploy-remote.sh
# Invoked by infra/deploy-server.ps1 (Windows) over SSH; can also be run by hand.
#
#   deploy-remote.sh --version V [--domain D --email E | --site S --public-url U]
#                    [--restore ARCHIVE] [--skip-backup] [--health-timeout SECS]
#                    [--bind IPV4]   publish Caddy (80/443) on this address only
#
# Steps: install compose/scripts → ensure config + key → build image → public key →
# backup → (restore) → migrations → start/update → wait for health → diagnostics.

set -euo pipefail

SELF_DIR="$(cd "$(dirname "$0")" && pwd)"
SRC="$(cd "$SELF_DIR/../.." && pwd)"
. "$SELF_DIR/lib.sh"

VERSION="" DOMAIN="" EMAIL="" SITE="" PUBLIC_URL="" RESTORE="" SKIP_BACKUP=0 HEALTH_TIMEOUT=300 BIND=""
while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="$2"; shift ;;
        --domain) DOMAIN="$2"; shift ;;
        --email) EMAIL="$2"; shift ;;
        --site) SITE="$2"; shift ;;
        --public-url) PUBLIC_URL="$2"; shift ;;
        --restore) RESTORE="$2"; shift ;;
        --skip-backup) SKIP_BACKUP=1 ;;
        --health-timeout) HEALTH_TIMEOUT="$2"; shift ;;
        --bind) BIND="$2"; shift ;;
        *) die "opção desconhecida: $1" ;;
    esac
    shift
done
[ -n "$VERSION" ] || die "--version é obrigatório"

STEP=0
step() { STEP=$((STEP + 1)); printf '\n[%d] %s\n' "$STEP" "$*"; }

step "Instalando compose e scripts da versão $VERSION"
install -d "$AIONIX_ROOT/compose" "$AIONIX_ROOT/scripts"
for f in docker-compose.yml Caddyfile prometheus.yml .env.example; do
    install -m 0644 "$SRC/infra/compose/$f" "$AIONIX_ROOT/compose/$f"
done
for f in "$SRC"/infra/scripts/*.sh; do
    install -m 0755 "$f" "$AIONIX_ROOT/scripts/$(basename "$f")"
done

step "Configuração"
if [ ! -f "$AIONIX_ENV_FILE" ]; then
    if [ -n "$DOMAIN" ]; then
        bash "$SELF_DIR/generate-config.sh" --domain "$DOMAIN" --email "$EMAIL"
    elif [ -n "$SITE" ] && [ -n "$PUBLIC_URL" ]; then
        bash "$SELF_DIR/generate-config.sh" --site "$SITE" --public-url "$PUBLIC_URL"
    else
        die "primeiro deploy: informe -Domain (ou -Site e -PublicUrl) para gerar a configuração"
    fi
else
    log "mantendo $AIONIX_ENV_FILE existente"
fi
[ -f "$AIONIX_ROOT/config/secrets/token-signing.key" ] || bash "$SELF_DIR/generate-config.sh" --site x --public-url x
env_set AIONIX_VERSION "$VERSION"
env_set AIONIX_SOURCE_DIR "$SRC"
env_set AIONIX_CONFIG_DIR "$AIONIX_ROOT/config"
env_set AIONIX_DATA_DIR "$AIONIX_ROOT/data"
env_set AIONIX_LOG_DIR "$AIONIX_ROOT/logs"
if [ -n "$BIND" ]; then
    # Docker publishes ports past UFW; binding to one address is what limits exposure.
    env_set AIONIX_HTTP_PORT "$BIND:80"
    env_set AIONIX_HTTPS_PORT "$BIND:443"
    log "Caddy publicado somente em $BIND (80/443)"
fi

# Compose refuses to interpolate an empty AIONIX_TOKEN_PUBLIC_KEYS even for `build`;
# the real value is derived from the built image in the next step.
[ -n "$(env_get AIONIX_TOKEN_PUBLIC_KEYS || true)" ] || env_set AIONIX_TOKEN_PUBLIC_KEYS change-me-derived-after-build

step "Build da imagem aionix-connect-server:$VERSION"
DOCKER_BUILDKIT=1 dc build aionix-api

step "Chave pública de assinatura"
if [ -z "$(env_get AIONIX_TOKEN_PUBLIC_KEYS || true)" ] || env_get AIONIX_TOKEN_PUBLIC_KEYS | grep -q change-me; then
    PK="$(dc --profile tools run --rm --no-deps -T aionix-admin aionix-admin public-key \
        --key-file /run/secrets/token_signing_key | tr -d '\r\n')"
    [ -n "$PK" ] || die "não foi possível derivar a chave pública"
    env_set AIONIX_TOKEN_PUBLIC_KEYS "$PK"
    log "AIONIX_TOKEN_PUBLIC_KEYS definida ($PK)"
else
    log "AIONIX_TOKEN_PUBLIC_KEYS já definida"
fi

step "Backup antes da atualização"
if [ "$SKIP_BACKUP" -eq 0 ] && dc ps --status running --services 2>/dev/null | grep -qx postgres; then
    bash "$AIONIX_ROOT/scripts/backup.sh"
else
    log "ignorado (primeira instalação ou --skip-backup)"
fi

if [ -n "$RESTORE" ]; then
    step "Restaurando backup $RESTORE (com a chave de assinatura)"
    bash "$AIONIX_ROOT/scripts/restore.sh" "$RESTORE" --with-key --yes
fi

step "Migrações do banco de dados"
dc up -d postgres redis
dc run --rm aionix-migrate

step "Iniciando/atualizando serviços"
dc up -d --remove-orphans

step "Aguardando health checks (até ${HEALTH_TIMEOUT}s)"
deadline=$(( $(date +%s) + HEALTH_TIMEOUT ))
while :; do
    states="$(dc ps --format '{{.Service}}={{.Health}}' 2>/dev/null || true)"
    ok=1
    for s in aionix-api aionix-signaling aionix-relay postgres redis; do
        echo "$states" | grep -qx "$s=healthy" || ok=0
    done
    [ "$ok" -eq 1 ] && break
    if [ "$(date +%s)" -ge "$deadline" ]; then
        echo "$states"
        dc logs --tail 60 aionix-api aionix-signaling aionix-relay || true
        die "serviços não ficaram saudáveis a tempo"
    fi
    sleep 5
done
echo "$states" | sed 's/^/  /'

ln -sfn "$(dirname "$SRC")" "$AIONIX_ROOT/current"
printf '%s %s\n' "$(date -u +%FT%TZ)" "$VERSION" >> "$AIONIX_ROOT/releases/deployed.log"

step "Diagnóstico"
bash "$AIONIX_ROOT/scripts/aionix-diagnostico.sh" --sem-internet || true
echo
log "deploy da versão $VERSION concluído"
