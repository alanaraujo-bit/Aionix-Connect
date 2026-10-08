# Deployment

Target: one bare-metal server, **Ubuntu Server 24.04.5 LTS**, Docker Engine + Compose
plugin. Development happens on a separate Windows workstation which deploys over SSH.

**Current situation:** the Ubuntu server is temporarily unavailable. The Windows
workstation runs the same stack under Docker Desktop as the **development/staging**
server (not production). Nothing in the stack is Windows-specific; moving to Ubuntu is a
backup + deploy with restore (below). None of the procedures below has been executed
against real Docker yet — see PROJECT_STATUS.md.

## Server layout

```text
/opt/aionix-connect/
├── compose/           docker-compose.yml, Caddyfile, prometheus.yml (from the release)
├── config/            .env (0600)
│   └── secrets/       token-signing.key (uid 65532, group aionix, 0440)
├── data/              postgres/ (PostgreSQL), caddy/ (certificates), prometheus/
├── logs/              caddy/access.log, backup.log
├── backups/           aionix-backup-<UTC>.tar.gz[.enc], pre-restore/, incoming/
├── scripts/           bootstrap, backup, restore, diagnostics, … (from the release)
├── releases/<ver>/src source of each deployed version; deployed.log
└── current ->         releases/<active version>
```

Persistent state = PostgreSQL + signing key + `.env` (+ Caddy certificates). Redis is
ephemeral by design.

## 1. Prepare a fresh Ubuntu server (once)

```bash
sudo bash bootstrap-ubuntu.sh --operator <your-ssh-user>
```

Installs updates, Docker Engine + Compose plugin from Docker's repository, Docker log
rotation (only if `/etc/docker/daemon.json` does not exist), the `aionix` group and the
directory layout. Options: `--no-upgrade`, `--configure-firewall` (UFW: allows OpenSSH
**first**, then 80/tcp, 443/tcp, 443/udp), `--install-backup-cron` (daily 03:15). It
never touches SSH configuration. Copy the script with `scp infra/scripts/bootstrap-ubuntu.sh
user@server:` first. Re-login afterwards so the docker group applies. The SSH user must
log in with a key (the deploy runs with `BatchMode`).

DNS: point the domain's A (and AAAA, if IPv6 is routed) record to the server and forward
80/443 to it if it sits behind a router. Check with the diagnostic script.

## 2. Deploy from Windows

```powershell
.\infra\deploy-server.ps1 -Server aionix@192.168.1.10 -Domain connect.example.com -AcmeEmail ops@example.com
```

Steps: SSH check → OS check (Ubuntu 24.04, override `-AllowOtherOs`) → Docker + layout
check → `git archive HEAD` upload to `releases/<version>` → on the server
(`deploy-remote.sh`): install compose/scripts, generate `config/.env` with random
passwords and a new signing key **only if none exist**, build the image, derive
`AIONIX_TOKEN_PUBLIC_KEYS`, backup (if a database is running), optional restore,
migrations (`aionix-migrate`), `docker compose up -d`, wait until api/signaling/relay/
postgres/redis report healthy, run the diagnostic. Uncommitted changes abort the deploy
(`-AllowDirty` to override). Later deploys need only `-Server`.

`-DryRun` prints every step without connecting (useful while the server is offline).
Other options: `-Port`, `-IdentityFile`, `-Site ':80' -PublicUrl http://ip` (LAN without
domain/TLS), `-SkipBackup`, `-HealthTimeoutSeconds`.

Rollback: previous releases stay in `releases/`; set `AIONIX_SOURCE_DIR` and
`AIONIX_VERSION` back in `.env` and `docker compose up -d` (migrations are forward-only;
restore the pre-deploy backup if a migration must be undone).

## 3. First organization and accounts

```bash
cd /opt/aionix-connect/compose
alias dca='docker compose --env-file ../config/.env --profile tools run --rm -T aionix-admin aionix-admin'
dca create-org --name "Minha Empresa"
read -rs PW && echo "$PW" | dca create-user --email admin@example.com --name "Administrador"
dca add-member --org <org-id> --email admin@example.com --role owner
dca enrollment-token --org <org-id> --hours 24 --uses 10
```

## Windows development / staging server

Requires Docker Desktop (WSL2). State lives in `infra/local/.staging` (gitignored).

```powershell
.\infra\local\staging.ps1 -Action Init      # config, random passwords, signing key
.\infra\local\staging.ps1 -Action Up        # build, migrate, start, wait for health
.\infra\local\staging.ps1 -Action Status
.\infra\local\staging.ps1 -Action Admin create-org --name "Teste"
.\infra\local\staging.ps1 -Action Backup
.\infra\local\staging.ps1 -Action Down
```

Endpoint: `http://<workstation-LAN-IP>:8080` (plain HTTP — staging only). PostgreSQL uses
a named Docker volume there (`aionix-postgres`); production uses `data/postgres`.

## Migrating staging → Ubuntu production

What must move and how:

| Item | How | Why |
| --- | --- | --- |
| PostgreSQL | `pg_dump` in the backup archive → `pg_restore` | accounts, devices, audit |
| Token signing key | in the archive, restored with `--with-key` | devices pinned its public key at enrollment; a new key would make every enrolled device reject tickets |
| `.env` | regenerated on Ubuntu (domain, paths, new passwords) | staging values (`:80`, Windows paths) do not apply |
| Secrets (DB/Redis passwords) | new ones on Ubuntu | the dump does not depend on them |
| Caddy data | not needed from staging (HTTP only) | production issues its own certificates |
| Redis | nothing | ephemeral |

```powershell
.\infra\local\staging.ps1 -Action Backup
.\infra\deploy-server.ps1 -Server aionix@server -Domain connect.example.com -AcmeEmail ops@example.com `
    -RestoreBackup .\infra\local\.staging\backups\aionix-backup-<UTC>.tar.gz
```

Enrolled devices keep their identities and registrations; they only need to be pointed
at the production server URL (the desktop applications that will hold this setting are
not built yet — the client library takes the URL as a parameter).

## Backups

```bash
bash /opt/aionix-connect/scripts/backup.sh            # → backups/aionix-backup-<UTC>.tar.gz
bash /opt/aionix-connect/scripts/restore.sh <archive> [--with-key] [--with-config] [--yes]
```

Archive = `postgres.dump`, `config/.env`, `config/secrets/token-signing.key`,
`caddy-data.tar.gz` (if available) and a `MANIFEST` with SHA-256 of each file (verified
on restore). Retention `AIONIX_BACKUP_KEEP` (14). With `AIONIX_BACKUP_PASSPHRASE_FILE`
the archive is AES-256 encrypted (openssl, PBKDF2). Restore takes a safety backup first,
replaces the database, applies migrations and starts the stack. Copy backups off the
server: they contain the signing key and passwords.

## Diagnostics

```bash
bash /opt/aionix-connect/scripts/aionix-diagnostico.sh [--sem-internet]
```

Reports (pt-BR) OS, Docker, PostgreSQL, Redis, API/signaling/relay readiness, local and
public IPs, IPv6, CGNAT (only asserted when it can be determined), DNS vs public IP, TLS
certificate expiry, listening ports, devices online, active sessions and relay
connections. Unknown values are shown as "Indisponível", "Não configurado" or "Não foi
possível determinar". Public IPs are discovered with `api.ipify.org`/`api64.ipify.org`
unless `--sem-internet` is given.

## Key rotation (manual)

1. `aionix-admin keygen --out new.key`; put the old public key in
   `AIONIX_TOKEN_PREVIOUS_PUBLIC_KEYS`, swap the key file, set `AIONIX_TOKEN_PUBLIC_KEYS`
   to "new,old"; restart api, signaling, relay.
2. Devices pin keys at enrollment; re-pinning on rotation is not implemented yet, so
   rotation currently requires devices to re-enroll. Avoid rotating unless compromised.
