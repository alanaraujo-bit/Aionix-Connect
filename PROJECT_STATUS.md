# Project status

Last updated: 2026-10-07 — milestone "Remote Session Foundation".

Legend: **IMPLEMENTED** = code exists and is wired; **TESTED** = covered by automated
tests (or the manual check named); **PARTIAL** = works with stated gaps; **BLOCKED** =
cannot progress without something external; **PLANNED** = not started.

Validation environment so far: one Windows 11 machine, both peers in one process,
in-process relay. **No cross-internet (separate networks) validation has been done.**

## IMPLEMENTED

- Workspace, branding, shared types, Ed25519 identities, typed expiring signed tokens.
- Wire protocol (protobuf + bounded framing); additive `SenderReport.capture_backend`
  and `SenderReport.congested`.
- iroh/QUIC transport: authenticated, end-to-end encrypted; LAN/direct/relay; relay
  token access.
- Windows capture (DXGI → WGC → GDI, cursor), D3D11 conversion, Media Foundation H.264
  encode/decode, input injection with held-key tracking, clipboard, DPAPI, system info.
- Adaptive quality controller, now driven by the host session (bitrate, fps, resolution
  with new stream epochs, frames-in-flight backpressure).
- Session state machine (`session::state`) with explicit transition table.
- Session tickets: host `TicketValidator` (signature, type, expiry, target device,
  technician key, session id, mode, permission, replay guard).
- `AuthorizationProvider` interface; `DevelopmentAuthority` (DEVELOPMENT ONLY, behind the
  `dev-authority` feature).
- Approval abstraction with enforced timeout; native Windows consent window in pt-BR
  (`[Recusar] [Permitir]`, countdown, delayed arming of "Permitir", fail-closed).
- Host coordinator (`HostService`): validation → approval → media/input/clipboard →
  lifecycle → teardown; single session, `BUSY` for others.
- Technician coordinator (`TechnicianSession`): ticket → connect → approval wait →
  video decode → frame consumer, input, clipboard, reports, teardown.
- Real pipeline wiring: capture → encoder → QUIC streams → decoder → `FrameConsumer`;
  input → QUIC → `HostInput` (`SystemInput` uses `InputInjector`); cursor shape and
  position; text clipboard both ways; session control messages (HelloAck, AuthStatus,
  StreamStarted, KeyframeRequest, Sender/ReceiverReport, Ping/Pong, Goodbye).
- Bounded reconnection with rotating resume tokens; reconnect count/attempts exposed.
- Session diagnostics with explicit unavailable values.
- Examples: `consent_probe`, `dev_loopback_session`.

## TESTED

84 automated tests pass (`cargo test --workspace`), 2 ignored manual window tests.

- Unit: state transitions incl. invalid ones; valid / expired / tampered / unknown-issuer
  / wrong-target / wrong-technician / replayed / wrong-type tickets, session-id pinning,
  unattended and view-less tickets; approval approved / rejected / timeout + withdrawal;
  pt-BR text and sanitization; backoff; decode resync rules; clipboard echo suppression;
  video frame framing; diagnostics serialization; plus all earlier crate tests.
- Integration (`remote-core/tests/session_e2e.rs`, Windows, real capture/encode/decode
  and transport): full lifecycle (friendly-id lookup → ticket → approval → connected →
  decoded frames validated → input applied → clipboard both ways → diagnostics → busy
  refusal → injected interruption → reconnect → keyframe after resume → input after
  resume → teardown with input released and media stopped); rejection; approval timeout;
  ticket from an untrusted authority (never reaches the prompt); failed reconnect
  (bounded attempts, host fails after grace, held keys released). Ran 3× consecutively
  without failure. On this machine DXGI duplication was refused and capture ran on
  Windows.Graphics.Capture.
- Manual: consent window rendered and screenshotted; unanswered prompt times out
  (`consent_probe`); `dev_loopback_session` with the real window → `Rejected
  (ApprovalTimeout)` on both sides; window tests `--ignored` (timeout, dismissal).

## PARTIAL

- Input: the automated tests stop at the `HostInput` boundary (recording double);
  `SystemInput` → `SendInput` is not exercised by automated session tests.
- Clipboard: `SystemClipboard` adapter not covered by automated session tests
  (in-memory clipboards are used because both peers share one machine).
- Approval UI: functional Win32 window; no persistent "session active" indicator or
  end-session button for the host user yet (`HostSessionHandle::end` exists).
- Replay guard is in memory only (lost on host restart; see ADR-0006).
- `Degraded` transitions are implemented but not covered by an automated network
  impairment test.
- Diagnostics: CPU usage, jitter and render latency are not measured (sent as 0 in
  reports, never shown as values); the host cannot report the technician's decoder name.
- Single display only; `SelectDisplay`, `SpecialAction` (Ctrl+Alt+Del), `PasswordAuth`
  ignored; `DisplayList`, `Notice`, `SystemInfo` not sent.
- Secure desktop (UAC/lock screen): the pipeline re-attaches and reopens capture on
  access loss, but this path has not been tested.
- Technician rendering: `FrameConsumer` abstraction only; no GUI renderer.
- Session lifecycle events are not reported to a Control Plane (none exists).

## BLOCKED

- Cross-internet validation (two separate networks, real NATs, deployed relay): needs a
  second machine/network and a reachable relay deployment.
- Production `AuthorizationProvider` and persistent replay protection: need the Control
  Plane (PostgreSQL) service, which is not implemented.

## PLANNED

- Control Plane service (accounts, organizations, devices, session offers over
  signaling, audit) and its `AuthorizationProvider` client.
- Host session indicator with "Encerrar sessão", Technician app UI and renderer.
- Multi-monitor, special keys, file transfer.
- Unattended access (explicitly out of scope of this milestone), Admin interface,
  billing.
