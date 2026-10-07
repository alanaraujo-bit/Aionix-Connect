#!/usr/bin/env bash
# Shared helpers for the Aionix Connect server scripts (sourced, not executed).

AIONIX_ROOT="${AIONIX_ROOT:-/opt/aionix-connect}"
AIONIX_ENV_FILE="${AIONIX_ENV_FILE:-$AIONIX_ROOT/config/.env}"
AIONIX_COMPOSE_FILE="${AIONIX_COMPOSE_FILE:-$AIONIX_ROOT/compose/docker-compose.yml}"
# uid/gid of the distroless "nonroot" user inside the server image.
AIONIX_CONTAINER_UID=65532

log()  { printf '[aionix] %s\n' "$*" >&2; }
warn() { printf '[aionix] AVISO: %s\n' "$*" >&2; }
die()  { printf '[aionix] ERRO: %s\n' "$*" >&2; exit 1; }

# docker compose with the server layout.
dc() {
    docker compose -f "$AIONIX_COMPOSE_FILE" --env-file "$AIONIX_ENV_FILE" "$@"
}

# Reads KEY from the env file without sourcing it (values may contain shell chars).
env_get() {
    local key="$1"
    [ -f "$AIONIX_ENV_FILE" ] || return 1
    grep -E "^${key}=" "$AIONIX_ENV_FILE" | tail -n1 | cut -d= -f2-
}

# Sets KEY=VALUE in the env file (replacing an existing line), keeping mode 0600.
env_set() {
    local key="$1" value="$2" tmp
    tmp="$(mktemp)"
    if [ -f "$AIONIX_ENV_FILE" ]; then
        grep -vE "^${key}=" "$AIONIX_ENV_FILE" > "$tmp" || true
    fi
    printf '%s=%s\n' "$key" "$value" >> "$tmp"
    install -m 0600 "$tmp" "$AIONIX_ENV_FILE"
    rm -f "$tmp"
}

# Gives the signing key to the container user (65532) with group "aionix" read access
# (operator backups), mode 0440. Uses the Docker daemon when not root: membership in
# the docker group is root-equivalent anyway.
secure_key_file() {
    local key="$1" gid
    gid="$(getent group aionix | cut -d: -f3)"
    gid="${gid:-$AIONIX_CONTAINER_UID}"
    if [ "$(id -u)" -eq 0 ]; then
        chown "$AIONIX_CONTAINER_UID:$gid" "$key"
        chmod 0440 "$key"
    else
        docker run --rm --user 0 -v "$(dirname "$key"):/s" postgres:17.6-alpine \
            sh -c "chown $AIONIX_CONTAINER_UID:$gid /s/$(basename "$key") && chmod 0440 /s/$(basename "$key")"
    fi
}

# 32 random bytes, URL-safe base64 without padding (passwords, Ed25519 seeds).
random_secret() {
    head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -d '=\n'
}
