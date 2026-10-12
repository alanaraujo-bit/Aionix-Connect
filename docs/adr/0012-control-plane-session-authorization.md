# ADR-0012: Control Plane session authorization replaces the development authority

- Status: Accepted
- Date: 2026-10-07
- Builds on: ADR-0006 (ticket format and host checks unchanged)

## Context

`DevelopmentAuthority` signed tickets for anyone. Production needs a real decision
based on authenticated users, workstations, organizations, permissions and presence.

## Decision

`POST /v1/sessions` issues a ticket only if all of these hold:

1. bearer access token valid, login session active, account enabled (ADR-0009);
2. workstation device token valid; the workstation is a `technician` device, not
   revoked, owned by that user, with the token's key — this key becomes `viewer.key`,
   which the host compares with the QUIC peer;
3. the friendly id resolves to a device that exists, is not revoked and can host;
4. authority: for organization devices, the user holds `session.request` in the
   device's organization; for unmanaged quick-support devices (no organization), the
   user holds `session.request` in some organization (the host user's visible consent
   is then the grant, ADR-0007);
5. permissions = requested ∩ role (`view` mandatory; `control`/`clipboard` need
   `session.control`/`session.clipboard`; file transfer and restart are never granted);
6. presence is `Online` and not busy.

The server then signs `SessionTicketClaims` (90 s, assisted mode only), stores the
session row (SHA-256 of the ticket id, never the ticket), audits `session.requested` and
`session.ticket_issued`, sends `SessionOffer` to the host over signaling and waits up to
15 s for its answer (accept + current address / busy / policy). Denials are audited as
`permission.denied` or `session.requested` with outcome `denied`.

Clients use `aionix_client::authority::ControlPlaneAuthority`, an implementation of the
existing `AuthorizationProvider` trait. `DevelopmentAuthority` stays behind the
`dev-authority` feature for tests and examples only.

## Consequences

- No host-side change was needed: the host keeps validating tickets exactly as before,
  plus the session id pinning it already supported (`expect_session`), now fed by
  signaling offers (`HostOfferHandler`).
- Unattended tickets exist only with the device password and the owner's live consent
  (ADR-0015); the host refuses them unless its own setting allows them.
