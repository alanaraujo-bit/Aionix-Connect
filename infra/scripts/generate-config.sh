#!/usr/bin/env bash
# Creates /opt/aionix-connect/config/.env and the token signing key on a server that
# has none yet. Never overwrites existing secrets.
#
#   bash generate-config.sh --domain connect.example.com --email ops@example.com
#   bash generate-config.sh --site ':80' --public-url http://192.168.1.50   # LAN staging

set -euo pipefail
. "$(dirname "$0")/lib.sh"

DOMAIN="" EMAIL="" SITE="" PUBLIC_URL=""
while [ $# -gt 0 ]; do
    case "$1" in
        --domain) DOMAIN="$2"; shift ;;
        --email) EMAIL="$2"; shift ;;
        --site) SITE="$2"; shift ;;
        --public-url) PUBLIC_URL="$2"; shift ;;
        *) die "opção desconhecida: $1" ;;
    esac
    shift
done
if [ -n "$DOMAIN" ]; then
    SITE="${SITE:-$DOMAIN}"
    PUBLIC_URL="${PUBLIC_URL:-https://$DOMAIN}"
fi
[ -n "$SITE" ] && [ -n "$PUBLIC_URL" ] || die "informe --domain (produção) ou --site e --public-url"

install -d -m 0750 "$AIONIX_ROOT/config/secrets"
KEY="$AIONIX_ROOT/config/secrets/token-signing.key"
if [ -f "$KEY" ]; then
    log "chave de assinatura já existe; mantida ($KEY)"
else
    (umask 077 && random_secret > "$KEY")
    secure_key_file "$KEY"
    log "chave de assinatura criada ($KEY) — inclua-a nos backups"
fi

if [ -f "$AIONIX_ENV_FILE" ]; then
    log "$AIONIX_ENV_FILE já existe; nada a fazer (edite manualmente se necessário)"
    exit 0
fi
umask 077
cat > "$AIONIX_ENV_FILE" <<EOF
AIONIX_VERSION=dev
AIONIX_SOURCE_DIR=$AIONIX_ROOT/current/src
AIONIX_CONFIG_DIR=$AIONIX_ROOT/config
AIONIX_DATA_DIR=$AIONIX_ROOT/data
AIONIX_LOG_DIR=$AIONIX_ROOT/logs
AIONIX_SITE_ADDRESS=$SITE
ACME_EMAIL=$EMAIL
AIONIX_HTTP_PORT=80
AIONIX_HTTPS_PORT=443
AIONIX_RELAY_URLS=$PUBLIC_URL
AIONIX_SERVER_NAME=Aionix Connect
POSTGRES_PASSWORD=$(random_secret)
REDIS_PASSWORD=$(random_secret)
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
EOF
chmod 0600 "$AIONIX_ENV_FILE"
log "configuração criada em $AIONIX_ENV_FILE"
log "AIONIX_TOKEN_PUBLIC_KEYS é preenchida pelo deploy (aionix-admin public-key) após o build da imagem"
