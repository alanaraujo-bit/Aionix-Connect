# ADR-0008: Bounded, technician-driven reconnection with rotating resume tokens

- Status: Accepted
- Date: 2026-10-07

## Context

QUIC already survives path changes within its idle timeout, but connections still die
(Wi-Fi drop, NAT rebinding, sleep). A short interruption should not require a new ticket
and a new approval, yet a dropped session must not linger forever or be resumable by
anyone else.

## Decision

- On approval the host issues an opaque resume token (32 random bytes; only its SHA-256
  is kept). Every successful resume rotates it.
- On unexpected loss (no `Goodbye`), both sides enter `Reconnecting`. The host releases
  every held key/button, pauses capture and waits `resume_grace` (default 30 s); after
  that the session is `Failed`.
- The technician alone drives resumption: at most `max_attempts` (default 6), exponential
  backoff from 250 ms to 4 s with ±20 % jitter, each attempt bounded by
  `attempt_timeout`. One reconnection loop per session; a definitive refusal stops it.
- A resume is accepted only from the same authenticated technician key, with the current
  token, while the session is `Reconnecting` (or still live, in which case the old
  connection is superseded). It does not ask the local user again.
- After resume the host resends the stream description and restarts from a keyframe;
  input queued on the technician while disconnected is discarded, never replayed.

## Consequences

- `reconnect_count` and the current episode's attempts are exposed in `SessionStatus`
  and diagnostics.
- Approval is not repeated for resumes inside the grace window; that is the reason for
  binding the token to the technician key and keeping the window short.
