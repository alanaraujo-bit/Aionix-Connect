#!/usr/bin/env bash
# Installs the operations observability layer on the Aionix server.
#
#   sudo bash install-ops.sh --operator <ssh-user> [--nopasswd-sudo]
#
# - /usr/local/bin/aionix-run     audited command runner → /var/log/aionix-ops/ops.log
# - /usr/local/bin/aionix-status  one-screen resource/service status
# - /usr/local/bin/aionix-watch   live, coloured follow of the ops log
# - logrotate for /var/log/aionix-ops (daily, 30 days, compressed)
# - aionix-observer.service       keeps the tmux session "aionix-setup" alive (operator user):
#       window "ops"     → aionix-watch
#       window "monitor" → watch aionix-status
#   Observe from any SSH client (read-only):  tmux attach -r -t aionix-setup
# - --nopasswd-sudo               /etc/sudoers.d/90-aionix-operator (validated with visudo)
#
# Idempotent. Does not touch sshd, the firewall or disks.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
OPERATOR="${SUDO_USER:-}"
NOPASSWD=0
while [ $# -gt 0 ]; do
    case "$1" in
        --operator) OPERATOR="$2"; shift ;;
        --nopasswd-sudo) NOPASSWD=1 ;;
        *) echo "opção desconhecida: $1" >&2; exit 2 ;;
    esac
    shift
done
[ "$(id -u)" -eq 0 ] || { echo "execute como root (sudo)" >&2; exit 1; }
[ -n "$OPERATOR" ] && id "$OPERATOR" >/dev/null 2>&1 || { echo "--operator inválido" >&2; exit 1; }

echo "==> pacotes (tmux, logrotate, curl, jq)"
need=()
for p in tmux logrotate curl jq; do dpkg -s "$p" >/dev/null 2>&1 || need+=("$p"); done
if [ ${#need[@]} -gt 0 ]; then
    DEBIAN_FRONTEND=noninteractive apt-get update -y
    DEBIAN_FRONTEND=noninteractive apt-get install -y "${need[@]}"
fi

echo "==> ferramentas em /usr/local/bin"
install -m 0755 "$HERE/aionix-run" /usr/local/bin/aionix-run
install -m 0755 "$HERE/aionix-status" /usr/local/bin/aionix-status
cat > /usr/local/bin/aionix-watch <<'EOF'
#!/usr/bin/env bash
# Live follow of the operations audit log (survives rotation).
while true; do
    tail -n 300 -F /var/log/aionix-ops/ops.log 2>/dev/null \
        || { echo "aguardando /var/log/aionix-ops/ops.log ..."; sleep 5; }
done
EOF
chmod 0755 /usr/local/bin/aionix-watch

echo "==> /var/log/aionix-ops (root:adm 0750, log 0640)"
install -d -m 0750 -o root -g adm /var/log/aionix-ops
[ -f /var/log/aionix-ops/ops.log ] || install -m 0640 -o root -g adm /dev/null /var/log/aionix-ops/ops.log
id -nG "$OPERATOR" | grep -qw adm || usermod -aG adm "$OPERATOR"

cat > /etc/logrotate.d/aionix-ops <<'EOF'
/var/log/aionix-ops/*.log {
    daily
    rotate 30
    compress
    delaycompress
    missingok
    notifempty
    copytruncate
    create 0640 root adm
}
EOF
logrotate -d /etc/logrotate.d/aionix-ops >/dev/null 2>&1 || { echo "logrotate config inválida" >&2; exit 1; }

echo "==> aionix-observer.service (tmux aionix-setup como $OPERATOR)"
cat > /usr/local/bin/aionix-observer <<'EOF'
#!/usr/bin/env bash
# Creates the observation tmux session if missing (called by aionix-observer.service).
S=aionix-setup
tmux has-session -t "$S" 2>/dev/null && exit 0
tmux new-session -d -s "$S" -n ops -x 200 -y 50 /usr/local/bin/aionix-watch
tmux new-window  -t "$S" -n monitor 'watch -t -c -n 5 /usr/local/bin/aionix-status'
tmux set-option  -t "$S" status-left '#[bold] AIONIX #[default]'
tmux set-option  -t "$S" status-right '#(hostname)  %H:%M:%S '
tmux set-option  -t "$S" status-interval 1
tmux set-option  -t "$S" history-limit 50000
tmux set-window-option -t "$S" -g remain-on-exit on
tmux select-window -t "$S:ops"
EOF
chmod 0755 /usr/local/bin/aionix-observer
cat > /etc/systemd/system/aionix-observer.service <<EOF
[Unit]
Description=Aionix ops observation tmux session (aionix-setup)
After=network.target

[Service]
Type=forking
User=$OPERATOR
Environment=TERM=xterm-256color
ExecStart=/usr/local/bin/aionix-observer
ExecStop=/usr/bin/tmux kill-session -t aionix-setup
RemainAfterExit=yes
Restart=on-failure

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable aionix-observer.service >/dev/null 2>&1
# The adm group membership above only applies to new logins/processes: restart.
systemctl restart aionix-observer.service

if [ "$NOPASSWD" -eq 1 ]; then
    echo "==> sudo sem senha para $OPERATOR"
    f=/etc/sudoers.d/90-aionix-operator
    tmp="$(mktemp)"
    printf '# Aionix ops automation (remove this file to revert)\n%s ALL=(ALL) NOPASSWD: ALL\n' "$OPERATOR" > "$tmp"
    visudo -cf "$tmp" >/dev/null
    install -m 0440 -o root -g root "$tmp" "$f"
    rm -f "$tmp"
    visudo -c >/dev/null
fi

echo "==> pronto"
echo "OPS_INSTALL_OK"
