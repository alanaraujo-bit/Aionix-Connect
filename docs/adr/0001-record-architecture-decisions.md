# ADR-0001: Record architecture decisions

- Status: Accepted
- Date: 2026-10-07

## Context

Aionix Connect makes security-relevant choices (identity, authorization, consent,
transport, media pipeline) that later contributors must be able to understand and
challenge without reverse-engineering the code.

## Decision

Decisions that materially affect the architecture are recorded as short ADRs in
`docs/adr/NNNN-title.md` (context, decision, consequences). An ADR is never rewritten
after acceptance; a change of mind is a new ADR that supersedes the old one.

ADR-0002 to ADR-0004 were written retroactively on 2026-10-07 for decisions that the
code already referenced; they restate the rationale documented in the code at that time.

## Consequences

Code comments may cite ADR numbers. `ARCHITECTURE.md` gives the overview,
`PROJECT_STATUS.md` the current state; ADRs explain *why*.
