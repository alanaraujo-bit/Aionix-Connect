//! Periodic housekeeping run by `aionix-api`:
//!
//! * offered/accepted sessions whose ticket expired without the host reporting
//!   approval become `expired`;
//! * approved/connected sessions whose host device is offline become `failed`
//!   (`host_offline`) — stale sessions do not stay "active" forever;
//! * expired device challenges are deleted.
//!
//! Every statement is idempotent, so several API replicas may run it concurrently.

use std::{sync::Arc, time::Duration};

use aionix_control_api::rest::PresenceState;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use crate::presence::{PresenceStore, state_of};

/// Grace after ticket expiry: the host validates the ticket at connection time, then
/// the consent prompt may still take up to its own timeout.
const APPROVAL_GRACE: &str = "5 minutes";
/// An approved/connected session survives this long without host presence (covers
/// signaling reconnects).
pub const STALE_AFTER: Duration = Duration::from_secs(120);

pub async fn run_once(db: &PgPool, presence: &dyn PresenceStore) -> anyhow::Result<()> {
    let expired = sqlx::query(
        "UPDATE sessions SET status = 'expired', ended_at = now(), end_reason = 'ticket_expired'
         WHERE status IN ('requested', 'offered', 'accepted')
           AND ticket_expires_at < now() - $1::interval",
    )
    .bind(APPROVAL_GRACE)
    .execute(db)
    .await?
    .rows_affected();

    let live: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, host_device_id FROM sessions
         WHERE status IN ('approved', 'connected')
           AND COALESCE(connected_at, approved_at, requested_at) < now() - make_interval(secs => $1)",
    )
    .bind(STALE_AFTER.as_secs_f64())
    .fetch_all(db)
    .await?;
    let hosts: Vec<Uuid> = live.iter().map(|(_, h)| *h).collect();
    let mut stale = 0;
    if !hosts.is_empty() {
        let present = presence.get_many(&hosts).await?;
        for (sid, host) in live {
            if state_of(present.get(&host)) == PresenceState::Offline {
                stale += sqlx::query(
                    "UPDATE sessions SET status = 'failed', ended_at = now(), end_reason = 'host_offline'
                     WHERE id = $1 AND status IN ('approved', 'connected')",
                )
                .bind(sid)
                .execute(db)
                .await?
                .rows_affected();
            }
        }
    }
    let challenges = sqlx::query("DELETE FROM device_challenges WHERE expires_at < now()")
        .execute(db)
        .await?
        .rows_affected();
    if expired + stale + challenges > 0 {
        info!(expired, stale, challenges, "janitor pass");
    }
    Ok(())
}

pub fn spawn(
    db: PgPool,
    presence: Arc<dyn PresenceStore>,
    every: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if let Err(e) = run_once(&db, presence.as_ref()).await {
                warn!(error = %e, "janitor pass failed");
            }
        }
    })
}
