# ADR-0006: Signed, short-lived session tickets bound to both peers' keys

- Status: Accepted
- Date: 2026-10-07

## Context

The host must decide whether an incoming, transport-authenticated peer may start a
session. The Control Plane (PostgreSQL-backed) that will make that decision does not
exist yet, but the host-side checks must already be final.

## Decision

- The authority issues an `axt1` token of type `session_ticket`
  (`SessionTicketClaims`, TTL 90 s) signed with its Ed25519 key. It binds session id,
  technician (user id, workstation device id and endpoint key, display name, e-mail,
  organization), target device (device id and endpoint key), issue/expiry time, access
  mode and permissions.
- The host (`TicketValidator`) accepts it only if: signed by a pinned authority key with
  the right type and not expired (30 s leeway); `host.key`/`host.device_id` are its own;
  `viewer.key` equals the key the QUIC handshake authenticated; the session id is not nil
  and, when the authority announced sessions, is one of them; the mode is allowed
  (assisted only for now); `view` is granted; neither the token id nor the session id was
  consumed before. Only fully valid tickets are consumed, so a stolen ticket presented by
  the wrong peer does not burn the legitimate one.
- Friendly 9-digit ids are lookup handles: the authority resolves them to a device id
  and key, which the ticket binds. No check uses them.
- Technicians obtain tickets through the `AuthorizationProvider` trait. The only
  implementation today is `DevelopmentAuthority`, compiled only with the
  `dev-authority` feature (enabled for tests and examples), logged as DEVELOPMENT ONLY at
  construction and performing no real authorization.

## Consequences

- The replay guard is in memory: after a host restart a ticket could be presented again
  inside its 90 s lifetime, still only by the bound technician key and still subject to a
  fresh local approval. Persisting it is planned with the Control Plane integration.
- The Control Plane client will implement `AuthorizationProvider` and must derive the
  technician identity from the authenticated API session, never from client input.
