# ADR-0004: DXGI Desktop Duplication first, with WGC and GDI fallbacks

- Status: Accepted (recorded retroactively, 2026-10-07; implemented in `crates/capture`)

## Context

Screen capture must be fast, keep frames on the GPU for the encoder, and keep working
when the preferred API is unavailable.

## Decision

`ScreenCapturer::open` tries, in order:

1. DXGI Desktop Duplication: fastest, dirty rectangles from the OS, cursor delivered
   separately, secure desktop when running as SYSTEM;
2. Windows.Graphics.Capture when duplication is refused;
3. GDI as universal fallback.

Every backend produces a BGRA `ID3D11Texture2D` on the same D3D11 device, so the
conversion/encoder pipeline is backend-agnostic. The cursor is sent separately so the
viewer can draw it locally at full rate.

## Consequences

- `CaptureError::AccessLost` (desktop switch, display change, GPU reset) means "reattach
  and recreate"; the session pipeline does that and starts a new stream epoch.
- On the development machine Desktop Duplication was refused during the session tests
  and the pipeline ran on Windows.Graphics.Capture, which exercises the fallback.
