# Project status

Last updated: 2026-10-07 — milestone "Control Plane + Server foundation" (branch
`milestone/control-plane-server`, built on "Remote Session Foundation").

Legend: **IMPLEMENTED** = code exists and is wired; **TESTED** = covered by automated
tests that actually ran (or the manual check named); **NOT YET VERIFIED** = written and
compiled, tests exist but could not run in this environment; **PARTIAL** = works with
stated gaps; **BLOCKED** = needs something external; **PLANNED** = not started.

Environment: one Windows 11 workstation. Docker Desktop/WSL2, PostgreSQL and Redis are
**not installed yet**, so nothing that needs a database or a container has been
executed. The dedicated Ubuntu server is temporarily unavailable; the workstation is the
intended development/staging server once Docker Desktop is installed.

## IMPLEMENTED

Remote Session Foundation (unchanged): see ARCHITECTURE.md — capture, codec, input,
clipboard, QUIC transport with relay, state machine, tickets, visible approval,
coordinators, reconnection, diagnostics.

Control Plane + Server:

- `aionix-api`: password login (Argon2id, lockout), 15-min access tokens re-checked
  against the login session, rotating refresh tokens with reuse detection, logout,
  logout-all; device challenge/register/login (proof of possession, enrollment tokens,
  technician ownership, open+rate-limited quick support); device lookup; device
  revocation with propagation; organization devices with presence, enrollment tokens,
  members, audit listing; session request with full authorization and ticket issuance
  (ADR-0012); technician session events; janitor (expired and stale sessions,
  challenges).
- PostgreSQL schema + migrations (accounts, login sessions, organizations, roles,
  permissions, memberships, customers, enrollment tokens, devices, challenges, sessions,
  session events, append-only audit).
- Presence in Redis with connection ownership (Online/Connecting/Offline); Redis pub/sub
  bus; shared rate limits with local fallback; relay revocation markers.
- `aionix-signaling` as a standalone service (heartbeat, timeouts, token expiry,
  supersede, revocation, offers, host events, graceful drain, multi-instance).
- `aionix-relay` as a standalone service (signed grants bound to the handshake key,
  revocation, connection limits, bandwidth cap option, status/metrics/health).
- `aionix-admin` CLI (migrations, keygen, bootstrap of orgs/users/memberships,
  enrollment tokens, disable user, revoke device, status).
- `aionix-client`: identity store (DPAPI on Windows), enrollment, device sessions,
  signaling client with reconnect, `ControlPlaneAuthority` (production
  `AuthorizationProvider`), `HostOfferHandler`.
- Health: `/healthz` vs `/readyz` (DB reachable + migrated, Redis, subscriptions),
  `/metrics`, relay `/status`; `aionix-healthcheck` probe.
- Docker image, production/staging Compose stack (Caddy, internal network, one-shot
  migrations, health-gated startup), dev `compose.yaml`, Windows staging script,
  Windows→Ubuntu SSH deployment, Ubuntu bootstrap, config generation, backup/restore
  (staging→production migration path), pt-BR diagnostic script.
- Docs: SERVER.md, DEPLOYMENT.md, SECURITY.md, NETWORKING.md, ADR-0009 … ADR-0014.

## TESTED (ran)

`cargo test --workspace`: **111 tests executed and passed**, 2 ignored manual window
tests; plus 24 integration tests that were **skipped** (reported as passing because they
return early without databases — see below).

- The **84 Remote Core tests** of the previous milestone all still pass (including
  `session_e2e` with real capture/encode/decode).
- New, executed: server-common (8: config validation without secret leakage, key files,
  readiness aggregation, trusted proxies, rate limiter), control-plane units (12:
  Argon2id hashing/policy, refresh token format, config validation, presence timeout
  and reconnect semantics on the in-memory store, bus delivery and offer timeout,
  session status transitions), client units (4, incl. a real Windows DPAPI identity
  roundtrip), relay (3, real iroh endpoints through the real relay service: authorized
  end-to-end echo over the relay with metrics/status/readiness; stolen, forged, expired
  and wrong-type grants refused; connection cap enforced).
- `cargo fmt --check` and `cargo clippy --workspace --all-targets --all-features -D
  warnings`: clean.
- Scripts: PowerShell scripts parse; `bash -n` passes for every shell script;
  `deploy-server.ps1 -DryRun` walks all steps and rejects shell metacharacters in
  arguments; `git archive` release packaging verified.

## NOT YET VERIFIED (needs Docker Desktop: `docker compose up -d`)

Written, compiled, and run only as far as their skip guard:

- Migrations from an empty database and idempotent re-run; append-only audit triggers;
  constraints (`tests/migrations.rs`).
- Authentication: login, wrong password, unknown account, refresh rotation, reuse →
  revocation, logout/logout-all, expired login session, disabled account, lockout,
  health readiness with and without PostgreSQL (`tests/auth.rs`).
- Device registration, duplicate registration, exhausted/forged enrollment tokens,
  invalid identities (wrong key, wrong domain, reused nonce, malformed key), device login
  tokens and relay grants, revocation and its permission check (`tests/devices.rs`).
- Session tickets bound to both keys, permission intersection, viewer denial, wrong
  organization, foreign workstation token, technician-as-target, unknown/busy/offline/
  revoked targets, revoked workstation, host refusal, offer timeout, janitor
  (`tests/sessions.rs`).
- Presence timeout/reconnect against real Redis (`tests/presence_redis.rs`).
- Signaling authentication, heartbeat timeout, goodbye, abrupt drop, supersede, full
  offer flow over WebSocket, host events, revocation of a live link, restart with
  reconnect to a new instance (`crates/signaling/tests`).
- Client end-to-end: enrollment from an identity file, presence, Control Plane ticket
  validated by the real `TicketValidator`, signaling restart with automatic reconnect,
  revocation (`crates/client/tests/e2e.rs`).
- Relay refusing a revoked device via the Redis marker.
- Docker image build, Compose startup, health checks, service restart, staging script,
  backup/restore round trip, diagnostic script on Linux, deployment to Ubuntu.

To verify: install Docker Desktop, then `docker compose up -d` and
`AIONIX_REQUIRE_INTEGRATION=1 cargo test --workspace`; then
`infra/local/staging.ps1 -Action Up`.

## PARTIAL

- Remote Core gaps from the previous milestone remain (see git history of this file):
  input/clipboard OS adapters not in automated session tests, no host session indicator,
  in-memory replay guard, single display, untested secure desktop, no GUI renderer.
- Session events are reported by the client library (`SignalingClient::report`,
  `ApiClient::report_session_event`) but the host/technician coordinators do not call
  them automatically yet (no application wires remote-core to the client library).
- Token key rotation: verification of several keys works; devices cannot re-pin keys,
  so rotation requires re-enrollment.
- Relay: QUIC address discovery disabled; revocation fail-open while Redis is down.
- Customers table exists; no API to create customers (enrollment tokens accept an
  existing customer id).

## BLOCKED

- Cross-internet validation (separate networks, real NAT, public relay): needs the
  Ubuntu server (or staging exposed with a public address) and a second network.
- Production deployment: Ubuntu server unavailable.

## PLANNED

- Technician/host applications using `aionix-client` + `aionix-remote-core`.
- MFA/SSO, admin web interface, persistent host replay guard, key re-pinning, QAD on the
  relay, customer management, unattended access (explicitly out of scope), billing (not
  started, by decision).
