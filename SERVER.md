# Aionix Connect Server

The server side of Aionix Connect: Control Plane API, signaling, relay, PostgreSQL,
Redis and the Caddy reverse proxy. Deployment: [DEPLOYMENT.md](DEPLOYMENT.md). Security
model: [SECURITY.md](SECURITY.md). Network paths: [NETWORKING.md](NETWORKING.md).
Validation status: [PROJECT_STATUS.md](PROJECT_STATUS.md).

```text
                 Internet / LAN
                       │ 80, 443 (only published ports)
                ┌──────┴──────┐
                │    Caddy    │  TLS, routing, access log
                └─┬────┬────┬─┘
     /v1/*        │    │    │  /relay /ping /generate_204
   ┌──────────────┘    │    └──────────────┐
   ▼                   ▼ /v1/signal (WS)   ▼
aionix-api      aionix-signaling      aionix-relay    ◀── E2E-encrypted QUIC packets
(control plane) (presence, offers)    (data plane)        (never plaintext)
   │   │               │   │               │
   │   └──── Redis ────┘───┼───────────────┘  presence · pub/sub · rate limits · revocation
   └──────── PostgreSQL ───┘                   source of truth
```

Control plane and data plane are separate processes. The API and signaling only carry
JSON control traffic; screen, input and clipboard data go peer-to-peer or through the
relay, end-to-end encrypted between the two devices.

## Services

| Service | Binary | Port (internal) | Responsibility |
| --- | --- | --- | --- |
| `aionix-api` | `aionix-api` | 8080 | REST `/v1`: accounts, auth, devices, orgs, session tickets, audit; janitor |
| `aionix-signaling` | `aionix-signaling` | 8081 | `/v1/signal` device links, presence, offers, host session events |
| `aionix-relay` | `aionix-relay` | 3340 (+9090 status) | Authenticated iroh relay |
| `aionix-migrate` | `aionix-admin migrate` | — | One-shot schema migration before the services start |
| `aionix-admin` | `aionix-admin` | — | Operator CLI (Compose profile `tools`) |
| `postgres` | PostgreSQL 17 | 5432 | Source of truth (persistent) |
| `redis` | Redis 7.4 | 6379 | Ephemeral state, no persistence |
| `caddy` | Caddy 2.10 | 80/443 | Reverse proxy, automatic HTTPS |
| `prometheus` | optional | 127.0.0.1:9095 | Metrics (profile `monitoring`) |

Crates: `aionix-server-common` (config, keys, logging, health, rate limits, shutdown),
`aionix-control-plane` (API + store + presence + bus + admin), `aionix-signaling`,
`aionix-relay`, and `aionix-client` (used by the desktop apps).

## Health

Every service distinguishes **alive** from **ready**:

| Service | `/healthz` (alive) | `/readyz` (ready) checks |
| --- | --- | --- |
| API | process serving | PostgreSQL reachable **and** all migrations applied; Redis; offer subscription active |
| Signaling | process serving | PostgreSQL + migrations; Redis; device subscription active; not draining |
| Relay (`:9090`) | process serving | relay listener accepts TCP (Redis reported, not required) |

`/metrics` (Prometheus) on each; the relay adds `/status` (JSON: active connections,
bytes in/out, denials by reason). Only `/healthz` of the API is reachable through Caddy.
Docker health checks use the bundled `aionix-healthcheck` probe against `/readyz`.

## REST API (`/v1`)

| Method & path | Auth | Purpose |
| --- | --- | --- |
| `GET /server/info` | — | Server name, version, token public keys, relay URLs |
| `POST /auth/login` | — | E-mail + password → access (15 min) + refresh token |
| `POST /auth/refresh` | refresh token | Rotate; reuse of a rotated token revokes the login |
| `POST /auth/logout` | refresh token | Revoke that login |
| `POST /auth/logout-all` | access | Revoke every login of the caller |
| `GET /me` | access | Profile + memberships |
| `POST /devices/challenge` | — | Single-use nonce (60 s) |
| `POST /devices/register` | per kind | Enroll a public key (agent: enrollment token; technician: access token; quick support: open, rate limited) |
| `POST /devices/login` | signature | Device token (1 h) + relay grants (12 h) |
| `GET /devices/lookup/{friendly_id}` | access | Kind + presence; details only with `device.view` |
| `POST /devices/{id}/revoke` | access + `device.revoke` | Revoke an identity (propagated to signaling, relay, sessions) |
| `GET /orgs/{org}/devices` | `device.view` | Inventory with presence |
| `POST /orgs/{org}/enrollment-tokens` | `enrollment.create` | Token shown once (hash stored) |
| `GET/PUT /orgs/{org}/members` | `member.view` / `member.manage` | List; add account / change role |
| `GET /orgs/{org}/audit` | `audit.view` | Audit and security events |
| `POST /sessions` | access + workstation device token | Authorize and issue a session ticket (ADR-0012) |
| `GET /sessions` | access | Caller's sessions |
| `POST /sessions/{id}/events` | access (technician of the session) | Lifecycle report |

Errors use `aionix_control_api::ApiError` (`code`, `detail`, `retry_after`).

## Signaling (`GET /v1/signal`)

Bearer device token. Server → device: `ready`, `ping`, `session_offer`,
`session_revoked`, `device_revoked`. Device → server: `presence`, `pong`,
`session_offer_response`, `session_event`, `goodbye`. Close codes: 4000 heartbeat
timeout, 4001 token expired, 4002 superseded, 4003 revoked, 4008 policy, 1012 restart.
Details: ADR-0010 and `crates/control-api/src/signal.rs`.

## Roles and permissions

| Permission | viewer | technician | admin | owner |
| --- | --- | --- | --- | --- |
| `device.view`, `member.view` | ✔ | ✔ | ✔ | ✔ |
| `session.request`, `session.control`, `session.clipboard` | | ✔ | ✔ | ✔ |
| `device.manage`, `device.revoke`, `enrollment.create`, `member.manage`, `audit.view` | | | ✔ | ✔ |

Stored in `roles` / `permissions` / `role_permissions`; checked on every request.
Only owners grant or remove the owner role; nobody changes their own role.

## Configuration

All configuration comes from environment variables (Compose env file); the signing key
comes from a file. Each binary validates its settings at startup and exits with code 2
listing **every** problem (missing variables, invalid URLs, placeholder passwords copied
from `.env.example`), without printing secret values. Template:
[infra/compose/.env.example](infra/compose/.env.example).

| Variable | Used by | Meaning |
| --- | --- | --- |
| `AIONIX_DATABASE_URL` | api, signaling, admin | PostgreSQL URL (Compose builds it from `POSTGRES_PASSWORD`) |
| `AIONIX_REDIS_URL` | api, signaling, relay | Redis URL (from `REDIS_PASSWORD`) |
| `AIONIX_REDIS_PREFIX` | all | Key prefix (default `aionix`) |
| `AIONIX_TOKEN_SIGNING_KEY_FILE` | api | Ed25519 seed file (Compose secret) |
| `AIONIX_TOKEN_PUBLIC_KEYS` | signaling, relay | Trusted public key(s) |
| `AIONIX_TOKEN_PREVIOUS_PUBLIC_KEYS` | api | Retired keys still accepted during rotation |
| `AIONIX_RELAY_URLS` | api | Relay URL(s) handed to devices |
| `AIONIX_TRUSTED_PROXIES` | api, signaling | Networks whose `X-Forwarded-For` is believed |
| `AIONIX_ALLOW_QUICK_SUPPORT`, `AIONIX_OFFER_TIMEOUT_SECS` | api | Policy / offer wait |
| `AIONIX_SIGNAL_PING_SECS`, `…_HEARTBEAT_TIMEOUT_SECS`, `…_RECONNECT_GRACE_SECS` | signaling | 15 / 45 / 30 by default |
| `AIONIX_RELAY_*` | relay | Bind, status bind, connect limit, connection cap, client rx cap |
| `AIONIX_LOG`, `AIONIX_LOG_FORMAT` | all | Filter (`info,sqlx=warn`), `json` (default) or `pretty` |

## Operator CLI

```bash
docker compose --env-file /opt/aionix-connect/config/.env --profile tools run --rm aionix-admin aionix-admin <command>
```

`migrate`, `migration-status`, `keygen --out F`, `public-key --key-file F`,
`create-org --name N`, `create-user --email E --name N` (password from
`AIONIX_NEW_USER_PASSWORD` or stdin — never an argument), `add-member --org O --email E
--role R`, `enrollment-token --org O [--hours H --uses N]`, `disable-user --email E`,
`revoke-device --device ID`, `status [--json]`.

## Logging and audit

JSON logs on stdout (Docker rotates them: 20 MB × 5 per container). Request spans carry
`request_id` (also returned as `x-request-id`), method and path; signaling spans carry
`device_id` and `org_id`; audit lines carry user, device, organization and session ids.
Query strings, headers, tokens, passwords, keys, clipboard and screen content are never
logged. Audit events are stored append-only in PostgreSQL (see SECURITY.md).

## Local development

Requires Docker Desktop (WSL2 backend) on the Windows workstation.

```bash
docker compose up -d
```

```bash
cargo test --workspace
```

`compose.yaml` at the repository root starts development-only PostgreSQL
(`127.0.0.1:55432`, user/password `aionix`/`aionix-dev`) and Redis (`127.0.0.1:56379`).
Integration tests create a fresh database per test (migrations from empty) and a unique
Redis prefix. Without them, those tests print `SKIPPED` and pass; set
`AIONIX_REQUIRE_INTEGRATION=1` to make missing databases a failure. Override with
`AIONIX_TEST_DATABASE_URL` / `AIONIX_TEST_REDIS_URL`.

To run the complete stack (API, signaling, relay, Caddy) locally as the staging server:
`infra/local/staging.ps1` — see DEPLOYMENT.md.
