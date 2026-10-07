# ADR-0002: iroh (QUIC) as the peer-to-peer transport backend

- Status: Accepted (recorded retroactively, 2026-10-07; implemented in `crates/transport`)

## Context

Sessions must work on the same LAN, across the internet through NATs, and through a
relay when no direct path exists, always end-to-end encrypted, and the peer that is
connected must be cryptographically the device the authority knows.

## Decision

Use [iroh](https://iroh.computer) (QUIC with hole punching and relay fallback) behind the
`aionix-transport` crate, the only crate that knows the backend.

- A device's Ed25519 identity key is its iroh endpoint id. The TLS 1.3 handshake
  therefore authenticates exactly the key the control plane registered
  (`PeerConnection::remote_key`).
- Traffic is end-to-end encrypted with forward secrecy; relays forward ciphertext only.
- Path selection (LAN → direct → relay) and path changes are handled by the backend;
  the application sees them through `PeerConnection::path()` diagnostics.
- Relays require an access token (relay grant).

## Consequences

- Application authorization (session tickets) is layered on top of an authenticated
  peer key (see ADR-0006).
- Swapping the backend later only touches `crates/transport`.
- Keepalive 2 s / idle timeout 20 s are transport constants; session-level reconnection
  (ADR-0008) covers connections that do die.
