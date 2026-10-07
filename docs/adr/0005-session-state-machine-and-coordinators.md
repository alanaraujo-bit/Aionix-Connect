# ADR-0005: Explicit session state machine and host/technician coordinators

- Status: Accepted
- Date: 2026-10-07

## Context

A remote session combines authorization, a human decision, transport, media, input,
clipboard and reconnection. Tracking that with scattered booleans makes illegal
combinations (input before approval, media after rejection, reconnecting a rejected
session) easy to introduce and hard to test.

## Decision

1. One explicit lifecycle, `session::state`: `Idle → Requesting → Authenticating →
   AwaitingApproval → Establishing → Connected ⇄ Degraded`, `Connected/Degraded →
   Reconnecting → Connected`, terminal `Rejected`, `Failed`, `Ended` (via `Ending`).
   A single pure `transition(state, event)` table defines every legal move; anything
   else is an `InvalidTransition` and leaves the state unchanged. Assisted sessions
   cannot reach `Establishing` except through `Approved`.
2. Two coordinators own the orchestration: `HostService`/host session and
   `TechnicianSession`. They depend on traits, not on subsystem internals:
   `AuthorizationProvider`, `TicketValidator`, `ApprovalPrompt`, `VideoSource`,
   `VideoDecoderFactory`, `FrameConsumer`, `HostInput(Factory)`, `ClipboardAccess`.
   Windows implementations live in `session::windows_media` and
   `approval::WindowsApprovalPrompt`.
3. Capture, encoder and decoder are thread-bound COM objects: each runs on a dedicated
   thread and exchanges plain data with the async session task through bounded
   channels. Framed control-stream reads (not cancellation safe) run in reader tasks;
   events carry a link generation so late events from a replaced connection are ignored.
4. A host serves one session at a time; further requests are refused with `BUSY`.

## Consequences

- State is observable (`watch` channel of `SessionStatus`) and testable in isolation.
- Subsystems can be replaced (e.g. a GPU renderer as `FrameConsumer`, the Control Plane
  as `AuthorizationProvider`) without touching orchestration.
- Multiple simultaneous sessions per host would need a new decision.
