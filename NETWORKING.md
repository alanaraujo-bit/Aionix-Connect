# Networking

## Paths

| Traffic | Path | Protocol | Encryption |
| --- | --- | --- | --- |
| REST (login, enrollment, tickets) | device → Caddy :443 → aionix-api | HTTPS | TLS (Caddy) |
| Signaling | device → Caddy :443 `/v1/signal` → aionix-signaling | WebSocket over TLS | TLS (Caddy) |
| Remote session, direct | technician ↔ host (LAN or hole-punched) | QUIC (iroh) | end-to-end TLS 1.3, keys pinned to device identities |
| Remote session, relayed | technician ↔ Caddy `/relay` ↔ aionix-relay ↔ host | QUIC packets inside the relay WebSocket | end-to-end between the peers; the relay sees ciphertext only |

The Control Plane never proxies remote desktop data; video, input and clipboard only use
the last two rows. Path selection (LAN → direct → relay) is automatic (ADR-0002,
`aionix-transport`) and reported as `TransportKind` in session events.

## Ports on the server

| Port | Exposure | Purpose |
| --- | --- | --- |
| 80/tcp | public | ACME HTTP-01 challenge, redirect to HTTPS (production); plain HTTP in staging |
| 443/tcp | public | API, signaling, relay (single hostname) |
| 443/udp | public, optional | HTTP/3 to Caddy |
| 22/tcp | operator | SSH (deployment) |
| 5432, 6379, 8080, 8081, 3340, 9090 | internal Docker network only | services |
| 9095/tcp | 127.0.0.1 only | Prometheus UI (profile `monitoring`) |

Staging on Windows publishes Caddy on 8080 (HTTP) by default.

## Single hostname

One DNS name (e.g. `connect.example.com`) serves everything; Caddy routes by path:
`/v1/signal` → signaling, `/relay`, `/ping`, `/generate_204` → relay, `/v1/*` and
`/healthz` → API. Devices receive the relay URL (`AIONIX_RELAY_URLS`, normally the same
https URL) together with a relay grant at device login. A separate relay hostname can
be configured later without code changes (relay URLs are configuration).

## Behind NAT / CGNAT

The server needs inbound 80/443. Behind a home/office router, forward those ports to
the server. Behind carrier-grade NAT (router WAN address in 100.64.0.0/10) inbound
connections are impossible on IPv4: use a public IPv4 from the ISP, IPv6 (publish an
AAAA record; Docker's default bridge does not route IPv6 inbound without extra
configuration — not set up yet), or a tunnel/VPS front (not part of this milestone).
`aionix-diagnostico.sh` reports what it can determine about CGNAT and never guesses.

## Client-side reachability

Devices connect outbound only (443). Their QUIC endpoint publishes its candidate
addresses (relay URL + direct addresses) to signaling in `presence`; the host repeats its
current address when accepting an offer, and the technician receives it with the ticket.
Relay QUIC address discovery (QAD) is not enabled on `aionix-relay` yet (ADR-0013).

## Reconnection behaviour

- Signaling links reconnect with jittered exponential backoff (0.5 s → 30 s), reset after
  a stable minute; the server's 1012 on restart, heartbeat loss and network errors all
  lead to reconnection; revocation and supersede stop it.
- Presence shows `Connecting` during the 30 s grace after an unexpected loss, then
  `Offline`; a killed signaling instance's records expire after 45 s.
- Remote sessions use the bounded technician-driven resume of ADR-0008, independent of
  signaling.

## Validation status

Relay forwarding and authorization were tested with real iroh endpoints on one machine
(loopback). No test across separate networks, real NATs or a deployed public relay has
been done yet.
