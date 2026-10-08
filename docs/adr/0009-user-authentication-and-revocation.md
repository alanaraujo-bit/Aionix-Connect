# ADR-0009: Password login, short access tokens, rotating refresh tokens, DB-checked revocation

- Status: Accepted
- Date: 2026-10-07

## Context

Technicians authenticate to the Control Plane. Logout, account disabling and theft
response must take effect immediately, and the design must stay open for MFA/SSO.

## Decision

- Passwords are hashed with Argon2id (default parameters, PHC strings) on the blocking
  pool. Unknown accounts are verified against a fixed dummy hash (no timing oracle).
  `users.password_hash` is nullable so future SSO-only accounts need no password.
- Login creates an `auth_sessions` row (one per login). The access token is the existing
  `axt1` token type `access` (`AccessClaims { sub, sid }`, 15 min). Every authenticated
  request re-reads the session row and the user: revoked/expired logins and disabled
  accounts are refused at once, not after the access token expires.
- Refresh tokens are opaque (`axr1.<sid>.<secret>`), stored as SHA-256 only, rotated on
  every use (sliding 14 days, absolute 30 days). Presenting the previous (already
  rotated) secret is treated as theft: the login is revoked and a `security` audit
  event is written.
- 10 consecutive failures lock the account for 15 min; logins are rate limited per IP
  and per account (Redis, keyed by a hash of the e-mail).
- MFA/SSO are not implemented. They would add steps before the `auth_sessions` row is
  created, without changing token formats.

## Consequences

- One indexed PostgreSQL read per authenticated request. Acceptable now; a short Redis
  cache can be added later without changing semantics if revocation is published.
- Refresh races from two concurrent clients of the same login look like reuse and
  revoke the login; clients must serialize refreshes (the client library does).
