#!/usr/bin/env bash
# Relatório de prontidão do servidor Aionix Connect (pt-BR).
#
#   bash aionix-diagnostico.sh [--sem-internet]
#
# Nunca inventa valores: quando algo não pode ser medido mostra "Indisponível",
# "Não configurado" ou "Não foi possível determinar". --sem-internet evita as
# consultas externas usadas para descobrir os IPs públicos.

set -uo pipefail
. "$(dirname "$0")/lib.sh"

OFFLINE=0
[ "${1:-}" = "--sem-internet" ] && OFFLINE=1

ND="Não foi possível determinar"
line() { printf '%-25s %s\n' "$1 $(printf '.%.0s' $(seq 1 $((24 - ${#1}))))" "$2"; }
have() { command -v "$1" >/dev/null 2>&1; }

svc_running() { dc ps --status running --services 2>/dev/null | grep -qx "$1"; }
probe() { # service url
    svc_running "$1" || { echo "Offline (container parado)"; return; }
    if dc exec -T "$1" aionix-healthcheck "$2" >/dev/null 2>&1; then echo "Online"
    else echo "Não pronto (processo ativo, dependências falhando)"; fi
}

echo
echo "AIONIX CONNECT SERVER"
echo
if [ -r /etc/os-release ]; then . /etc/os-release; line "Sistema" "${PRETTY_NAME:-$ND}"; else line "Sistema" "$ND"; fi
if have docker && docker info >/dev/null 2>&1; then
    line "Docker" "OK ($(docker version --format '{{.Server.Version}}' 2>/dev/null))"
    DOCKER_OK=1
else
    line "Docker" "Indisponível"; DOCKER_OK=0
fi
if [ ! -f "$AIONIX_ENV_FILE" ]; then
    line "Configuração" "Não configurado ($AIONIX_ENV_FILE ausente)"
fi

if [ "$DOCKER_OK" -eq 1 ] && [ -f "$AIONIX_ENV_FILE" ]; then
    if svc_running postgres && dc exec -T postgres pg_isready -U aionix -d aionix -h 127.0.0.1 >/dev/null 2>&1; then
        line "PostgreSQL" "Online"
    else
        line "PostgreSQL" "Offline"
    fi
    if svc_running redis && dc exec -T redis redis-cli ping 2>/dev/null | grep -q PONG; then
        line "Redis" "Online"
    else
        line "Redis" "Offline"
    fi
    line "API" "$(probe aionix-api http://127.0.0.1:8080/readyz)"
    line "Signaling" "$(probe aionix-signaling http://127.0.0.1:8081/readyz)"
    line "Relay" "$(probe aionix-relay http://127.0.0.1:9090/readyz)"
    if svc_running caddy; then line "Proxy reverso" "Online"; else line "Proxy reverso" "Offline"; fi
else
    for s in PostgreSQL Redis API Signaling Relay; do line "$s" "Indisponível"; done
fi

echo
echo "REDE"
echo
LOCAL_IP="$(ip -4 route get 1.1.1.1 2>/dev/null | awk '{for(i=1;i<NF;i++) if($i=="src"){print $(i+1); exit}}')"
line "IP local" "${LOCAL_IP:-$ND}"
PUB4="" PUB6=""
if [ "$OFFLINE" -eq 0 ] && have curl; then
    PUB4="$(curl -4 -fsS --max-time 5 https://api.ipify.org 2>/dev/null || true)"
    PUB6="$(curl -6 -fsS --max-time 5 https://api64.ipify.org 2>/dev/null || true)"
fi
if [ "$OFFLINE" -eq 1 ]; then
    line "IPv4 público" "Não verificado (--sem-internet)"
    line "IPv6" "$(ip -6 addr show scope global 2>/dev/null | awk '/inet6/{print $2; exit}' || true)"
else
    line "IPv4 público" "${PUB4:-$ND}"
    if [ -n "$PUB6" ]; then line "IPv6" "$PUB6"
    elif ip -6 addr show scope global 2>/dev/null | grep -q inet6; then line "IPv6" "Endereço global local presente, sem conectividade verificada"
    else line "IPv6" "Indisponível"; fi
fi
# CGNAT: só é possível afirmar quando o próprio host tem endereço 100.64.0.0/10, ou
# quando o IP público é o do próprio host. Atrás de NAT, o IP WAN do roteador decide.
in_cgnat() { local a b; IFS=. read -r a b _ <<<"$1"; [ "${a:-0}" = 100 ] && [ "${b:-0}" -ge 64 ] && [ "${b:-0}" -le 127 ]; }
if [ -n "$LOCAL_IP" ] && in_cgnat "$LOCAL_IP"; then
    line "CGNAT" "Sim (endereço local em 100.64.0.0/10)"
elif [ -n "$PUB4" ] && [ "$PUB4" = "$LOCAL_IP" ]; then
    line "CGNAT" "Não (IP público direto no servidor)"
elif [ -n "$PUB4" ] && in_cgnat "$PUB4"; then
    line "CGNAT" "Sim"
elif [ -n "$PUB4" ]; then
    line "CGNAT" "$ND (atrás de NAT; se o IP WAN do roteador for 100.64.x.x–100.127.x.x, há CGNAT)"
else
    line "CGNAT" "$ND"
fi

SITE="$(env_get AIONIX_SITE_ADDRESS 2>/dev/null || true)"
DOMAIN=""
case "$SITE" in
    ""|:*|localhost|127.*) ;;
    *) DOMAIN="${SITE%%:*}" ;;
esac
if [ -z "$DOMAIN" ]; then
    line "DNS" "Não configurado (site: ${SITE:-vazio})"
    line "TLS" "Não configurado (HTTP sem certificado)"
else
    RESOLVED="$(getent ahostsv4 "$DOMAIN" 2>/dev/null | awk '{print $1; exit}')"
    if [ -z "$RESOLVED" ]; then line "DNS" "$DOMAIN não resolve"
    elif [ -n "$PUB4" ] && [ "$RESOLVED" = "$PUB4" ]; then line "DNS" "$DOMAIN → $RESOLVED (corresponde ao IP público)"
    elif [ -n "$PUB4" ]; then line "DNS" "$DOMAIN → $RESOLVED (DIFERENTE do IP público $PUB4)"
    else line "DNS" "$DOMAIN → $RESOLVED"; fi
    if have openssl; then
        END="$(echo | timeout 8 openssl s_client -connect "$DOMAIN:443" -servername "$DOMAIN" 2>/dev/null \
            | openssl x509 -noout -enddate 2>/dev/null | cut -d= -f2)"
        if [ -n "$END" ]; then line "TLS" "Válido até $END"; else line "TLS" "Indisponível (sem certificado em $DOMAIN:443)"; fi
    else
        line "TLS" "$ND (openssl ausente)"
    fi
fi
if have ss; then
    HTTP_PORT="$(env_get AIONIX_HTTP_PORT 2>/dev/null || true)"; HTTP_PORT="${HTTP_PORT:-80}"
    HTTPS_PORT="$(env_get AIONIX_HTTPS_PORT 2>/dev/null || true)"; HTTPS_PORT="${HTTPS_PORT:-443}"
    PORTS=""
    for p in "$HTTP_PORT" "$HTTPS_PORT"; do
        # The value may be "ADDR:PORT" (Caddy bound to one address); ss filters by port.
        if ss -ltnH "sport = :${p##*:}" 2>/dev/null | grep -q .; then PORTS="$PORTS $p/tcp:escutando"
        else PORTS="$PORTS $p/tcp:fechada"; fi
    done
    line "Portas" "${PORTS# } (acesso externo depende do firewall/roteador)"
else
    line "Portas" "$ND"
fi

echo
echo "SESSÕES"
echo
if [ "$DOCKER_OK" -eq 1 ] && svc_running aionix-api; then
    STATUS="$(dc exec -T aionix-api aionix-admin status --json 2>/dev/null || true)"
else
    STATUS=""
fi
json_num() { printf '%s' "$1" | grep -oE "\"$2\":[0-9]+" | head -n1 | cut -d: -f2; }
if [ -n "$STATUS" ]; then
    ONLINE="$(json_num "$STATUS" devices_online)"
    line "Dispositivos online" "${ONLINE:-Indisponível}"
    line "Dispositivos" "$(json_num "$STATUS" devices)"
    line "Sessões ativas" "$(json_num "$STATUS" sessions_active)"
else
    line "Dispositivos online" "Indisponível"
    line "Sessões ativas" "Indisponível"
fi
if [ "$DOCKER_OK" -eq 1 ] && svc_running aionix-relay; then
    RELAY="$(dc exec -T aionix-relay aionix-healthcheck http://127.0.0.1:9090/status --print 2>/dev/null || true)"
    CONN="$(printf '%s' "$RELAY" | grep -oE '"connections_active":-?[0-9]+' | cut -d: -f2)"
    line "Conexões relay" "${CONN:-Indisponível}"
else
    line "Conexões relay" "Indisponível"
fi
echo
