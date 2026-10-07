#!/usr/bin/env bash
# Full backup of an Aionix Connect server:
#   * PostgreSQL (pg_dump, custom format — consistent snapshot while running)
#   * config/.env and config/secrets/token-signing.key   (SECRETS)
#   * Caddy data (TLS certificates/ACME account; optional, re-issued if lost)
#   * manifest with version, schema migrations and SHA-256 of every file
#
#   bash backup.sh [destination-dir]        default: /opt/aionix-connect/backups
#
# Redis is not backed up: it only holds ephemeral presence/rate-limit state.
# With AIONIX_BACKUP_PASSPHRASE_FILE set, the archive is AES-256 encrypted (openssl).
# The archive contains the token signing key: losing it invalidates every enrolled
# device's pinned key; leaking it allows forging tickets. Store it accordingly.

set -euo pipefail
. "$(dirname "$0")/lib.sh"

DEST="${1:-$AIONIX_ROOT/backups}"
KEEP="$(env_get AIONIX_BACKUP_KEEP || true)"; KEEP="${KEEP:-14}"
PASSFILE="$(env_get AIONIX_BACKUP_PASSPHRASE_FILE || true)"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
NAME="aionix-backup-$TS"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
OUT="$WORK/$NAME"
mkdir -p "$OUT/config/secrets"

log "exportando PostgreSQL"
dc exec -T postgres pg_dump -U aionix -d aionix --format=custom --no-owner --no-privileges > "$OUT/postgres.dump"
[ -s "$OUT/postgres.dump" ] || die "pg_dump produziu arquivo vazio"
MIGRATIONS="$(dc exec -T postgres psql -U aionix -d aionix -At -c \
    "SELECT string_agg(version::text, ',' ORDER BY version) FROM _sqlx_migrations WHERE success" 2>/dev/null || echo "?")"

log "copiando configuração e segredos"
cp "$AIONIX_ENV_FILE" "$OUT/config/.env"
if [ -r "$AIONIX_ROOT/config/secrets/token-signing.key" ]; then
    cp "$AIONIX_ROOT/config/secrets/token-signing.key" "$OUT/config/secrets/token-signing.key"
else
    die "não foi possível ler a chave de assinatura (execute como root ou membro do grupo aionix)"
fi

log "copiando dados do Caddy (certificados)"
if dc exec -T caddy tar -C /data -czf - . > "$OUT/caddy-data.tar.gz" 2>/dev/null; then :; else
    warn "dados do Caddy indisponíveis (serviço parado?); certificados serão reemitidos se necessário"
    rm -f "$OUT/caddy-data.tar.gz"
fi

{
    echo "format=aionix-backup/1"
    echo "created_at=$TS"
    echo "host=$(hostname)"
    echo "version=$(env_get AIONIX_VERSION || echo unknown)"
    echo "migrations=$MIGRATIONS"
    (cd "$OUT" && find . -type f ! -name MANIFEST -print0 | sort -z | xargs -0 sha256sum)
} > "$OUT/MANIFEST"

mkdir -p "$DEST"
ARCHIVE="$DEST/$NAME.tar.gz"
tar -C "$WORK" -czf "$ARCHIVE" "$NAME"
if [ -n "$PASSFILE" ]; then
    [ -r "$PASSFILE" ] || die "AIONIX_BACKUP_PASSPHRASE_FILE não legível: $PASSFILE"
    openssl enc -aes-256-cbc -pbkdf2 -iter 200000 -salt -pass "file:$PASSFILE" \
        -in "$ARCHIVE" -out "$ARCHIVE.enc"
    rm -f "$ARCHIVE"
    ARCHIVE="$ARCHIVE.enc"
fi
chmod 0640 "$ARCHIVE"

# Retention: keep the newest $KEEP archives.
ls -1t "$DEST"/aionix-backup-*.tar.gz* 2>/dev/null | tail -n +"$((KEEP + 1))" | xargs -r rm -f

log "backup concluído: $ARCHIVE ($(du -h "$ARCHIVE" | cut -f1))"
echo "$ARCHIVE"
