# Project status

Last updated: 2026-10-12 — unattended access and organization (see below); previous milestone 2026-10-07 — "Control Plane + Server foundation" (branch
`milestone/control-plane-server`, built on "Remote Session Foundation").

Legend: **IMPLEMENTED** = code exists and is wired; **TESTED** = covered by automated
tests that actually ran (or the manual check named); **NOT YET VERIFIED** = written and
compiled, tests exist but could not run in this environment; **PARTIAL** = works with
stated gaps; **BLOCKED** = needs something external; **PLANNED** = not started.

Environment: one Windows 11 workstation. Docker Desktop/WSL2, PostgreSQL and Redis are
**not installed yet**, so nothing that needs a database or a container has been
executed. The dedicated Ubuntu server is temporarily unavailable; the workstation is the
intended development/staging server once Docker Desktop is installed.

## Update 2026-10-12 — unattended access, clients and groups

Decision: ADR-0015 (unattended access with explicit owner opt-in and a device password).
Amends ADR-0007; PRODUCT.md principle 1 now reads "visible and revocable".

Implemented:

- Control Plane: migration `20261012000001_device_management` (device groups, customer
  notes, device description/favorite/technician, unattended-password hash; drops the
  never-written `devices.unattended_enabled`). Endpoints for customers, groups, device
  organization (`PATCH /v1/orgs/{org}/devices/{id}`) and the unattended password
  (`PUT`/`DELETE .../unattended-password`, permission `device.unattended`, admin/owner).
- Session creation with `unattended_password`: refused unless the owner's live consent is
  on in presence, a password is set, and the password matches (Argon2id, 10 tries/min per
  device). Ticket and session mode are `unattended`; the host and the audit log record it.
- Host (desktop): "Permitir acesso sem aceitação" in "Este computador" (managed computers,
  default off, kept in `host-settings.json`). While on, unattended offers and tickets are
  accepted; the consent prompt is skipped only for unattended tickets; a banner with
  "Encerrar acesso" shows the session; turning the setting off ends a running unattended
  session.
- Technician app: connecting to a computer with unattended access available asks for the
  device password ("Pedir permissão" keeps the normal request).
- Admin: "Editar computador" (context menu on the list) for customer, group, technician,
  description, favorite, and the unattended password; "Clientes e grupos" to create and
  delete (deleting unlinks computers, it never deletes them).
- Installer: "Iniciar com o Windows" was already on by default, so managed computers are
  reachable after a restart.

Verified (ran):

- `AIONIX_REQUIRE_INTEGRATION=1 cargo test --workspace --no-fail-fast` against PostgreSQL
  17.5 and Redis 3.0 (local, `.devtools/`): **164 passed, 0 failed**. Includes the new
  `tests/inventory.rs` (3 tests: customers/groups/organization; organization boundaries and
  permissions; the full unattended flow, each refusal, and clearing the password).
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` clean on the changed
  crates; `cargo check --workspace --all-targets` clean.
- UI previews (`ui_preview`) rendered and reviewed in the light theme: device edit (two
  columns, fits without scrolling), clients and groups, owner banner and switch. Dark
  theme not reviewed.

Not verified:

- Two real computers on two networks with unattended access. The host consent path runs
  only on Windows with a display.
- `direct_connection_is_used_on_lan` (transport) is environment-dependent: it failed once
  and passed once in the same run set, because the machine also has a VPN address that
  the path selection can classify as direct. The transport crate is untouched by this
  change.
- Tray indicator and Windows notification for unattended sessions are not built yet
  (ADR-0015 consequences).

## Update 2026-10-08 — first running, user-testable build

Workstation: WSL2 platform features + WSL + Docker Desktop 4.94 installed
(`infra/local/setup-workstation.ps1`, one UAC approval); **a reboot is pending** before
Docker can run. `infra/local/after-reboot.ps1` is registered (HKCU RunOnce) to start
Docker, run the dev databases, the full test suite with integration required, and the
staging stack after that reboot.

Until then the server runs natively (`infra/local/native-stack.ps1`, DEVELOPMENT ONLY):
the real aionix-api / aionix-signaling / aionix-relay binaries, official PostgreSQL 17.11
Windows binaries, and Microsoft's archived Redis 3.0 for Windows (NOT the validated Redis
7.4 — final Redis validation happens on Docker).

Ran and verified on this machine:
- `AIONIX_REQUIRE_INTEGRATION=1 cargo test --workspace` against real PostgreSQL 17 +
  Redis 3.0: all integration tests ran (none skipped); one real bug found and fixed
  (presence TTL expired before heartbeat-timeout detection, so devices skipped the
  `Connecting` grace state — see ADR-0010).
- New desktop apps (`crates/desktop`): **Aionix Connect** (host), **Aionix Connect
  Técnico**, **Dev Launcher**. Full flow with real components: login → workstation
  enrollment → presence Online → session request with real Control Plane permission
  check → signed ticket → host `TicketValidator` → real consent window ("Permitir"
  clicked by a person) → DXGI/WGC capture → Intel Quick Sync H.264 → Media Foundation
  decode → rendered in the Technician (1920×1080, ~36 fps release build) → ended from
  either side; session history (requested/connected/ended) and audit rows in PostgreSQL.
- Internet path: temporary Cloudflare quick tunnel → local Caddy → API/signaling/relay
  (`infra/local/public-tunnel.ps1`); a packaged client (`dist/Aionix-Connect-Cliente.zip`)
  enrolled and went Online through the public address; a session through it connected.
  Both ends were still this same PC — **cross-network media is not validated** until the
  test with a second network.

Still to run after the reboot (Docker): Redis 7.4 integration run, Compose startup and
health, restart tests, `staging.ps1 Backup → Destroy → Restore` round trip.

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
  relay, tray indicator for unattended sessions, billing (not
  started, by decision).
