//! Device records, proof-of-possession challenges and revocation (shared by the API,
//! the signaling service and the admin CLI).

use aionix_control_api::{
    rest::{DeviceSummary, PresenceState},
    signal::ServerMessage,
};
use aionix_crypto::{PublicKey, random_token, sha256_hex};
use aionix_shared::{DeviceKind, FriendlyId};
use serde_json::json;
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use tracing::warn;
use uuid::Uuid;

use crate::{
    audit::{AuditRecord, Outcome, action},
    bus::{DeviceBusMessage, SignalBus},
    presence::PresenceRecord,
    redis_conn::Keys,
};

/// Challenge lifetime.
pub const CHALLENGE_TTL: Duration = Duration::seconds(60);

macro_rules! device_columns {
    () => {
        "id, friendly_id, public_key, kind, org_id, customer_id, owner_user_id, display_name,
         hostname, os_name, os_version, app_version, unattended_enabled, last_seen_at, revoked_at"
    };
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DeviceRow {
    pub id: Uuid,
    pub friendly_id: i32,
    pub public_key: String,
    pub kind: String,
    pub org_id: Option<Uuid>,
    pub customer_id: Option<Uuid>,
    pub owner_user_id: Option<Uuid>,
    pub display_name: String,
    pub hostname: String,
    pub os_name: String,
    pub os_version: String,
    pub app_version: String,
    pub unattended_enabled: bool,
    pub last_seen_at: Option<OffsetDateTime>,
    pub revoked_at: Option<OffsetDateTime>,
}

impl DeviceRow {
    pub fn kind(&self) -> DeviceKind {
        DeviceKind::parse(&self.kind).unwrap_or(DeviceKind::QuickSupport)
    }

    pub fn friendly(&self) -> FriendlyId {
        FriendlyId::new(self.friendly_id as u32).expect("CHECK constraint keeps 9 digits")
    }

    pub fn key(&self) -> Option<PublicKey> {
        PublicKey::from_base64(&self.public_key).ok()
    }

    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    pub fn name(&self) -> String {
        if self.display_name.is_empty() {
            self.hostname.clone()
        } else {
            self.display_name.clone()
        }
    }

    pub fn summary(&self, presence: Option<&PresenceRecord>) -> DeviceSummary {
        let state = crate::presence::state_of(presence);
        DeviceSummary {
            id: self.id,
            friendly_id: self.friendly(),
            kind: self.kind(),
            display_name: self.name(),
            hostname: self.hostname.clone(),
            os_name: self.os_name.clone(),
            os_version: self.os_version.clone(),
            online: state == PresenceState::Online,
            presence: state,
            unattended_enabled: self.unattended_enabled,
            favorite: false,
            customer_id: self.customer_id,
            customer_name: None,
            last_seen_at: self.last_seen_at,
            last_session_at: None,
        }
    }
}

pub async fn by_id(db: &PgPool, id: Uuid) -> Result<Option<DeviceRow>, sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT ",
        device_columns!(),
        " FROM devices WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(db)
    .await
}

pub async fn by_friendly(db: &PgPool, f: FriendlyId) -> Result<Option<DeviceRow>, sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT ",
        device_columns!(),
        " FROM devices WHERE friendly_id = $1"
    ))
    .bind(f.value() as i32)
    .fetch_optional(db)
    .await
}

pub async fn by_key(db: &PgPool, key: &PublicKey) -> Result<Option<DeviceRow>, sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT ",
        device_columns!(),
        " FROM devices WHERE public_key = $1"
    ))
    .bind(key.to_base64())
    .fetch_optional(db)
    .await
}

pub async fn by_org(db: &PgPool, org: Uuid) -> Result<Vec<DeviceRow>, sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT ",
        device_columns!(),
        " FROM devices WHERE org_id = $1 AND revoked_at IS NULL ORDER BY display_name, hostname, id"
    ))
    .bind(org)
    .fetch_all(db)
    .await
}

pub async fn mark_seen(db: &PgPool, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE devices SET last_seen_at = now() WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Issues a single-use nonce; only its hash is stored.
pub async fn new_challenge(db: &PgPool) -> Result<String, sqlx::Error> {
    let nonce = random_token(32);
    sqlx::query("INSERT INTO device_challenges (nonce_hash, expires_at) VALUES ($1, $2)")
        .bind(sha256_hex(nonce.as_bytes()))
        .bind(OffsetDateTime::now_utc() + CHALLENGE_TTL)
        .execute(db)
        .await?;
    Ok(nonce)
}

/// Consumes a nonce. `false` if unknown, already used or expired.
pub async fn consume_challenge(db: &PgPool, nonce: &str) -> Result<bool, sqlx::Error> {
    if nonce.is_empty() || nonce.len() > 128 {
        return Ok(false);
    }
    let found: Option<i32> = sqlx::query_scalar(
        "DELETE FROM device_challenges WHERE nonce_hash = $1 AND expires_at > now() RETURNING 1",
    )
    .bind(sha256_hex(nonce.as_bytes()))
    .fetch_optional(db)
    .await?;
    Ok(found.is_some())
}

/// Revokes a device identity and propagates the revocation:
/// * PostgreSQL: `revoked_at` (device login, signaling, session requests refuse it);
/// * active sessions involving it are failed, and hosts are told to drop sessions
///   opened from a revoked technician workstation;
/// * the signaling link receives `DeviceRevoked` and is closed;
/// * the relay is told to refuse it before its relay grant expires.
///
/// Returns `false` if it was already revoked (idempotent).
pub async fn revoke(
    db: &PgPool,
    bus: &dyn SignalBus,
    redis: Option<&(redis::aio::ConnectionManager, Keys)>,
    device: Uuid,
    by_user: Option<Uuid>,
    reason: &str,
    client: (Option<&str>, Option<&str>),
) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;
    let org: Option<Option<Uuid>> = sqlx::query_scalar(
        "UPDATE devices SET revoked_at = now(), revoked_by = $2, revoked_reason = $3, updated_at = now()
         WHERE id = $1 AND revoked_at IS NULL RETURNING org_id",
    )
    .bind(device)
    .bind(by_user)
    .bind(reason)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(org) = org else {
        return Ok(false);
    };
    let ended: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "UPDATE sessions SET status = 'failed', ended_at = now(), end_reason = 'device_revoked'
         WHERE (host_device_id = $1 OR technician_device_id = $1)
           AND status IN ('requested', 'offered', 'accepted', 'approved', 'connected')
         RETURNING id, host_device_id, technician_device_id",
    )
    .bind(device)
    .fetch_all(&mut *tx)
    .await?;
    let mut rec = AuditRecord::new(action::DEVICE_REVOKED, Outcome::Success)
        .org(org)
        .target_device(device)
        .client(client.0, client.1)
        .details(json!({"reason": reason, "sessions_ended": ended.len()}));
    if let Some(u) = by_user {
        rec = rec.user(u);
    }
    rec.write(&mut *tx).await?;
    tx.commit().await?;

    if let Err(e) = bus.publish_device(device, &DeviceBusMessage::Revoked).await {
        warn!(error = %e, %device, "could not notify signaling of revocation");
    }
    for (sid, host, tech) in ended {
        if tech == device && host != device {
            let msg = DeviceBusMessage::Deliver {
                message: ServerMessage::SessionRevoked { session_id: sid },
            };
            if let Err(e) = bus.publish_device(host, &msg).await {
                warn!(error = %e, session_id = %sid, "could not notify host of session revocation");
            }
        }
    }
    if let Some((conn, keys)) = redis {
        let mut c = conn.clone();
        let ttl = aionix_control_api::claims::RelayClaims::TTL.whole_seconds() + 120;
        let r: Result<(), _> = redis::cmd("SET")
            .arg(keys.revoked_device(device))
            .arg(reason)
            .arg("EX")
            .arg(ttl)
            .query_async(&mut c)
            .await;
        if let Err(e) = r {
            warn!(error = %e, %device, "could not publish relay revocation marker");
        }
    }
    Ok(true)
}
