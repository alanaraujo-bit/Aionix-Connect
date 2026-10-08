# ADR-0010: Standalone signaling with Redis-backed, connection-owned presence

- Status: Accepted
- Date: 2026-10-07

## Context

Technicians must see truthful Online / Connecting / Offline states. Presence must
survive signaling restarts and reconnects, never outlive its evidence, and work with
more than one signaling instance. Signaling must never carry screen content.

## Decision

- `aionix-signaling` is its own process. Devices keep one WebSocket (`/v1/signal`,
  bearer device token) carrying only JSON control messages.
- Presence lives in Redis: one record per device with a TTL equal to the heartbeat
  timeout (45 s), owned by the link's connection id. Lua scripts make every update
  conditional on ownership, so a late heartbeat or close of a superseded link cannot
  change the state set by the newer link.
- States: `Connecting` (authenticated, reachability not yet published; or lost
  unexpectedly within the 30 s reconnect grace), `Online` (published and heartbeat on
  time), `Offline` (no record). `Goodbye` and revocation remove the record at once.
  A killed instance stops refreshing, so its records expire after the TTL.
- Heartbeat: server `Ping` every 15 s; any client message refreshes the TTL; silence
  longer than 45 s closes the link (4000). Links also close when the device token
  expires (4001), when superseded (4002), on revocation (4003) and on restart (1012).
- API ↔ signaling messages travel over Redis pub/sub (`<prefix>:dev:<id>`,
  `<prefix>:offer:<session>`), so any instance can hold any device's link.
- `devices.last_seen_at` in PostgreSQL is updated on connect/disconnect and at most once
  a minute; it is history, not presence.

## Consequences

- A Redis restart briefly shows devices offline until their next heartbeat recreates
  the records (≤ 15 s). Redis therefore needs no persistence.
- Pub/sub is not durable: an offer lost during a Redis outage surfaces as a timeout to
  the technician, which is the truthful outcome.
