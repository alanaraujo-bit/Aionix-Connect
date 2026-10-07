#!/usr/bin/env bash
# Restores an archive made by backup.sh (or by infra/local/staging.ps1 backup).
#
#   bash restore.sh ARCHIVE [--with-key] [--with-config] [--yes]
#
#   --with-key     install the archive's token signing key (REQUIRED when migrating a
#                  server: enrolled devices pinned its public key)
#   --with-config  also replace config/.env (same-host disaster recovery; when moving
#                  from staging to production keep the production .env instead)
#   --yes          do not ask for confirmation
#
# The current database is replaced. A safety backup is taken first when possible.

set -euo pipefail
. "$(dirname "$0")/lib.sh"

ARCHIVE="" WITH_KEY=0 WITH_CONFIG=0 YES=0
for a in "$@"; do
    case "$a" in
        --with-key) WITH_KEY=1 ;;
        --with-config) WITH_CONFIG=1; WITH_KEY=1 ;;
        --yes) YES=1 ;;
        -*) die "opção desconhecida: $a" ;;
        *) ARCHIVE="$a" ;;
    esac
done
[ -f "$ARCHIVE" ] || die "arquivo de backup não encontrado: $ARCHIVE"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
SRC="$ARCHIVE"
if [[ "$ARCHIVE" == *.enc ]]; then
    PASSFILE="$(env_get AIONIX_BACKUP_PASSPHRASE_FILE || true)"
    [ -r "$PASSFILE" ] || die "backup criptografado: defina AIONIX_BACKUP_PASSPHRASE_FILE"
    openssl enc -d -aes-256-cbc -pbkdf2 -iter 200000 -pass "file:$PASSFILE" -in "$ARCHIVE" -out "$WORK/archive.tar.gz"
    SRC="$WORK/archive.tar.gz"
fi
tar -C "$WORK" -xzf "$SRC"
DIR="$(find "$WORK" -mindepth 1 -maxdepth 1 -type d -name 'aionix-backup-*' | head -n1)"
[ -n "$DIR" ] && [ -f "$DIR/MANIFEST" ] || die "arquivo não é um backup Aionix (MANIFEST ausente)"
grep -q '^format=aionix-backup/1$' "$DIR/MANIFEST" || die "formato de backup desconhecido"

log "verificando integridade (SHA-256)"
(cd "$DIR" && grep -E '^[0-9a-f]{64}  ' MANIFEST | sha256sum -c --quiet -) || die "falha de integridade"
grep -E '^(created_at|host|version|migrations)=' "$DIR/MANIFEST" | sed 's/^/  /'

if [ "$YES" -ne 1 ]; then
    read -r -p "O banco de dados atual será SUBSTITUÍDO. Continuar? [s/N] " ans
    [ "$ans" = "s" ] || die "cancelado"
fi

if dc ps --status running postgres 2>/dev/null | grep -q postgres; then
    log "backup de segurança do estado atual"
    bash "$(dirname "$0")/backup.sh" "$AIONIX_ROOT/backups/pre-restore" || warn "backup de segurança falhou"
fi

if [ "$WITH_CONFIG" -eq 1 ]; then
    [ -f "$AIONIX_ENV_FILE" ] && cp "$AIONIX_ENV_FILE" "$AIONIX_ENV_FILE.before-restore.$(date +%s)"
    install -m 0600 "$DIR/config/.env" "$AIONIX_ENV_FILE"
    log "config/.env restaurado (revise AIONIX_SOURCE_DIR/AIONIX_DATA_DIR para este host)"
fi
if [ "$WITH_KEY" -eq 1 ]; then
    KEY="$AIONIX_ROOT/config/secrets/token-signing.key"
    if [ -f "$KEY" ]; then
        if [ "$(id -u)" -eq 0 ]; then mv "$KEY" "$KEY.before-restore.$(date +%s)"; else
            docker run --rm --user 0 -v "$(dirname "$KEY"):/s" postgres:17.6-alpine \
                sh -c "mv /s/token-signing.key /s/token-signing.key.before-restore.$(date +%s)"
        fi
    fi
    install -m 0600 "$DIR/config/secrets/token-signing.key" "$KEY"
    secure_key_file "$KEY"
    log "chave de assinatura restaurada"
fi

log "parando serviços da aplicação"
dc stop caddy aionix-api aionix-signaling aionix-relay >/dev/null 2>&1 || true
dc up -d postgres redis
for _ in $(seq 1 60); do
    dc exec -T postgres pg_isready -U aionix -d postgres -h 127.0.0.1 >/dev/null 2>&1 && break
    sleep 2
done

log "restaurando PostgreSQL"
dc exec -T postgres psql -U aionix -d postgres -v ON_ERROR_STOP=1 \
    -c "DROP DATABASE IF EXISTS aionix WITH (FORCE)" -c "CREATE DATABASE aionix OWNER aionix"
dc exec -T postgres pg_restore -U aionix -d aionix --no-owner --no-privileges --exit-on-error < "$DIR/postgres.dump"

if [ -f "$DIR/caddy-data.tar.gz" ]; then
    log "restaurando dados do Caddy"
    dc run --rm --no-deps --entrypoint sh -T caddy -c 'tar -C /data -xzf -' < "$DIR/caddy-data.tar.gz" \
        || warn "não foi possível restaurar os dados do Caddy"
fi

log "aplicando migrações pendentes e iniciando serviços"
dc run --rm aionix-migrate
dc up -d
log "restauração concluída — execute scripts/aionix-diagnostico.sh"
