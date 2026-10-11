---
name: Aionix Connect
description: Remote support for Windows. The category's standard app, finished to the bar set by Apple Screen Sharing and the current TeamViewer.
colors:
  bg-light: "#ffffff"
  bg-dark: "#1e1e21"
  sidebar-light: "#f4f4f6"
  sidebar-dark: "#17171a"
  surface-light: "#ffffff"
  surface-dark: "#28282c"
  surface-alt-light: "#f0f1f3"
  surface-alt-dark: "#2d2d32"
  border-light: "#d8dadf"
  border-dark: "#3d3d44"
  field-border-light: "#8c8f96"
  field-border-dark: "#72747b"
  hairline-light: "#e6e7ea"
  hairline-dark: "#303035"
  text-light: "#18191c"
  text-dark: "#ededef"
  muted-light: "#55585f"
  muted-dark: "#a6a8af"
  faint-light: "#686b72"
  faint-dark: "#95979e"
  accent-light: "#0b63e5"
  accent-dark: "#226ce8"
  accent-hover-light: "#0a58cc"
  accent-hover-dark: "#2670ec"
  accent-press-light: "#094db3"
  accent-press-dark: "#1f62d4"
  accent-soft-light: "#e8f0fd"
  accent-soft-dark: "#222e45"
  accent-text-light: "#0b5cd6"
  accent-text-dark: "#6ca6ff"
  on-accent: "#ffffff"
  success-light: "#1a8f4c"
  success-dark: "#3cc373"
  warning-light: "#9a6008"
  warning-dark: "#e6a23c"
  danger-light: "#cf3238"
  danger-dark: "#f25c58"
  danger-fill: "#d1343a"
  brand-tile: "#1b6aeb"
typography:
  id:
    fontFamily: "Segoe UI Semibold, Segoe UI, sans-serif"
    fontSize: "44px"
    fontWeight: 600
  title:
    fontFamily: "Segoe UI Semibold, Segoe UI, sans-serif"
    fontSize: "20px"
    fontWeight: 600
  id-input:
    fontFamily: "Segoe UI Semibold, Segoe UI, sans-serif"
    fontSize: "17px"
    fontWeight: 600
  overlay-headline:
    fontFamily: "Segoe UI Semibold, Segoe UI, sans-serif"
    fontSize: "17px"
    fontWeight: 600
  heading:
    fontFamily: "Segoe UI Semibold, Segoe UI, sans-serif"
    fontSize: "15px"
    fontWeight: 600
  body:
    fontFamily: "Segoe UI, sans-serif"
    fontSize: "14px"
    fontWeight: 400
  button:
    fontFamily: "Segoe UI Semibold, Segoe UI, sans-serif"
    fontSize: "13.5px"
    fontWeight: 600
  secondary:
    fontFamily: "Segoe UI, sans-serif"
    fontSize: "13.5px"
    fontWeight: 400
  small:
    fontFamily: "Segoe UI, sans-serif"
    fontSize: "12.5px"
    fontWeight: 400
  caption:
    fontFamily: "Segoe UI, sans-serif"
    fontSize: "11.5px"
    fontWeight: 400
rounded:
  sm: "6px"
  md: "8px"
  banner: "10px"
  sheet: "12px"
  mark-tile: "22.5%"
spacing:
  xs: "4px"
  sm: "8px"
  md: "12px"
  lg: "16px"
  xl: "24px"
  sheet: "28px"
  page-top: "32px"
  page-x: "40px"
components:
  button-primary:
    backgroundColor: "{colors.accent-light}"
    textColor: "{colors.on-accent}"
    typography: "{typography.button}"
    rounded: "{rounded.md}"
    padding: "0 16px"
    height: "36px"
  button-primary-hover:
    backgroundColor: "{colors.accent-hover-light}"
  button-primary-press:
    backgroundColor: "{colors.accent-press-light}"
  button-primary-disabled:
    backgroundColor: "{colors.accent-soft-light}"
    textColor: "{colors.accent-text-light}"
  button-secondary:
    backgroundColor: "{colors.surface-alt-light}"
    textColor: "{colors.text-light}"
    typography: "{typography.button}"
    rounded: "{rounded.md}"
    padding: "0 16px"
    height: "36px"
  button-ghost:
    backgroundColor: "transparent"
    textColor: "{colors.text-light}"
    typography: "{typography.button}"
    rounded: "{rounded.md}"
    height: "36px"
  button-ghost-hover:
    backgroundColor: "{colors.surface-alt-light}"
  button-quiet:
    backgroundColor: "transparent"
    textColor: "{colors.accent-text-light}"
    typography: "{typography.button}"
    rounded: "{rounded.md}"
    height: "36px"
  button-quiet-hover:
    backgroundColor: "{colors.accent-soft-light}"
  button-danger:
    backgroundColor: "{colors.danger-fill}"
    textColor: "{colors.on-accent}"
    typography: "{typography.button}"
    rounded: "{rounded.md}"
    padding: "0 16px"
    height: "36px"
  icon-button:
    backgroundColor: "transparent"
    textColor: "{colors.muted-light}"
    rounded: "{rounded.md}"
    size: "32px"
  icon-button-active:
    backgroundColor: "{colors.accent-soft-light}"
    textColor: "{colors.accent-text-light}"
  text-field:
    backgroundColor: "{colors.bg-light}"
    textColor: "{colors.text-light}"
    typography: "{typography.body}"
    rounded: "{rounded.md}"
    padding: "8px 12px"
    height: "36px"
  id-field:
    backgroundColor: "{colors.bg-light}"
    textColor: "{colors.text-light}"
    typography: "{typography.id-input}"
    rounded: "{rounded.md}"
    padding: "7px 12px"
    width: "220px"
    height: "36px"
  nav-item:
    backgroundColor: "{colors.sidebar-light}"
    textColor: "{colors.text-light}"
    typography: "{typography.body}"
    rounded: "{rounded.md}"
    height: "34px"
  nav-item-selected:
    backgroundColor: "{colors.accent-soft-light}"
    textColor: "{colors.text-light}"
  sidebar:
    backgroundColor: "{colors.sidebar-light}"
    width: "232px"
    padding: "14px 12px"
  sheet:
    backgroundColor: "{colors.surface-light}"
    rounded: "{rounded.sheet}"
    padding: "{spacing.sheet}"
  banner:
    rounded: "{rounded.banner}"
    padding: "14px 16px"
  device-row:
    textColor: "{colors.text-light}"
    height: "46px"
  device-row-table:
    height: "52px"
  session-toolbar:
    backgroundColor: "{colors.sidebar-light}"
    height: "52px"
    padding: "0 12px"
  brand-mark:
    backgroundColor: "{colors.brand-tile}"
    textColor: "{colors.on-accent}"
    rounded: "{rounded.mark-tile}"
---

# Design System: Aionix Connect

Recorded from the built desktop app: `crates/desktop/src/theme.rs` (palettes, type, motion, controls), `views.rs` (screens), `icons.rs` (icons), the consent prompt `crates/platform/src/windows/consent.rs`, and the mark from `infra/branding/make-icons.py` → `branding/icons/`. Where the build and the plan disagree, this file follows the build. Token keys ending in `-light` and `-dark` are the two theme variants of one role. Components reference the light variant, and the dark theme swaps in the matching `-dark` token.

## Overview

**Creative North Star: "The Number Read Aloud"**

Aionix Connect looks like the standard remote-support app, built carefully. The window has a sidebar and a content area. The 9-digit ID is the largest thing on Início. Computers are listed as rows, and a session fills the window edge to edge. The person being helped reads out one number, and everything else stays quiet so that number reads clearly. The surfaces are Windows-native neutrals that follow the system light/dark setting. One blue marks where you act and where you are. Green appears only when something is actually connected.

The finish target is Apple Screen Sharing and the current TeamViewer. The surfaces are flat, the separators are 1 px hairlines, controls have 8 px corners and sheets have 12 px corners. All type is Segoe UI, in two weights. Icons are line icons drawn in code, and the mark is two overlapping screens. The interface has no gradients on controls, no glows, no neon, no monogram and no card grids. Status sits in dots and plain lines of text, not in badges.

Motion is calm and serves a purpose. Everything uses an exponential ease-out with no overshoot. Only states that are still in progress pulse. All animation stops when Windows animation effects are turned off.

**Key Characteristics:**
- Neutral light and dark palettes that follow the Windows setting. Neither theme is the "real" one.
- One blue accent for primary actions, focus, selection, links and progress.
- Green for online, connected and in session. Amber for in progress and urgent. Red for offline, errors and ending things.
- The ID at 44 px Segoe UI Semibold, with its digits settling into place the first time it appears.
- Flat surfaces separated by hairlines. Shadows appear only on sheets and popups.
- Line icons on a 16-unit grid, drawn in code.

## Colors

The palette is neutral and slightly cool, with one blue accent and three status hues, each with a light and a dark variant.

### Primary
- **Action Blue** (`accent-light` / `accent-dark`): the filled primary button (Conectar, Entrar, Reiniciar e atualizar, Conectar novamente), the focus ring (at 80 % opacity), the text cursor, the selection outline, the spinner arc and the update download bar. Hover and press darken it in the light theme (`accent-hover`, `accent-press`). In the dark theme hover lightens it and press darkens it.
- **Selection Wash** (`accent-soft-*`): the selected nav item, an active toolbar toggle, the hover of Quiet row actions, the avatar circle, the pending-request banner, and the fill of a disabled primary button.
- **Link Blue** (`accent-text-*`): blue used as text. Links, the label of a Quiet button, avatar initials, the "este computador" tag, the selected nav icon and an active toolbar icon. It is 5.97:1 on white and 6.76:1 on the dark background.

### Secondary
- **Brand Tile Blue** (`brand-tile`, #1b6aeb): used only as the mark's tile. It is a separate color from the accent, and the two are never swapped.

### Tertiary (status)
- **Connected Green** (`success-*`): the online dot, the "Pronto para receber suporte" and "Em sessão" dots, the dot of a connected session, and the inbound banner ("… está acessando este computador"), whose fill is green at 10 % with a 35 % outline.
- **In-Progress Amber** (`warning-*`): dots that pulse while starting, connecting or reconnecting; the consent countdown in its last 10 s; and non-blocking problems ("Não foi possível verificar atualizações.", the development environment badge, the self-test notice).
- **Stop Red** (`danger-*`): the dot when there is no connection to the server, error text, and the "Não foi possível atualizar a lista" line. `danger-fill` (#d1343a, white text at 4.93:1) fills destructive buttons: Encerrar, Encerrar acesso.

### Neutral
- **Page** (`bg-*`): the content area, and the fill inside text fields.
- **Rail** (`sidebar-*`): the sidebar and the session toolbar. It sits one step below the page in both themes.
- **Raised** (`surface-*`): sheets, menus and overlays.
- **Control Fill** (`surface-alt-*`): Secondary buttons, the hover fill of Ghost buttons, rows and icon buttons, and skeletons.
- **Field Edge** (`field-border-*`): the 1 px outline of a text field at rest. It is 3.24:1 on white and 3.56:1 on the dark background.
- **Control Edge** (`border-*`): the outline of a widget in its open state, and the track of a switch that is off.
- **Hairline** (`hairline-*`): every separator, the sidebar's right edge, the toolbar's bottom edge, row dividers, and the outlines of sheets and Secondary buttons.
- **Ink** (`text-*`), **Muted** (`muted-*`, secondary text, 7:1 or better) and **Faint** (`faint-*`, tertiary text and placeholders). Faint measures 5.34 on `bg`, 4.86 on `sidebar` and 4.72 on `surface-alt` in light, and 5.70, 6.13 and 4.70 in dark.

### Named Rules
**The One Blue Rule.** Blue means "act here", "you are here", or "on its way". It marks one filled primary button per region, plus focus, selection, links and progress. Blue never decorates, and a second filled blue button in the same region is a bug.

**The Earned Green Rule.** Green appears only when something is actually online or connected. It never means "success" in general, never appears on buttons, and never marks a list that could not be refreshed.

**The Honest List Rule.** When a signed-in list cannot be refreshed, every dot turns faint, names drop to Muted, and Conectar is disabled with the reason on hover ("Sem conexão com o servidor. A lista volta quando a conexão voltar."). On Início the heading becomes "Última lista conhecida". On Computadores a Stop Red line ("Não foi possível atualizar a lista: …") sits above the table. Data the app cannot confirm is never shown as current.

## Typography

**Display Font:** Segoe UI Semibold (`seguisb.ttf`, falling back to `segoeuib.ttf`)
**Body Font:** Segoe UI (`segoeui.ttf`), loaded from the Windows fonts folder
**Label/Mono Font:** none. Only diagnostics use egui's default monospace, at 13 px.

**Character:** this is the system's own voice, in two weights. Semibold carries hierarchy, Regular carries reading, and color (Ink, Muted, Faint) does the rest.

### Hierarchy
- **ID** (600, 44 px): this computer's ID on Início, grouped "000 000 000" with a gap of 0.42 digit widths between groups. It is never set in monospace.
- **Title** (600, 20 px): page titles (Computadores) and sheet titles (Entrar).
- **ID input** (600, 17 px): the ID typed into "Acessar outro computador". It auto-groups as you type.
- **Overlay headline** (600, 17 px): the headline of the session status sheet.
- **Heading** (600, 15 px): section headings ("Acessar outro computador") and empty-state titles.
- **Body** (400, 14 px): status lines, nav items, field text. Names in device rows use 600 at 14 px.
- **Button** (600, 13.5 px): every button label.
- **Secondary** (400, 13–13.5 px, Muted): explanations under headings, banner detail, error lines (in Stop Red).
- **Small** (400, 12.5 px): links, session metrics, last-seen time, field labels in the sign-in sheet.
- **Column/aside** (400, 12 px, Faint): table headers (Nome, ID, Sistema), the customer name under a device, the "este computador" tag.
- **Caption** (400, 11.5 px, Faint or Muted): version line, organization under the account name, "ID …" under the session title.

### Named Rules
**The Two Weights Rule.** Only Regular and Semibold exist. Hierarchy comes from size and from the Ink/Muted/Faint ladder, never from bold, italics or uppercase.

## Layout

The window has two regions. On the left is a **232 px sidebar** (Rail color, a 1 px hairline on its right edge, 12 px horizontal and 14 px vertical inset). From the top it holds the mark (26 px) and the name at Semibold 14.5, then Início and Computadores, then a block pinned to the bottom with the update card or line, a hairline, the account row (32 px avatar, name, organization, sign-out icon button) or the Entrar button, and the version line. On the right is the **content area**, inset 40 px from the sides and 32 px from the top.

**Início** reads top to bottom in one column, at most 760 px wide. Any request or session banner comes first, followed by 24 px of space. Then come "Seu ID", the ID with Copiar 20 px to its right, the status line (dot, text, optional Muted detail indented 18 px), the consent promise in Muted, 30 px of space, a hairline, and 26 px of space. Then "Acessar outro computador": the heading and detail, the 220 px ID field with a 112 px Conectar 10 px away, and, when signed in, "Disponíveis agora" with "Ver todos (n)" right-aligned above a hairline and the device rows. A "Detalhes técnicos" disclosure ends the page.

**Computadores** has the title and count line ("6 computadores · 3 online") on the left and a 236 × 34 search field on the right. A table follows. Columns are Nome 40 %, ID 20 % and Sistema 22 %, with text starting 26 px after the presence dot, and Conectar or the last-seen time sits on one right edge.

**The session** has a 52 px toolbar (Rail color, hairline at the bottom). On the left are the title over "ID …" in Caption, the state dot with its text, and the metrics in Faint. On the right are the icon toggles and Encerrar, or Conectar novamente once the session has ended. The remote picture fills everything below the toolbar edge to edge and never sits under chrome.

Spacing is 8 px between items, with steps of 4, 8, 12, 16, 24 and 28 px. Rows are 46 px on Início and 52 px in the table. Controls are 34–36 px tall.

## Elevation & Depth

The app is flat by default. Regions are told apart by tone (Rail against Page) and by hairlines. Shadows are soft, neutral black, and appear only on things that float above the window.

### Shadow Vocabulary
- **Popup** (level 1, `0 4px 14px` black, alpha 34/255 light, 110/255 dark): menus and tooltips.
- **Sheet** (level 2, `0 12px 36px` black, alpha 51/255 light, 165/255 dark): the sign-in sheet, the session status overlay, and windows.

The sign-in sheet also dims the window behind it (black at 80/255 in light, 140/255 in dark).

### Named Rules
**The Float-Only Shadow Rule.** A shadow means "this sits above the window". Rows, banners, buttons and the sidebar never cast one. Tinted banners use a 10–35 % tint of their status color and a 1 px outline, not elevation.

## Shapes

Corners are gentle and consistent: **8 px** on every control (buttons, fields, nav items, icon buttons, row hover fills), **10 px** on banners and the sidebar update card, **12 px** on sheets, and **6 px** on skeletons and the environment badge. Status dots are 8 px circles in a 10 px cell. Avatars are full circles with initials in a Selection Wash fill. The mark's tile has a 22.5 % corner radius. Progress bars are 4 px tall in the app and 3 px in the consent prompt, with fully rounded ends.

## Components

### Buttons
Buttons are calm and sit firmly in place.
- **Shape:** 8 px corners, 36 px tall, 16 px side padding (9 px when icon-only), Button type. An optional 16 px leading icon sits 8 px from the label.
- **Primary:** Action Blue fill with white text. Use one per region.
- **Secondary:** Control Fill with a 1 px hairline outline and Ink text (Copiar/Copiado, Entrar in the sidebar).
- **Ghost:** no fill at rest and Control Fill on hover. Used for Cancelar and the session's "‹ Início".
- **Quiet:** Link Blue text with Selection Wash on hover. Used for row actions, such as Conectar in device rows.
- **Danger:** `danger-fill` with white text. Used for ending things: Encerrar, Encerrar acesso.
- **Hover / Press:** colors cross-fade (0.12 s hover, 0.08 s press). A pressed button moves 1 px lower. Secondary shifts 6 % toward Ink on hover and 11 % on press. Ghost goes from transparent to Control Fill on hover, and to Control Fill shifted 11 % toward Ink on press. Danger fades to 90 % opacity on hover and 80 % on press.
- **Focus:** a 2 px Action Blue ring at 80 % opacity, 2 px outside the control, with its corner radius increased by 2.
- **Disabled:** a Primary takes the Selection Wash fill with Link Blue text at 60 %, never the field fill, so it still reads as "the blue action, not yet". Other kinds take Control Fill with Faint text. Ghost and Quiet stay unfilled. A disabled button explains itself on hover when the reason is not visible ("Disponível quando não houver sessão em andamento.").

### Icon buttons
- A 32 px square with 8 px corners and a 16 px icon in Muted. Hover fills it with Control Fill. A toggle that is on (control, diagnostics) keeps a Selection Wash fill with a Link Blue icon and cross-fades in over 0.16 s. Every icon button has a tooltip.

### Inputs / Fields
- **Style:** Page-colored fill, a 1 px Field Edge outline at rest, 8 px corners, 36 px tall, 12 px side padding. Placeholders are Faint ("000 000 000", "voce@empresa.com.br", "Buscar").
- **Hover:** the outline turns Muted.
- **Focus:** the outline becomes Action Blue, with a 1.5 px blue cursor.
- **ID field:** 220 px wide, ID-input type. It accepts only digits, at most nine, groups them as you type, and keeps the cursor at the end. Conectar is enabled only for nine digits that do not start with 0. Enter also connects.
- **Search field:** 236 × 34, with a 14 px Search icon in Faint at the left (34 px left padding).
- **Error:** a Secondary-size line in Stop Red directly under the field or form.

### Navigation
- Sidebar items are 34 px tall and full width with 8 px corners, a 16 px icon at 20 px from the left, and the label at 38 px. At rest the icon is Muted and the label is Ink in Regular. Hover tints the item 5 % toward Ink (0.12 s). Selected items take Selection Wash with a Link Blue icon and a Semibold label (0.18 s). Trailing content is a Faint count of online computers, or a 13 px Lock icon when signed out.

### Status line and dots
- A dot followed by Body text, with no container. Dot color follows the status rules. Only in-progress states pulse: a ring grows from 4 to 9 px and fades from 35 % to 0, about once every 1.1 s. Settled states never move.

### Device rows
- A presence dot at 12 px. The name in Semibold 14 (Ink when online, Muted otherwise), then the ID in Faint (Muted in the table) and the optional "este computador" tag in Link Blue. A hairline runs under every row. Hover fills the row with Control Fill (8 px corners, inset 2 px vertically). Online rows end in a Quiet Conectar, and a double-click also connects. Other rows end in Faint "conectando…" or the last-seen time. When the list is stale, the Honest List Rule applies.

### Banners
- Shown at the top of Início, 10 px corners, 16 × 14 px padding, fading in over 0.35 s.
- **Pending request:** Selection Wash fill with a 35 % accent outline and a 20 px Lock icon in Link Blue. The text reads "{name} pediu para acessar este computador", with a Muted line pointing to the confirmation window.
- **In session:** Connected Green at 10 % fill with a 35 % outline and a status dot (amber and pulsing while the session reconnects). The text reads "{name} está acessando este computador", with permissions, duration and state in Muted, and **Encerrar acesso** (Danger) on the right.

### Sheets and overlays
- **Sign-in sheet:** a 340 px content column on a Raised surface with 28 px padding, 12 px corners and a hairline outline at Sheet elevation, over a dimmed window. Title "Entrar", a Muted line, labeled fields (Small, Muted labels), and Entrar (Primary, 104 px) to the right of Cancelar (Ghost). While working, the Primary label changes and a 16 px spinner appears.
- **Session overlay:** a 400 × 176 sheet centered on the session area, with a 26 px spinner, or a status disc (13 px radius at 15 % tint around a 4.5 px solid dot), then a 17 px headline and a Muted detail.

### Loading
- **Skeleton:** Control Fill bars with 6 px corners that shimmer toward Hairline about every 1.25 s. They stand in for the ID (300 × 36) and for table rows. Under reduced motion they hold at mid-tone.
- **Spinner:** a 2 px Hairline circle with a 1.6 rad Action Blue arc that turns at 5.5 rad/s. Under reduced motion it holds still.
- **Update bar:** 4 px, Hairline track, Action Blue fill with 2 px corners, under "Baixando a versão …".

### Disclosure
- A Small, Muted label with a 12 px chevron (right when closed, down when open). On hover it moves toward Ink. When opened, its content fades in over 0.22 s. Inside, diagnostic key–value rows have a 150 px Muted key column and Small values.

### Brand mark
- Two landscape screens overlap on a 24-unit grid: the back screen at (3, 3.6)–(15.6, 11.8) and the front at (8.4, 8.8)–(21, 17), both with 1.6-unit corners. The region they share is filled solid, and the front screen stands on a neck and base. The mark is white on a flat Brand Tile Blue square with a 22.5 % corner radius. The stroke is 1.7 units, or 2.1 units at 32 px and below so the two outlines never merge. The same geometry is drawn at runtime (`theme::paint_mark`, the consent prompt) and rasterized to `branding/icons/setup.ico` (16–256 px), `setup-512.png` and `app-256.png`. Sizes used: 26 px in the sidebar, 32 px in the brand header, 40 px in the consent prompt.

### Icons
- 19 line icons are drawn as vectors on a 16-unit grid: Home, Computers, Copy, Check, Search, User, SignOut, Pointer, Pulse, Fullscreen, ExitFullscreen, Chevron Left/Right/Down, Refresh, Download, Lock, Close and Phone. All use one stroke of 1.5 units (at least 1.25 px) with round caps, and none are filled. They are drawn at 12–16 px in controls, 20 px in banners and 28 px in empty states. Their ink follows the text ladder (Muted at rest, Link Blue when active or selected, Faint for tertiary hints). No emoji and no glyphs from icon fonts.

### Consent prompt (Win32)
The same visual language, painted with GDI+ and ClearType in a native top-most window that follows the Windows light/dark setting. The caption, caption text and border take the prompt's colors, and the window has Windows 11 rounded corners.
- **Layout:** 460 px wide with 28 px padding. The 40 px mark sits beside the heading (Segoe UI 600, 18 px). Next comes an identity block: a panel-colored rounded box holding a 36 px Selection Wash circle with Link Blue initials, the requester's name (600, 15 px) and a Small Muted line with e-mail and organization. Then the body (14 px, Muted) and the caution line, emphasized (600, 14 px, Ink, "Permita somente se você reconhece esta solicitação."). Then the countdown text (Small, Faint) above a 3 px bar that empties toward the left. Buttons are 36 px tall and at least 108 px wide, with Recusar to the left of Permitir.
- **Countdown:** Action Blue, turning to In-Progress Amber (bar and text) in the last 10 s. The bar repaints every 50 ms.
- **Safety in the visual layer:** Recusar has the initial focus and is the default button. Enter, Esc, Alt+F4 and the close box all refuse. Its focus ring is a neutral 1.5 px Muted ring inset with a gap, so the focused Recusar never looks like a second blue button. Permitir stays disabled for the allow delay, then becomes the Action Blue primary. Pressed labels move 1 px lower. The window flashes in the taskbar until it is in the foreground.
- **Palette (prompt-specific constants):**

| Role | Light | Dark |
| --- | --- | --- |
| Background (caption too) | #ffffff | #28282c |
| Identity panel | #f4f4f6 | #313136 |
| Text / muted | #18191c / #55585f | #ededef / #a6a8af |
| Faint (countdown) | #686b72 | #95979e |
| Hairline (border, bar track) | #e6e7ea | #3a3a40 |
| Accent / hover / press | #0b63e5 / #0a58cc / #094db3 | #226ce8 / #2670ec / #1f62d4 |
| Accent soft / accent text | #e8f0fd / #0b5cd6 | #222e45 / #6ca6ff |
| Secondary fill / hover / press / line | #f0f1f3 / #e6e7ea / #dcdde1 / #e2e3e7 | #38383e / #414148 / #4a4a52 / #44444b |
| Warning | #9a6008 | #e6a23c |
| Disabled fill / text (Permitir) | #e8f0fd / #7fa5e6 | #222e45 / #5b7fb6 |

A disabled Permitir follows the app rule: Selection Wash fill with dimmed Link Blue text. The prompt keeps its own color constants (it lives in the platform crate), mirrored from `theme.rs`; change both together.

### Motion
- **Curve:** exponential ease-out, `1 − 2^(−10t)`: a fast start and a long settle with no overshoot. egui's default animation time is 0.16 s.
- **Durations:** hover 0.12 s, press 0.08 s, toggle/active 0.16 s, selection 0.18 s, disclosure 0.22 s, sign-in sheet 0.28 s, overlay 0.30 s, page change 0.32 s, banner 0.35 s, ID digits 0.55 s each.
- **ID digits:** the first time an ID appears, each digit rises from 55 % of the line height below its own clipped cell and fades in. They follow one another 35 ms apart after a 50 ms delay.
- **Page change:** the new page fades in while it rises 6 px. **Sign-in sheet:** fades in and rises 14 px while the backdrop dims. **Overlay:** fades in and rises 8 px.
- **Reduced motion:** when Windows "Efeitos de animação" is off (read once at launch), every transition jumps to its end state. Pulses, shimmer and the spinner hold still. The consent countdown keeps running because it is a live timer, not a transition.

## Do's and Don'ts

### Do:
- **Do** follow the Windows light/dark setting and design every screen in both palettes.
- **Do** keep one filled Action Blue button per region, and give a disabled primary the Selection Wash fill with dimmed Link Blue text.
- **Do** use a 1 px Field Edge outline on a Page fill for text fields. The outline turns Muted on hover and Action Blue on focus.
- **Do** keep tertiary text on Faint (#686b72 / #95979e) so it stays at 4.5:1 or better on Page, Rail and Control Fill.
- **Do** put status in a dot plus a plain line of text. Pulse only states that are still in progress.
- **Do** show data the app cannot confirm as "Última lista conhecida" with neutral dots and Conectar disabled with the reason.
- **Do** run every transition through the shared ease-out helpers so that turning off Windows animation effects stops it.
- **Do** draw new icons on the 16-unit grid with a 1.5-unit stroke and round caps.

### Don't:
- **Don't** put gradients on controls, glows, neon, or colored shadows anywhere.
- **Don't** use a monogram or letter mark. The mark is the two screens, on Brand Tile Blue (#1b6aeb), and the accent never replaces it.
- **Don't** lay content out as card grids. Lists are hairline-separated rows.
- **Don't** use green for anything that is not online, connected or in session.
- **Don't** set the ID or other numbers in a monospace or display face. The ID is Segoe UI Semibold.
- **Don't** fill text fields with the control gray, and don't use that gray as a disabled primary.
- **Don't** let chrome overlap the remote picture. The session toolbar sits above it, never on top of it.
- **Don't** give the focused Recusar in the consent prompt a blue ring.

## Not yet in this language: installer, website

The installer (`crates/setup`) and the website (`site/`) have not been redesigned and still carry the retired look. Nothing in this file describes them. Bring them into this language before documenting them here. The `theme::card` frame and `theme::toggle_chip` switch serve the installer and the development launcher, and they are not part of the system above.
