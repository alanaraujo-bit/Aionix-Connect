# ADR-0015: Unattended access with explicit owner opt-in and device password

- Status: Accepted
- Date: 2026-10-12
- Amends: ADR-0007 (unattended access is no longer out of scope; assisted sessions keep
  the mandatory visible approval unchanged)

## Context

Managed computers (agents installed by the organization) must be reachable by the
organization's technicians the way TeamViewer does it: the computer appears in the
organization's list, and an administrator-set password lets a technician connect without
someone at the computer clicking "Permitir". Until now only assisted sessions existed, and
the requirements asked for consent on every session.

Unattended access is powerful: used without the owner's knowledge it is indistinguishable
from misuse. The change therefore has to keep the owner in control, visibly and
revocably, and must never happen silently.

## Decision

An unattended session is authorized only when **all** of the following hold:

1. **Owner consent, on the computer.** The owner turns on "Permitir acesso sem aceitação"
   in the app's own window ("Este computador", managed computers only). The setting
   defaults to off and is stored locally (`host-settings.json` next to the identity). Only
   the owner's window changes it. Nothing is stored on the server.
2. **Device password, set by an administrator.** An owner or administrator (permission
   `device.unattended`) sets a password of at least 12 characters for the computer. The
   server stores only an Argon2id hash and never returns it.
3. **Live check on the server.** A technician's request with the password is refused
   unless the computer is Online and its presence reports the owner's consent right now.
   Consent is never stored in the database; it expires with the connection. Guesses are
   rate limited per computer (10 per minute, all technicians together).
4. **Host check, independent of the server.** The host accepts an unattended offer and
   ticket only while its own local setting is on (`TicketValidator::set_unattended_allowed`).
   Only an `unattended` ticket skips the consent prompt. Assisted tickets still need the
   prompt (ADR-0007).
5. **Visible while it runs.** The host window shows "Acesso sem aceitação: <technician> está
   conectado" with an "Encerrar acesso" button, and the taskbar button asks for attention
   (it does not steal focus). Turning the setting off ends a running unattended session at
   once.
6. **Audited.** Every request, refusal (with its reason, never the password) and session is
   recorded in the append-only audit log with the session's mode.

Technicians type the device password for each connection; the app does not store it.

## Consequences

- Assisted access is unchanged. Unattended access is an explicit, reversible opt-in per
  computer, and it is never the default for a new install.
- The consent a person gives is "this computer may be reached without my click, while my
  setting is on", not "any session". Turning the setting off takes effect on the next
  offer, on the next ticket, and on the running session.
- The "unattended" mode is visible in the session history and the audit log.
- Still to build: a tray indicator and a Windows notification for unattended sessions, a
  password rotation reminder, and per-technician restrictions on unattended access.
- ADR-0007's claim that "tickets with mode `unattended` are refused" is replaced by the
  rules above; its visible-approval rule for assisted sessions stands.
