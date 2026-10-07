# ADR-0003: H.264 through Media Foundation as the initial codec

- Status: Accepted (recorded retroactively, 2026-10-07; implemented in `crates/codec`)

## Context

Remote support needs low-latency video that encodes cheaply on the host and decodes
everywhere the Technician app runs.

## Decision

Use H.264 as the first codec, through the operating system's Media Foundation
transforms on Windows:

- hardware accelerated on NVIDIA, Intel and AMD; software MFT as fallback;
- decodable by every WebCodecs / Media Foundation decoder;
- the encoder is the OS's licensed component, so the product does not redistribute an
  encoder implementation;
- low-latency configuration: no B-frames, CBR, effectively infinite GOP with keyframes
  only on demand; Annex B with SPS/PPS repeated on keyframes so a decoder can join at
  any keyframe.

## Consequences

- The protocol carries a codec field (`VideoCodec`) so other codecs can be added.
- Keyframes are explicit events: session start, resume, viewer request, decoder
  resynchronisation after a gap.
