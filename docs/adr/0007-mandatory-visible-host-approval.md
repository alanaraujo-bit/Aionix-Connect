# ADR-0007: Mandatory visible approval on the host for assisted sessions

- Status: Accepted
- Date: 2026-10-07

## Context

Aionix Connect is consent-based remote support. A person in front of the host must know
that a technician is asking for access and must actively allow it.

## Decision

- After a valid ticket, the host shows an `ApprovalPrompt` and nothing else happens until
  it answers. The state machine has no path from `Authenticating` to `Establishing` that
  skips `Approved`. Capture, input and clipboard start only after approval.
- `request_approval` enforces the timeout itself (prompt timeout + 0.5 s); an unanswered,
  failed or unavailable prompt is a refusal (fail-closed). Dropping the request (the
  technician cancelled or disconnected, the host is shutting down) withdraws the prompt.
- The Windows prompt (`aionix_platform::windows::consent`) is a top-most window in
  Brazilian Portuguese: title from the brand file, "Um técnico está solicitando acesso a
  este computador.", technician name / e-mail / organization from the signed ticket
  (control and bidi-override characters stripped, length bounded, `&` shown literally),
  what the technician will be able to do, a live countdown, and `[Recusar] [Permitir]`.
  "Recusar" has the focus and is the default (Enter, Esc, Alt+F4 and the close box all
  refuse). "Permitir" is disabled for the first 1.5 s and only a real `BN_CLICKED` from
  the enabled button counts.
- No auto-approving implementation exists in the product code; the approval doubles live
  in the test suite.
- Unattended access was out of scope here. ADR-0015 adds it as an explicit, per-computer
  opt-in by the owner; assisted sessions keep the visible approval above unchanged.

## Consequences

- Assisted sessions always involve a human decision on the host; the only exception is a
  resume of an already approved session within its grace window (ADR-0008).
- A persistent on-screen "session active" indicator with an end button for the host user
  is still to be built (see PROJECT_STATUS.md).
