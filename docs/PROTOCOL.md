# Remote session protocol (data plane)

Schema: `crates/protocol/proto/remote.proto` (package `aionix.remote.v1`).
ALPN: `aionix/remote/1`. Compatibility: fields and messages are only ever added; a
breaking change needs a new ALPN.

All traffic runs inside one QUIC connection whose TLS 1.3 handshake authenticates both
Ed25519 endpoint keys (ADR-0002). Relays only forward ciphertext.

## Framing

Every message on the control and input streams is a little-endian `u32` length followed
by the protobuf bytes. Readers reject oversized frames before allocating
(`aionix_protocol::limits`: control 2 MiB, input batch 64 KiB, frame header 256 B, frame
payload 16 MiB, clipboard text 1 MiB, cursor 256×256 RGBA).

## Streams

| Stream | Opened by | Content |
| --- | --- | --- |
| Control (first bidirectional) | technician | `ViewerToHost` / `HostToViewer` |
| Input (unidirectional) | technician, after `GRANTED`, only with `control` permission | `InputBatch` |
| Video (one unidirectional stream per frame) | host | framed `VideoFrameHeader` + `payload_len` bytes of Annex B H.264, then FIN |

## Session establishment (as implemented)

```text
technician                                   host
   │ Hello{ticket, capabilities}  ─────────▶  │ validate ticket (ADR-0006)
   │                                          │   invalid → AuthStatus{DENIED, INVALID_TICKET}, close
   │ ◀───────── HelloAck{PENDING, LOCAL_PROMPT, host_name}
   │                                          │ visible approval prompt (ADR-0007)
   │ ◀───────── AuthStatus{DENIED, USER_REJECTED | TIMEOUT | POLICY}, close
   │ ◀───────── AuthStatus{GRANTED, resume_token}
   │ open input stream                        │ start capture/encode
   │ ◀───────── StreamStarted{epoch, codec, size, source size}
   │ ◀═════════ video streams, CursorShape, CursorPosition, SenderReport, Clipboard
   │ InputBatch, ReceiverReport, KeyframeRequest, Clipboard, Ping ═════════▶
   │ Goodbye{reason} ──────────────────────▶  │ (either side) orderly end
```

A second request while a session is active gets `AuthStatus{DENIED, BUSY}`.

### Resume

After an unexpected loss the technician opens a new connection and sends
`Hello{resume_token}` with an empty ticket. The host accepts it only from the same
technician key, with the current token, inside the grace window, then answers
`HelloAck{GRANTED}` + `AuthStatus{GRANTED, new resume_token}`, resends `StreamStarted`
and restarts video from a keyframe. Anything else gets `AuthStatus{DENIED,
INVALID_TICKET}`. See ADR-0008.

## Video

`StreamStarted.stream_epoch` changes whenever encoded geometry changes; frames carry
their epoch and a sequence number. The technician decodes only from a keyframe after a
new epoch, a sequence gap, a decode error or a full decode queue, and asks for one with
`KeyframeRequest` (rate limited on both sides).

## Reports

- `ReceiverReport` (technician → host, every second): per-interval frames received /
  decoded / dropped, average decode time, decode queue, bytes. `render_us_avg` and
  `jitter_us` are not measured yet and sent as 0.
- `SenderReport` (host → technician, every 500 ms): measured fps, bitrate, capture and
  encode averages, encoder name and hardware flag, encoded size, skipped frames, RTT,
  loss ‰, `capture_backend` (field 15) and `congested` (field 16, drives `Degraded`).
  `cpu_percent` is not measured and sent as 0.

## Close codes

`0` normal, `1` rejected, `2` protocol error, `3` injected transport failure (tests and
diagnostics), `4` superseded by a resumed connection.

## Not implemented yet

The host ignores `PasswordAuth`, `SelectDisplay` and `SpecialAction` (single display,
local-prompt approval only, no Ctrl+Alt+Del yet), applies `QualitySettings` to the
adaptive controller, and does not send `DisplayList`, `Notice` or `SystemInfo` yet.
