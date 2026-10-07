//! Operator operations used by `aionix-admin` (and reused by the API and tests).
//! There is no public sign-up: organizations and first accounts are created here.

use aionix_control_api::rest::OrgRole;
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    api::{new_enrollment_secret, normalize_email, token_expiry},
    audit::{AuditRecord, Outcome, action},
    password,
    presence::PresenceStore,
};

#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("{0}")]
    Invalid(String),
    #[error("an account with this e-mail already exists")]
    DuplicateEmail,
    #[error("not found: {0}")]
    NotFound(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

fn slugify(name: &str) -> String {
    let mut s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    s.trim_matches('-').chars().take(40).collect()
}

pub async fn create_org(db: &PgPool, name: &str) -> Result<Uuid, AdminError> {
    let name = name.trim();
    if name.is_empty() || name.len() > 120 {
        return Err(AdminError::Invalid("organization name must have 1..=120 characters".into()));
    }
    let id = Uuid::now_v7();
    let slug = format!("{}-{}", slugify(name), &id.simple().to_string()[24..]);
    let mut tx = db.begin().await?;
    sqlx::query("INSERT INTO organizations (id, name, slug) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(name)
        .bind(slug)
        .execute(&mut *tx)
        .await?;
    AuditRecord::new(action::ORG_CREATED, Outcome::Success)
        .org(Some(id))
        .details(json!({"name": name, "via": "aionix-admin"}))
        .write(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn create_user(
    db: &PgPool,
    email: &str,
    display_name: &str,
    password_plain: String,
) -> Result<Uuid, AdminError> {
    let normalized = normalize_email(email);
    if !normalized.contains('@') || normalized.len() > 320 {
        return Err(AdminError::Invalid("invalid e-mail".into()));
    }
    let display_name = display_name.trim();
    if display_name.is_empty() || display_name.len() > 120 {
        return Err(AdminError::Invalid("display name must have 1..=120 characters".into()));
    }
    password::check_policy(&password_plain).map_err(|e| AdminError::Invalid(e.to_string()))?;
    let hash = password::hash(password_plain).await;
    let id = Uuid::now_v7();
    let mut tx = db.begin().await?;
    let r = sqlx::query(
        "INSERT INTO users (id, email, email_normalized, display_name, password_hash)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(email.trim())
    .bind(&normalized)
    .bind(display_name)
    .bind(hash)
    .execute(&mut *tx)
    .await;
    if let Err(e) = r {
        return Err(if crate::db::is_unique_violation(&e, None) {
            AdminError::DuplicateEmail
        } else {
            e.into()
        });
    }
    AuditRecord::new(action::USER_CREATED, Outcome::Success)
        .target_user(id)
        .details(json!({"via": "aionix-admin"}))
        .write(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn user_id_by_email(db: &PgPool, email: &str) -> Result<Uuid, AdminError> {
    sqlx::query_scalar("SELECT id FROM users WHERE email_normalized = $1")
        .bind(normalize_email(email))
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("user {email}")))
}

pub async fn set_membership(
    db: &PgPool,
    org: Uuid,
    user: Uuid,
    role: OrgRole,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO memberships (org_id, user_id, role) VALUES ($1, $2, $3)
         ON CONFLICT (org_id, user_id) DO UPDATE SET role = EXCLUDED.role, updated_at = now()",
    )
    .bind(org)
    .bind(user)
    .bind(role.as_str())
    .execute(db)
    .await?;
    Ok(())
}

/// Returns `(id, token, expires_at)`. The token is never stored, only its hash.
pub async fn insert_enrollment_token(
    db: &PgPool,
    org: Uuid,
    customer: Option<Uuid>,
    valid_hours: i64,
    max_uses: i32,
    created_by: Option<Uuid>,
) -> Result<(Uuid, String, OffsetDateTime), sqlx::Error> {
    let (token, hash) = new_enrollment_secret();
    let id = Uuid::now_v7();
    let expires_at = token_expiry(valid_hours);
    sqlx::query(
        "INSERT INTO enrollment_tokens (id, org_id, customer_id, token_hash, created_by, expires_at, max_uses)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(org)
    .bind(customer)
    .bind(hash)
    .bind(created_by)
    .bind(expires_at)
    .bind(max_uses)
    .execute(db)
    .await?;
    Ok((id, token, expires_at))
}

/// Disables an account and revokes all of its logins.
pub async fn disable_user(db: &PgPool, user: Uuid) -> Result<u64, sqlx::Error> {
    let mut tx = db.begin().await?;
    sqlx::query("UPDATE users SET disabled_at = COALESCE(disabled_at, now()), updated_at = now() WHERE id = $1")
        .bind(user)
        .execute(&mut *tx)
        .await?;
    let n = sqlx::query(
        "UPDATE auth_sessions SET revoked_at = now(), revoked_reason = 'account_disabled'
         WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(user)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    AuditRecord::new(action::USER_DISABLED, Outcome::Success)
        .target_user(user)
        .details(json!({"revoked_sessions": n, "via": "aionix-admin"}))
        .write(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(n)
}

#[derive(Debug, Serialize)]
pub struct ServerStatus {
    pub organizations: i64,
    pub users: i64,
    pub devices: i64,
    pub devices_revoked: i64,
    /// `None` when the presence store is unreachable.
    pub devices_online: Option<u64>,
    pub sessions_active: i64,
    pub sessions_last_24h: i64,
    pub schema_ready: bool,
}

pub async fn status(db: &PgPool, presence: Option<&dyn PresenceStore>) -> Result<ServerStatus, sqlx::Error> {
    let (organizations, users, devices, devices_revoked, sessions_last_24h): (i64, i64, i64, i64, i64) =
        sqlx::query_as(
            "SELECT (SELECT count(*) FROM organizations),
                    (SELECT count(*) FROM users),
                    (SELECT count(*) FROM devices WHERE revoked_at IS NULL),
                    (SELECT count(*) FROM devices WHERE revoked_at IS NOT NULL),
                    (SELECT count(*) FROM sessions WHERE requested_at > now() - interval '24 hours')",
        )
        .fetch_one(db)
        .await?;
    let devices_online = match presence {
        Some(p) => p.online_count().await.ok(),
        None => None,
    };
    Ok(ServerStatus {
        organizations,
        users,
        devices,
        devices_revoked,
        devices_online,
        sessions_active: crate::sessions::active_count(db).await?,
        sessions_last_24h,
        schema_ready: crate::db::check_ready(db).await.is_ok(),
    })
}

#[cfg(test)]
mod tests {
    use super::slugify;

    #[test]
    fn slugs() {
        assert_eq!(slugify("Suporte  Exemplo Ltda."), "suporte-exemplo-ltda");
        assert_eq!(slugify("***"), "");
    }
}
