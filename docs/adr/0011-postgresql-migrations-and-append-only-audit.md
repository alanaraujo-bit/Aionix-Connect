# ADR-0011: PostgreSQL as source of truth, explicit migrations, append-only audit

- Status: Accepted
- Date: 2026-10-07

## Context

Accounts, organizations, devices, permissions, sessions and audit need a durable,
transactional store with controlled schema evolution.

## Decision

- PostgreSQL 17 via `sqlx` 0.9 with runtime-checked queries (no database needed at
  compile time). Migrations live in `crates/control-plane/migrations` and are embedded
  in the binaries.
- Services never migrate on start. `aionix-admin migrate` (Compose service
  `aionix-migrate`, one-shot) applies them under sqlx's advisory lock, and the services
  start only after it succeeded. Readiness (`/readyz`) fails while any embedded migration
  is missing, so an old schema is never served.
- Roles, permissions and their mapping are data (`roles`, `permissions`,
  `role_permissions`), seeded by the migration; memberships reference roles. Checks read
  the database on every request.
- `audit_events` is append-only: triggers refuse UPDATE, DELETE and TRUNCATE. One table
  holds both normal audit (`category = 'audit'`) and defensive security events
  (`category = 'security'`). Writing the audit record is part of the action; a failed
  write fails the request.
- Secrets are never stored in clear: refresh tokens, enrollment tokens and ticket ids are
  stored as SHA-256; device private keys never reach the server.

## Consequences

- Removing audit history requires a superuser dropping the triggers — deliberate.
- Backups are logical (`pg_dump` custom format), portable between the Windows staging
  host and the Ubuntu production host.
