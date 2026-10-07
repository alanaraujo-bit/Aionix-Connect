//! Session metadata lifecycle (shared by the API and the signaling service).
//!
//! ```text
//! requested ─▶ offered ─▶ accepted ─▶ approved ─▶ connected ─▶ ended
//!                 │           │           │            └──────▶ failed
//!                 └▶ rejected └▶ denied   └▶ failed / ended
//! (any non-terminal) ─▶ failed | expired (janitor)
//! ```
//!
//! `approved`/`denied` record the host user's answer to the visible consent prompt,
//! as reported by the host. Only metadata is stored — never screen, input or
//! clipboard content.

use aionix_control_api::rest::SessionEventKind;
use aionix_shared::TransportKind;
use serde_json::json;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::audit::{AuditRecord, Outcome, action};

pub const ACTIVE_STATUSES: &[&str] = &["requested", "offered", "accepted", "approved", "connected"];
pub const TERMINAL_STATUSES: &[&str] = &["rejected", "denied", "ended", "failed", "expired"];

pub fn is_terminal(status: &str) -> bool {
    TERMINAL_STATUSES.contains(&status)
}

/// Who reports an event. Reports are only accepted from the session's own peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reporter {
    Host { device: Uuid },
    Technician { user: Uuid },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventOutcome {
    Recorded,
    /// The reporter is not a peer of this session (or it does not exist).
    NotAllowed,
}

pub fn transport_str(t: TransportKind) -> &'static str {
    match t {
        TransportKind::Lan => "lan",
        TransportKind::Direct => "direct",
        TransportKind::Relay => "relay",
        TransportKind::Unknown => "unknown",
    }
}

#[derive(sqlx::FromRow)]
struct SessionPeers {
    host_device_id: Uuid,
    technician_user_id: Uuid,
    org_id: Option<Uuid>,
    status: String,
}

/// Next status for an event, or `None` to keep the current one.
fn next_status(current: &str, kind: SessionEventKind, reporter: Reporter) -> Option<&'static str> {
    if is_terminal(current) {
        return None;
    }
    let host = matches!(reporter, Reporter::Host { .. });
    match kind {
        SessionEventKind::AuthGranted if host => {
            matches!(current, "requested" | "offered" | "accepted").then_some("approved")
        }
        SessionEventKind::AuthDenied if host => Some("denied"),
        SessionEventKind::Connected => {
            matches!(current, "accepted" | "approved").then_some("connected")
        }
        SessionEventKind::Ended => Some("ended"),
        SessionEventKind::Failed => Some("failed"),
        _ => None,
    }
}

/// Records a lifecycle report and advances the session status.
pub async fn record_event(
    db: &PgPool,
    session: Uuid,
    reporter: Reporter,
    kind: SessionEventKind,
    transport: Option<TransportKind>,
    detail: &str,
    client: (Option<&str>, Option<&str>),
) -> Result<EventOutcome, sqlx::Error> {
    let detail: String = detail
        .chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect();
    let mut tx = db.begin().await?;
    let peers: Option<SessionPeers> = sqlx::query_as(
        "SELECT host_device_id, technician_user_id, org_id, status FROM sessions
         WHERE id = $1 FOR UPDATE",
    )
    .bind(session)
    .fetch_optional(&mut *tx)
    .await?;
    let allowed = match (&peers, reporter) {
        (Some(p), Reporter::Host { device }) => p.host_device_id == device,
        (Some(p), Reporter::Technician { user }) => p.technician_user_id == user,
        (None, _) => false,
    };
    if !allowed {
        drop(tx);
        let mut rec = AuditRecord::security(action::PERMISSION_DENIED, Outcome::Denied)
            .session(session)
            .client(client.0, client.1)
            .details(json!({"reason": "session_event_from_non_peer", "kind": kind.as_str()}));
        rec = match reporter {
            Reporter::Host { device } => rec.device(device),
            Reporter::Technician { user } => rec.user(user),
        };
        rec.write(db).await?;
        return Ok(EventOutcome::NotAllowed);
    }
    let p = peers.expect("checked above");
    let (source, actor_user, actor_device) = match reporter {
        Reporter::Host { device } => ("host", None, Some(device)),
        Reporter::Technician { user } => ("technician", Some(user), None),
    };
    sqlx::query(
        "INSERT INTO session_events (session_id, source, kind, transport, detail)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(session)
    .bind(source)
    .bind(kind.as_str())
    .bind(transport.map(transport_str))
    .bind(&detail)
    .execute(&mut *tx)
    .await?;

    let now = OffsetDateTime::now_utc();
    if let Some(next) = next_status(&p.status, kind, reporter) {
        sqlx::query(
            "UPDATE sessions SET status = $2,
                 approved_at  = CASE WHEN $2 = 'approved' THEN $3 ELSE approved_at END,
                 connected_at = CASE WHEN $2 = 'connected' THEN COALESCE(connected_at, $3) ELSE connected_at END,
                 ended_at     = CASE WHEN $2 IN ('denied', 'ended', 'failed') THEN $3 ELSE ended_at END,
                 end_reason   = CASE WHEN $2 IN ('denied', 'ended', 'failed') THEN NULLIF($4, '') ELSE end_reason END,
                 transport    = COALESCE($5, transport)
             WHERE id = $1",
        )
        .bind(session)
        .bind(next)
        .bind(now)
        .bind(&detail)
        .bind(transport.map(transport_str))
        .execute(&mut *tx)
        .await?;
        let audit_action = match next {
            "approved" => Some((action::SESSION_APPROVED, Outcome::Success)),
            "denied" => Some((action::SESSION_REJECTED, Outcome::Denied)),
            "ended" => Some((action::SESSION_ENDED, Outcome::Success)),
            "failed" => Some((action::SESSION_ENDED, Outcome::Failure)),
            _ => None,
        };
        if let Some((a, o)) = audit_action {
            let mut rec = AuditRecord::new(a, o)
                .org(p.org_id)
                .session(session)
                .target_device(p.host_device_id)
                .client(client.0, client.1)
                .details(json!({"reported_by": source, "previous_status": p.status, "detail": detail}));
            if let Some(u) = actor_user {
                rec = rec.user(u);
            }
            if let Some(d) = actor_device {
                rec = rec.device(d);
            }
            rec.write(&mut *tx).await?;
        }
    } else if transport.is_some() {
        sqlx::query("UPDATE sessions SET transport = $2 WHERE id = $1")
            .bind(session)
            .bind(transport.map(transport_str))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(EventOutcome::Recorded)
}

/// Sessions counted as active in diagnostics (consented and not finished).
pub async fn active_count(db: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM sessions WHERE status IN ('approved', 'connected')")
        .fetch_one(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: Reporter = Reporter::Host {
        device: Uuid::nil(),
    };
    const TECH: Reporter = Reporter::Technician { user: Uuid::nil() };

    #[test]
    fn status_transitions() {
        use SessionEventKind::*;
        assert_eq!(next_status("accepted", AuthGranted, HOST), Some("approved"));
        // Only the host reports the consent decision.
        assert_eq!(next_status("accepted", AuthGranted, TECH), None);
        assert_eq!(next_status("accepted", AuthDenied, TECH), None);
        assert_eq!(next_status("approved", Connected, TECH), Some("connected"));
        assert_eq!(next_status("requested", Connected, HOST), None);
        assert_eq!(next_status("connected", Ended, TECH), Some("ended"));
        // Terminal states never change.
        assert_eq!(next_status("ended", Connected, HOST), None);
        assert_eq!(next_status("denied", AuthGranted, HOST), None);
        assert_eq!(next_status("connected", Reconnecting, HOST), None);
    }
}
