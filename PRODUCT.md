# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Stack

Two surfaces:

- **The app** (the product people use every day): native Windows desktop, Rust +
  egui 0.36 (wgpu) in `crates/desktop` (screens in `views.rs`, look in `theme.rs`,
  icons in `icons.rs`); the consent prompt is plain Win32 + GDI/GDI+ in
  `crates/platform/src/windows/consent.rs`; the installer is egui in `crates/setup`.
  Screens render to PNG with sample data for review:
  `cargo test -p aionix-desktop --test ui_preview -- --ignored`.
- **The site** (`Platform: web` above refers to it): static HTML/CSS/JS (one page, no
  framework) on Vercel (`site/`, project `aionix-connect`, domain
  `aionixconnect.aionixdev.com`). Chosen because the site is a landing page plus
  downloads and the update channel files; nothing needs a server runtime. Published only
  by `infra/release.ps1` (git deploys are disabled).

## Users

IT / support technicians — initially the owner and two friends — who give remote support
to their clients, and the people they help. Everyone installs the same app,
**Aionix Connect** (like AnyDesk): it always shows this computer's 9-digit ID for
receiving help, and connects to other computers by ID after signing in once.
Future: a commercial product for support professionals.

## Product Purpose

Remote access and support for Windows: the technician sees and controls the client's
screen after the client explicitly allows it. Success: a technician sends one link, the
client installs in seconds, and the session is fluid and trustworthy; the apps keep
themselves up to date without anyone sending files again.

## Positioning

Own infrastructure (self-hosted server at connect.aionixdev.com), end-to-end encrypted
sessions, explicit on-screen consent for every session, direct peer-to-peer connection
with an encrypted relay fallback, and updates signed with the project's own release key.

## Operating Context

Both sides download the same installer from the site. The client opens Aionix Connect
and reads out the ID; the technician types it in "Acessar outro computador" (signed in);
the client clicks "Permitir". Receiving help needs no account; connecting does
(accounts are created by the server admin). Windows only.

## Capabilities and Constraints

- Windows 10/11 64-bit; per-user install, no administrator prompt.
- H.264 hardware encoding (e.g. Intel Quick Sync) with software decode; adaptive bitrate.
- Measured in real tests: 2 ms / 30 fps on LAN; ~90 ms, 30–34 fps, 0% loss over 4G, direct path.
- Apps check a signed update channel and install updates themselves.
- Installers are not Authenticode-signed yet: Windows SmartScreen warns on first install.
- Accounts are created by the server admin (no public sign-up).

## Brand Commitments

Name "Aionix Connect", company "Aionix" (branding/brand.json). pt-BR voice, direct and
calm.

- **Standing preference (chosen 2026-10-10): the category standard, executed at full
  craft.** The app looks like the canonical remote-support app (sidebar + content, the
  ID as the hero, a computer list, an edge-to-edge session), with the finish of Apple
  Screen Sharing (macOS) and the current TeamViewer as the quality bar. No neon, no
  glows, no gradient controls, nothing that reads as AI-generated.
- **Icon:** two screens that overlap, the region they share filled solid (what both
  people see in a session), white on a blue rounded square
  (infra/branding/make-icons.py → branding/icons). The old "A" monogram and the
  blue→violet gradient are retired.
- **Color:** Windows-native neutrals following the system light/dark setting, one blue
  accent (#0b63e5 light / #2b74f0 dark) for primary actions, focus and selection; green
  only for online/connected (crates/desktop/src/theme.rs).
- **Motion:** quiet, exponential ease-out transitions; everything jumps to its end state
  when Windows animation effects are off.

## Evidence on Hand

Real screenshots of sessions exist only in chat history (not in the repo). No customers,
testimonials, pricing or benchmarks beyond the measurements above — none may be invented.

## Product Principles

1. Consent is visible: nothing happens on a client's computer without them allowing it.
2. Trust is earned with mechanisms (encryption, signatures), not adjectives.
3. One app, zero confusion: the same download receives and gives support.
4. Updating should never require sending a file again.
