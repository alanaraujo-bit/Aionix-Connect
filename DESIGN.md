# Design

Recorded from the built site (`site/public/index.html`) and the desktop apps
(`crates/desktop/src/theme.rs`, `crates/setup`). Describes what exists.

## World

The product's own interface is the visual language: dark, quiet surfaces where the app UI
(status pills, chips, 10 px buttons, the 9-digit ID) is the hero material. Two product
colors split the world in half — **blue is the client** (Aionix Connect), **violet is the
technician** (Aionix Connect Técnico) — and appear only where that side of the session is
meant. Green belongs to consent ("Permitir", "Conectado").

## Tokens

| Role | Value |
| --- | --- |
| Background | `#0d1015` (deep `#0a0c10`) |
| Surface / raised | `#151921` / `#1c212b` |
| Border / soft border | `#2a303c` / `#20252f` |
| Text / muted / faint | `#e8ebf1` / `#8d96a7` / `#5d6574` |
| Accent (actions) | `#4f7cff`, hover `#6b91ff` |
| Client gradient | `#6a97ff → #2d55e6` (135°) |
| Technician gradient | `#a37bff → #5538db` (135°) |
| Success / warning / danger | `#34c77b` / `#f2ab3c` / `#ef555a` |

Radii: buttons 10 px, panels 12–14 px, download panels 20 px, icons ~23% of size.
Shadows: neutral elevation only (black, offset, soft blur); no colored glows.

## Type

- Display (site): Bricolage Grotesque 600–700, tracking −0.025em, balanced.
- UI/body: Segoe UI Variable (the apps' face), 16 px / 1.6 on the site.
- Numbers that are measurements, IDs or hashes: JetBrains Mono, tabular — never decorative.

## Components

- **Product replica**: technician window (title bar, session bar with pill + mono stats +
  chips, 16:9 screen) and the client consent card; the hero animates ID → Permitir →
  Conectado; reduced motion shows the connected state.
- **Flow**: three moments, each with a real UI fragment (ID, Conectar por ID, Recusar/Permitir).
- **Measures ledger**: label + mono value rows with a one-line context, hairline separated.
- **Mechanism ledger**: two-column grid of icon + heading + paragraph with hairlines, not cards.
- **Download panel**: product icon, audience line, name, gradient button in the product
  color, meta row (version, size, date, SHA-256) filled from the signed manifest.
- **Installer** (egui): 300 px gradient hero panel in the product color with the icon,
  dark content side with toggle chips, gradient progress bar with sheen, step checklist.

## Icons

`branding/icons/*.ico|png` from `infra/branding/make-icons.py`: rounded square, diagonal
product gradient, soft top-left light, white "A" monogram. Inline SVG line icons on the
site use 1.7–1.8 stroke, round caps, colored by the side they describe.

## Motion

One authored moment: the hero session sequence. Sections reveal once on scroll (fade +
16 px). Everything is visible without JavaScript and static under reduced motion.
