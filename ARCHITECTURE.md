# Aionix Connect — Architecture

Consent-based remote support. A technician can see and control a computer only after the
authority authorized the session **and** the person at that computer clicked "Permitir"
in a visible prompt. The product does not implement stealth access, hidden persistence,
concealed control, credential harvesting or security-software bypass.

Current state per component: [PROJECT_STATUS.md](PROJECT_STATUS.md). Decisions:
[docs/adr](docs/adr). Wire protocol: [docs/PROTOCOL.md](docs/PROTOCOL.md).

## Workspace

| Crate | Responsibility |
| --- | --- |
| `brand` | Product identity from `branding/brand.json` (names shown to users). |
| `shared` | Domain types: `FriendlyId` (lookup only), `AccessMode`, `DeviceKind`, `TransportKind`, `QualityMode`. |
| `crypto` | Ed25519 identities, typed expiring signed tokens (`axt1`), domain-separated signatures. |
| `control-api` | Control Plane contract: REST DTOs, signaling messages, token claims (`SessionTicketClaims`). |
| `protocol` | Data-plane protobuf schema and bounded length-delimited framing. |
| `transport` | iroh/QUIC endpoint: authenticated, end-to-end encrypted connections; LAN/direct/relay path selection (ADR-0002). |
| `capture` | Windows screen + cursor capture: DXGI → WGC → GDI (ADR-0004). |
| `codec` | D3D11 BGRA→NV12 conversion, Media Foundation H.264 encoder/decoder (ADR-0003). |
| `input` | Scancode/Unicode input injection with held-key tracking and `release_all`. |
| `platform` | Windows integration: input desktop, DPAPI, system info, clipboard, **consent window**. |
| `remote-core` | Session engine: `session` (this document) and the `adaptive` quality controller. |
| `server-common` | Server plumbing: env config validation, signing-key files, JSON logs, Prometheus, `/healthz` vs `/readyz`, trusted-proxy client IP, rate limiter, shutdown; `aionix-healthcheck`. |
| `control-plane` | Control Plane (`aionix-api`, `aionix-admin`): PostgreSQL store and migrations, auth, devices, orgs/roles/permissions, session tickets, audit, Redis presence + bus. |
| `signaling` | `aionix-signaling`: device WebSocket links, presence, offers, host session events. |
| `relay` | `aionix-relay`: iroh relay server behind Aionix access control. |
| `client` | Control Plane client for the desktop apps: enrollment, device sessions, signaling client, `ControlPlaneAuthority`. |

`infra/` holds the Docker image, Compose stack, Caddy config and deployment scripts
(SERVER.md, DEPLOYMENT.md). `apps/`, `services/`, `packages/`, `tools/` are
placeholders without code.

## Server (control plane and data plane)

```text
 desktop apps (aionix-client)                     Aionix Connect Server (Docker Compose)
┌────────────────────────────┐   HTTPS /v1/*    ┌───────┐   ┌────────────┐   ┌────────────┐
│ ControlPlaneAuthority ─────┼─────────────────▶│       ├──▶│ aionix-api ├──▶│ PostgreSQL │
│ DeviceSession / enroll     │                  │ Caddy │   └─────┬──────┘   └─────▲──────┘
│ SignalingClient ───────────┼─ WSS /v1/signal ▶│ (TLS) ├──▶ aionix-signaling ──────┘
│                            │                  │       │         │  presence/bus
│ Transport (iroh QUIC) ═════╪══ /relay ═══════▶│       ├──▶ aionix-relay     Redis (ephemeral)
└────────────────────────────┘   E2E encrypted  └───────┘
         ▲  direct QUIC (LAN / hole punched), end-to-end encrypted  ▼  other device
```

- **Control plane** (API + signaling + PostgreSQL + Redis): accounts, devices, presence,
  authorization, tickets, audit. JSON only.
- **Data plane**: peer-to-peer QUIC, or the relay forwarding ciphertext. The REST API
  never carries remote desktop data.
- Session flow: technician `ControlPlaneAuthority` → `POST /v1/sessions` (user,
  workstation, organization permission, presence checks; ticket signed) → `SessionOffer`
  over signaling → host `HostOfferHandler` pins the session id in `TicketValidator` and
  answers with its address → technician connects with the ticket → host validates it
  (unchanged, ADR-0006) → visible approval (ADR-0007) → host reports the decision as
  session events. Details: ADR-0009 … ADR-0014, SERVER.md.

## Remote session foundation (`remote-core::session`)

```text
 Technician process                                         Host process
┌─────────────────────────────┐                ┌──────────────────────────────────────┐
│ TechnicianSession           │                │ HostService ── accept loop           │
│  AuthorizationProvider ─────┼─ ticket ──┐    │   └─ host session (one at a time)    │
│  (Control Plane / DEV ONLY) │           │    │       TicketValidator  (ADR-0006)    │
│                             │  QUIC (iroh, E2E encrypted, keys authenticated)        │
│  control reader/writer ◀────┼───────────┴────┼──▶ control reader/writer           │
│  video task → DecodeWorker  │◀── 1 uni stream per frame ──  video task ◀─┐          │
│    (thread) → FrameConsumer │                │   ApprovalPrompt (ADR-0007)│          │
│  input task ────────────────┼── InputBatch ──┼─▶ input task → HostInput   │          │
│  ClipboardAccess ◀──────────┼── Clipboard ───┼─▶ ClipboardAccess          │          │
│  StateCell (state machine,  │                │   VideoSource → capture/encode thread │
│   status, diagnostics)      │                │   AdaptiveController, StateCell       │
└─────────────────────────────┘                └──────────────────────────────────────┘
```

### Lifecycle

`session::state` is the single transition table (ADR-0005):

```text
Idle → Requesting → Authenticating → AwaitingApproval → Establishing → Connected ⇄ Degraded
Connected/Degraded → Reconnecting → Connected | Failed
Requesting/Authenticating/AwaitingApproval → Rejected
any non-terminal → Failed | Ending → Ended
```

Host: connection + `Hello` (Requesting) → ticket validation (Authenticating) →
prompt (AwaitingApproval) → media/input start (Establishing) → first `StreamStarted`
sent (Connected). Technician: ticket from the authority (Requesting) → connect + `Hello`
(Authenticating) → `HelloAck{PENDING}` (AwaitingApproval) → `AuthStatus{GRANTED}`
(Establishing) → `StreamStarted` received (Connected). `Degraded` follows the host's
adaptive controller (congested for 3 ticks / good for 6 ticks), mirrored to the
technician through `SenderReport.congested`.

### Authorization

The technician asks an `AuthorizationProvider` for a `SessionGrant` (ticket, host device
id, host key, host address). The host's `TicketValidator` checks signature, type, expiry,
target device (id + key), technician key = transport peer key, session id, mode
(assisted only), `view` permission, and replay (token id and session id, consumed only
when everything else passed). Friendly ids only select the target inside the authority.
In production the provider is `aionix_client::authority::ControlPlaneAuthority`
(ADR-0012); `DevelopmentAuthority` (feature `dev-authority`) remains for tests and
examples only.

### Approval

`request_approval` wraps any `ApprovalPrompt` with the timeout; refusal, timeout or an
unavailable prompt reject the session (fail-closed). `WindowsApprovalPrompt` shows the
pt-BR consent window from `aionix_platform::windows::consent`. Nothing is captured,
injected or synchronized before approval.

### Media, input, clipboard

- Host: `VideoSource::start` (Windows: `ScreenVideoSource`) runs capture → conversion →
  H.264 on its own thread, paced by the adaptive decision and gated by frames in flight
  (unacknowledged video streams). Each encoded frame goes on its own unidirectional QUIC
  stream. Cursor shape/position go on the control stream.
- Technician: frames are read in order and fed to a `DecodeWorker` thread
  (`MfDecoderFactory`) that drops until a keyframe after an epoch change, a sequence gap,
  a decode error or queue overflow, and requests keyframes. Decoded NV12 frames go to a
  `FrameConsumer` (renderer abstraction; no GUI renderer yet).
- Input: the technician may send only with `control` granted and while live; the host
  applies `InputBatch` events through `HostInput` (Windows: `SystemInput` →
  `InputInjector`) only while live, and releases every held key/button whenever the link
  drops or the session ends (checked under the same lock, so nothing can be pressed after
  the release).
- Clipboard: text only, with `clipboard` granted, while live; only changes made during
  the session are sent and writes from the peer are not echoed back. Windows:
  `SystemClipboard`; `MemoryClipboard` for embedders and tests.

### Reconnection

Technician-driven, bounded, with a rotating single-use resume token bound to the
technician key; the host waits `resume_grace` then fails the session (ADR-0008).

### Diagnostics

`SessionDiagnostics` holds session id, state, transport kind, duration, reconnect count,
capture backend, encoder (+hardware), decoder, resolution, fps, RTT, bitrate, packet
loss, encode/decode latency and dropped frames. Each value is `Metric::Available` or
`Metric::Unavailable(reason)` (`NotConnected`, `NotMeasuredYet`, `NotReportedByPeer`,
`NotApplicable`); nothing is estimated.

## Threading

Async session logic runs on Tokio. COM-bound media objects live on dedicated threads
(`aionix-capture`, `aionix-decode`, `aionix-consent`, MFT event pump) and talk to the
async side through bounded channels. Framed stream reads run in their own tasks because
they are not cancellation safe.

## Testing

- Unit tests per crate (state table, ticket checks, approval timeout/withdrawal, pt-BR
  text, backoff, decode resynchronisation, clipboard echo suppression, diagnostics).
- `crates/remote-core/tests/session_e2e.rs`: real transport through an in-process relay,
  real tickets, real Windows capture/encode/decode and both coordinators, with doubles
  only for the approval click, `SendInput` and the system clipboard.
- Manual: `consent_probe` and `dev_loopback_session` examples show the real prompt;
  `cargo test -p aionix-platform consent -- --ignored` opens it briefly.
- Server: unit tests per crate; integration tests against real PostgreSQL/Redis
  (`compose.yaml`, one fresh database per test, migrations from empty) for auth,
  enrollment, sessions, presence, signaling (real WebSocket clients) and the client
  end-to-end flow; relay tests with real iroh endpoints. See SERVER.md.
