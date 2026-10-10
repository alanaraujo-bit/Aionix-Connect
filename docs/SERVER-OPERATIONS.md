# Aionix server — operational report

Last verified: **2026-10-10 (UTC)**. Every item marked VERIFIED was executed against the real
server and observed; the audit trail is in `/var/log/aionix-ops/ops.log` on the server.

## Server

| | |
| --- | --- |
| Host | `aionix-server`, `192.168.18.110` (LAN), bare metal |
| OS | Ubuntu Server 24.04.5 LTS, kernel 6.8 |
| CPU / RAM | Xeon E5-2650 v2, 16 threads · 15 GiB |
| System disk | SSD 120 GB (LVM, `/` 107 GB) |
| Second disk | Hitachi 1 TB (`sdb1`, **NTFS, not mounted, untouched**) |
| Docker | Engine 29.9 + Compose v5.6 (Docker's official apt repo) |
| Operator | `alanaraujo`, SSH key `id_ed25519` from the Windows workstation |

## Running stack — VERIFIED

Release `20261010.0213-b450ed708b61` in `/opt/aionix-connect` (layout in DEPLOYMENT.md),
deployed with:

```powershell
.\infra\deploy-server.ps1 -Server alanaraujo@192.168.18.110 -Site ':80' -PublicUrl http://192.168.18.110 -BindAddress 192.168.18.110
```

| Service | State | Check |
| --- | --- | --- |
| postgres 17.6, redis 7.4 | healthy | not published on the host (connect refused on :5432/:6379) |
| aionix-api / signaling / relay | healthy | `/v1/server/info` 200 · `/v1/signal` WS without token 401 · `/ping` 200 · `/generate_204` 204 |
| caddy 2.10 | running | `http://192.168.18.110/healthz` 200; `/readyz`, `/metrics`, unknown routes 404 |

Mode (2026-10-10): **public HTTPS at `https://connect.aionixdev.com`** — DNS A record
(Hostinger) → 138.0.54.176 (fixed IP), ONT Huawei EG8145X6 port mapping `Aionix_Connect`
(TCP/UDP 443, TCP 80 → 192.168.18.110), Let's Encrypt certificate via Caddy (tls-alpn-01),
HTTP→HTTPS 308. Verified from Brazil, Germany, Romania, Vietnam and Iran, and from the LAN
(NAT loopback works). Caddy publishes 80/443 **only on
192.168.18.110** (`AIONIX_HTTP_PORT`/`AIONIX_HTTPS_PORT` in `.env`), so nothing listens on
the server's public IPv6 address. Docker-published ports bypass UFW; the bind address is
what limits exposure.

## Recovery — VERIFIED

- Restart policy `unless-stopped` on every long-running service (`aionix-migrate` is one-shot).
- Crash simulation: `kill -9` of the relay process → `restarting` → `healthy` in **8 s**,
  RestartCount 0→1, `/ping` back to 200.
  Note: `docker kill`/`docker stop` count as *manual* stops and are **not** auto-restarted
  (Docker behaviour) — use `docker start` or `docker compose up -d` after those.
- Backup: `scripts/backup.sh` → `backups/aionix-backup-<UTC>.tar.gz` (PostgreSQL dump,
  `.env`, token-signing key, Caddy data). Daily cron 03:15 (`/etc/cron.d/aionix-backup`).
- **Not yet verified:** full reboot persistence (needs one controlled reboot) and a
  restore drill (`scripts/restore.sh`) on this server.

## Security posture

| Item | State |
| --- | --- |
| UFW | active: deny incoming; allow 22/tcp, 80/tcp, 443/tcp, 443/udp (v4+v6) |
| SSH | key login verified; **password login still enabled** (see open items) |
| sudo | `alanaraujo` has NOPASSWD sudo for automation — revert: `sudo rm /etc/sudoers.d/90-aionix-operator` |
| Secrets | `.env` 0600 and signing key 0440 (uid 65532:aionix) under `config/` |
| Backups | **unencrypted, on the same SSD** as the data (see open items) |

## Operations layer (`infra/ops/`)

- `aionix-run` — audited runner. Every change made on the server goes through it: command,
  stdout/stderr (stderr marked `ERR`), exit code, duration, operator and SSH origin →
  `/var/log/aionix-ops/ops.log` (root:adm 0640, logrotate daily ×30). Secrets
  (`*PASSWORD*=`, `SENHA`, tokens, URL credentials, `Authorization`, private-key blocks)
  are masked before anything is written or echoed.
- `aionix-observer.service` keeps the tmux session **`aionix-setup`** alive (boot-enabled):
  window `ops` = live audit log, window `monitor` = `aionix-status` every 5 s.
  Watch read-only from PuTTY: `tmux attach -r -f ignore-size -t aionix-setup`
  (`Ctrl+b 0/1` switch windows, `Ctrl+b d` detach).
- `deploy-server.ps1` sends every changing remote step through `aionix-run`, so deploys
  appear live in the session (`-NoAudit` to disable).

## Public status dashboard

**https://aionix-status.vercel.app** (Vercel team `aionixdev`, project `aionix-status`,
source `dashboard/`).

- Push model: `aionix-telemetry.service` (unprivileged user `aionix-telemetry`, groups
  docker+aionix, hardened unit) POSTs a snapshot every 10 s to `/api/ingest` with a bearer
  token (`/etc/aionix-telemetry.env`, root 0600; Vercel env `INGEST_TOKEN`, sensitive).
  No inbound port is opened for it.
- Published data: CPU (total/per thread), load, memory, disk usage and I/O, network rate,
  CPU temperature, uptime, service/container health, API latency, OS name, Aionix
  version. **Never** IPs, kernel/Docker/image versions, env values, logs or user data.
- State + 1 h history in the Vercel Runtime Cache; `/api/status` is CDN-cached 4 s, so
  viewer polling does not scale function invocations. Wrong/missing token → 401.
- Rotate the token: new value in Vercel (`vercel env rm/add INGEST_TOKEN production`,
  redeploy) and in `/etc/aionix-telemetry.env`, then `systemctl restart aionix-telemetry`.

## Open items (need a decision)

1. **SSH hardening** — the server password was shared in a chat; change it (`passwd`).
   Optional: disable `PasswordAuthentication` (key login is verified) and/or restrict
   22/tcp to `192.168.18.0/24` in UFW.
2. **Controlled reboot** to verify the stack, `aionix-observer`, `aionix-telemetry`, UFW
   and SSH come back unattended.
3. **Off-disk backups** — use the 1 TB HDD (currently NTFS; formatting it erases its
   contents) or another destination; consider encryption via `AIONIX_BACKUP_PASSPHRASE_FILE` in `.env`.
4. **Restore drill** of today's backup into a scratch stack.
5. Desktop packages for the public server: `infra/package-apps.ps1 -Server
   https://connect.aionixdev.com -Name internet` (the earlier LAN packages that point at
   `http://192.168.18.110` no longer work: Caddy now serves only the domain).
