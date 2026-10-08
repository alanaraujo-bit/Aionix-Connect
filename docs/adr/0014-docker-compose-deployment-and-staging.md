# ADR-0014: Docker Compose deployment; Windows Docker Desktop as temporary staging

- Status: Accepted
- Date: 2026-10-07

## Context

Production target: one bare-metal Xeon server, Ubuntu Server 24.04.5, Docker Engine and
Compose, no Kubernetes, no mandatory VPS. That server is temporarily unavailable;
development continues on a Windows workstation that must act as development/staging
server without introducing Windows-specific server dependencies.

## Decision

- One server image (`infra/docker/Dockerfile`, distroless `nonroot`, uid 65532) with all
  server binaries; Compose services choose the binary. Only Linux server crates are
  compiled in the image.
- One Compose file (`infra/compose/docker-compose.yml`) for production and staging; the
  env file is the only difference. Caddy is the only published service (80/443, TLS via
  Let's Encrypt in production, plain HTTP `:80` in staging). Services, PostgreSQL and
  Redis sit on an internal network.
- Server layout `/opt/aionix-connect/{compose,config,config/secrets,data,logs,backups,
  scripts,releases,current}`. The token signing key is a file mounted as a Compose
  secret, owned by uid 65532, group `aionix`, mode 0440 — never an environment variable.
- Releases are built on the server from `git archive HEAD` uploaded over SSH by
  `infra/deploy-server.ps1`, which then runs `infra/scripts/deploy-remote.sh` (backup →
  migrations → start → health). No registry needed.
- Redis is ephemeral (no RDB/AOF, no volume). PostgreSQL, the signing key, `.env` and
  Caddy data are the persistent state; `backup.sh`/`restore.sh` and the staging backup
  share one archive format, which is the migration path from staging to Ubuntu.
- On Windows staging PostgreSQL uses a named Docker volume (NTFS bind mounts break its
  permission checks); everything else is identical.

## Consequences

- Moving from staging to production = staging backup + `deploy-server.ps1
  -RestoreBackup`; the signing key must travel with the database, otherwise every
  enrolled device's pinned key is invalid.
- Builds take minutes on the server; a registry can be added later without changing the
  Compose file (the image name already carries the version).
