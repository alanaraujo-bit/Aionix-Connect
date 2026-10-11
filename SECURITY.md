# Security

Aionix Connect is consent-based remote support software. It does not implement — and
the server does not enable — stealth access, hidden persistence, credential collection,
antivirus or security-product evasion, or unattended access without explicit consent.
Every session still needs the visible "Permitir" click on the host (ADR-0007); the
Control Plane adds authorization on top of that, it never replaces it.

## Identities and secrets

| Secret | Where it lives | Never |
| --- | --- | --- |
| Device Ed25519 private key | on the device (DPAPI-protected on Windows) | sent to the server, logged |
| Control Plane signing key | `config/secrets/token-signing.key` (uid 65532, group aionix, 0440), Compose secret | in env vars, git, logs |
| User passwords | Argon2id PHC hash in `users.password_hash` | stored or logged in clear; passed as CLI args |
| Refresh tokens | client; server keeps SHA-256 (current + previous) | stored in clear |
| Enrollment tokens | shown once; server keeps SHA-256 | stored in clear |
| Session tickets | technician → host; server keeps SHA-256 of the token id | stored |
| DB/Redis passwords | `config/.env` (0600) | committed (`.gitignore` excludes `.env*` except examples) |

Friendly 9-digit ids are lookup handles only: no check anywhere accepts them as proof
of anything.

## Tokens (unchanged `axt1` format, ADR-0006)

| Type | Lifetime | Bound to | Verified by |
| --- | --- | --- | --- |
| `access` | 15 min | user + login session (re-checked in PostgreSQL per request) | API |
| `device` | 1 h | device id + key | API (workstation header), signaling |
| `relay` | 12 h | device id + key = relay handshake identity | relay |
| `session_ticket` | 90 s | session, host id+key, technician user+device+key, mode, permissions | host (`TicketValidator`) |

Distinct types: a token of one type is rejected everywhere else (tested).

## Authentication

Argon2id; constant-cost check for unknown accounts; generic error for wrong password,
unknown account or disabled account; lockout after 10 consecutive failures (15 min);
rate limits per IP and per account; rotating refresh tokens with reuse detection
(revokes the login, `auth.refresh_reuse` security event); logout, logout-all and
account disabling take effect immediately. MFA/SSO: not implemented (ADR-0009).

## Device enrollment

Proof of possession: the device signs a single-use, 60-second server nonce with its key
under a dedicated signature domain (registration and login domains are not
interchangeable). Agents need an organization enrollment token (hashed, expiring,
limited uses, consumed atomically); technician workstations need a signed-in user and
become owned by that user; quick-support registrations are open but rate limited and
belong to no organization. Re-registering an existing key → 409; a revoked key → 403.

## Session authorization

See ADR-0012: user, workstation ownership, target validity, organization permission,
role-limited capabilities, presence. Assisted mode only. Host-side checks (signature,
expiry, both keys, session id pinning, replay) are unchanged.

## Revocation

Device revocation (API or CLI) sets `revoked_at` (login, signaling, enrollment and
session requests refuse it), fails its active sessions, tells hosts to drop sessions of
a revoked workstation, closes the live signaling link (`DeviceRevoked`, 4003) and writes
a Redis marker that the relay checks. If Redis is down the relay admits on signature
alone until the grant expires (≤ 12 h) — accepted because relay access only forwards
end-to-end encrypted packets and every session still needs a new ticket (refused) and
local approval. Account disabling revokes all logins.

## Rate limiting

API (Redis fixed windows shared by instances, local fallback if Redis fails): global
600/min per IP; login 20/min per IP and 10/min per account; refresh 60/min per IP;
challenges and device logins 60/min per IP; registrations 30/h per IP (quick support
10/h); session requests 30/min per user; lookups 60/min per user; administration (create,
reset, sign-out, disable, enable, terminate) 30/min per user; own password changes
5/min per user. Signaling: 30
connection attempts/min per IP, 20 messages/s per link, 16 KiB per message. Relay:
connection attempts per IP (default 120/min), connection cap, optional bandwidth cap.
Client IPs come from `X-Forwarded-For` only when the peer is a trusted proxy.

## Audit (append-only)

`audit_events` refuses UPDATE/DELETE/TRUNCATE (triggers). Recorded actions:

| Action | When |
| --- | --- |
| `auth.login` (success/failure/denied), `auth.logout`, `auth.logout_all` | user authentication |
| `auth.password_changed` (success/failure) | own password change (other logins signed out) |
| `auth.account_locked`, `auth.refresh_reuse` (security) | defensive events |
| `device.enrolled`, `device.enroll_refused` (security), `device.login_refused` (security) | enrollment / device login |
| `device.revoked` | revocation |
| `enrollment.token_created`, `member.upserted`, `org.created`, `user.created`, `user.disabled` | administration |
| `user.enabled`, `user.signed_out`, `user.password_reset`, `user.email_changed`, `session.terminated` | administration panel and `aionix-admin` |
| `permission.denied` | any refused permission, workstation mismatch, non-peer session report |
| `session.requested` (success/denied), `session.ticket_issued`, `session.offer_answered` | ticket flow |
| `session.approved`, `session.rejected` | host user's consent decision (reported by the host) |
| `session.ended` | end / failure |
| `signaling.refused` (security) | revoked device trying to connect |

Records hold ids, outcome, client IP, request id and small metadata — never passwords,
tokens, keys, clipboard text or screen content (tests check that a password typed at
login, a temporary password and a new password never appear in audit details).

## Administration panel (owners and admins)

Accounts are created from the app by an organization's owners and admins; there is
still no public sign-up. The server enforces (tests in
`crates/control-plane/tests/admin.rs`):

- every route checks one permission in the organization of the path (`member.manage`,
  `audit.view`, and `session.view_all` / `session.terminate`, held by admins and owners);
- account-wide actions (reset password, sign out everywhere, disable, enable) only reach
  members of that organization who belong to no other organization, never the caller,
  and never someone of equal or higher role unless the caller is an owner; a new
  account's role cannot exceed the caller's; only owners grant `owner`; the last enabled
  owner can be neither disabled nor demoted;
- new and reset accounts get a server-generated temporary password (16 symbols without
  look-alikes, ~82 bits), returned once and never logged or audited; until the account
  chooses its own (`POST /v1/auth/password`, current password required, 12+ characters,
  every other login signed out) the server refuses its session requests and technician
  workstation registration, whatever client it uses;
- disabling or signing out an account revokes its logins and ends its live sessions;
  ending a session tells both computers (`SessionRevoked`), and app versions from 0.2.6
  stop it at once.

## Logging

Structured JSON. Never logged: passwords, private keys, token values, authorization
headers, query strings, clipboard contents, screen contents. Connection URLs are
redacted (`postgres://aionix:***@…`).

## Containers and network

Distroless image, non-root uid 65532, read-only root filesystem, all capabilities
dropped, `no-new-privileges`. Only Caddy publishes ports; API, signaling, relay,
PostgreSQL and Redis share an internal Docker network without external routing. Redis
requires a password. The bootstrap never changes SSH settings; the optional UFW
configuration allows OpenSSH before enabling the firewall.

## Known gaps

- Host replay guard is in memory (ADR-0006) — persistence not implemented yet.
- Token key re-pinning on rotation is not implemented (rotation needs re-enrollment).
- No MFA/SSO yet; no admin UI (CLI only).
- Relay QUIC address discovery disabled; relay revocation is fail-open when Redis is down.
- The full stack has not yet run under Docker; integration tests against real
  PostgreSQL/Redis have not been executed (see PROJECT_STATUS.md).
