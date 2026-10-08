# ADR-0013: Standalone relay = iroh relay server + Aionix access control

- Status: Accepted
- Date: 2026-10-07
- Builds on: ADR-0002

## Context

The transport already uses iroh relays (in-process in tests). Production needs an
independent, authenticated relay reachable from the Internet.

## Decision

- `aionix-relay` embeds `iroh_relay::server::Server` (no protocol changes) with an
  `AccessControl` that admits an endpoint only with a relay grant (`axt1` type `relay`)
  signed by a trusted Control Plane key, unexpired, whose `key` equals the endpoint id
  proven in the relay handshake.
- Revocation before grant expiry (12 h): the Control Plane writes
  `<prefix>:revoked:<device>` in Redis; the relay refuses those devices. If Redis is
  unreachable the relay admits on signature alone (fail-open), counts it and warns:
  relay access only moves end-to-end encrypted packets — a session still needs a fresh
  ticket (refused for revoked devices) and visible local approval.
- Per-IP connection-attempt limit, global connection cap, optional per-client receive
  bandwidth cap (iroh's `ClientRateLimit`).
- Plain HTTP behind Caddy, which terminates TLS on the shared hostname (`/relay`,
  `/ping`, `/generate_204`). QUIC address discovery (QAD, UDP) is not enabled yet.
- Separate status listener: `/healthz`, `/readyz` (relay listener accepts TCP),
  `/metrics`, `/status` (active connections, bytes in/out, denials by reason).

## Consequences

- Without QAD, endpoints learn their public address from other means; NAT traversal may
  fall back to the relay more often. Enabling QAD needs UDP exposure and a TLS config
  for the QUIC listener.
- The relay holds no keys able to decrypt session traffic.
