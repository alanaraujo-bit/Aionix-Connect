//! Append-only audit and security events.
//!
//! Records *who did what to which object, with what outcome* — never credentials,
//! tokens, clipboard text or screen content. The table refuses UPDATE/DELETE/TRUNCATE
//! through triggers (see the initial migration). Writing an audit record is part of
//! the action: handlers propagate failures instead of acting unaudited.

use serde_json::Value;
use sqlx::PgExecutor;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// Normal security-relevant activity (logins, enrollment, tickets…).
    Audit,
    /// Suspicious or defensive events (refresh-token reuse, lockouts, bad signatures…).
    Security,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Denied,
    Failure,
}

/// Stable action names. Keep in sync with SECURITY.md.
pub mod action {
    pub const LOGIN: &str = "auth.login";
    pub const LOGOUT: &str = "auth.logout";
    pub const LOGOUT_ALL: &str = "auth.logout_all";
    pub const REFRESH_REUSE: &str = "auth.refresh_reuse";
    pub const ACCOUNT_LOCKED: &str = "auth.account_locked";
    pub const DEVICE_ENROLLED: &str = "device.enrolled";
    pub const DEVICE_ENROLL_REFUSED: &str = "device.enroll_refused";
    pub const DEVICE_LOGIN_REFUSED: &str = "device.login_refused";
    pub const DEVICE_REVOKED: &str = "device.revoked";
    pub const ENROLLMENT_TOKEN_CREATED: &str = "enrollment.token_created";
    pub const MEMBER_UPSERTED: &str = "member.upserted";
    pub const PERMISSION_DENIED: &str = "permission.denied";
    pub const SESSION_REQUESTED: &str = "session.requested";
    pub const TICKET_ISSUED: &str = "session.ticket_issued";
    pub const SESSION_OFFER_ANSWERED: &str = "session.offer_answered";
    pub const SESSION_APPROVED: &str = "session.approved";
    pub const SESSION_REJECTED: &str = "session.rejected";
    pub const SESSION_ENDED: &str = "session.ended";
    pub const SIGNALING_REFUSED: &str = "signaling.refused";
    pub const USER_CREATED: &str = "user.created";
    pub const USER_DISABLED: &str = "user.disabled";
    pub const USER_ENABLED: &str = "user.enabled";
    pub const USER_SIGNED_OUT: &str = "user.signed_out";
    pub const USER_PASSWORD_RESET: &str = "user.password_reset";
    pub const USER_EMAIL_CHANGED: &str = "user.email_changed";
    pub const PASSWORD_CHANGED: &str = "auth.password_changed";
    pub const SESSION_TERMINATED: &str = "session.terminated";
    pub const ORG_CREATED: &str = "org.created";
    pub const DEVICE_UPDATED: &str = "device.updated";
    pub const DEVICE_UNATTENDED_PASSWORD_SET: &str = "device.unattended_password_set";
    pub const DEVICE_UNATTENDED_PASSWORD_CLEARED: &str = "device.unattended_password_cleared";
    pub const CUSTOMER_CHANGED: &str = "customer.changed";
    pub const GROUP_CHANGED: &str = "group.changed";
}

#[derive(Debug, Clone)]
pub struct AuditRecord {
    pub category: Category,
    pub action: &'static str,
    pub outcome: Outcome,
    pub actor_user_id: Option<Uuid>,
    pub actor_device_id: Option<Uuid>,
    pub org_id: Option<Uuid>,
    pub target_user_id: Option<Uuid>,
    pub target_device_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub client_ip: Option<String>,
    pub request_id: Option<String>,
    pub details: Value,
}

impl AuditRecord {
    pub fn new(action: &'static str, outcome: Outcome) -> Self {
        Self {
            category: Category::Audit,
            action,
            outcome,
            actor_user_id: None,
            actor_device_id: None,
            org_id: None,
            target_user_id: None,
            target_device_id: None,
            session_id: None,
            client_ip: None,
            request_id: None,
            details: Value::Object(Default::default()),
        }
    }

    pub fn security(action: &'static str, outcome: Outcome) -> Self {
        Self {
            category: Category::Security,
            ..Self::new(action, outcome)
        }
    }

    pub fn user(mut self, id: Uuid) -> Self {
        self.actor_user_id = Some(id);
        self
    }
    pub fn device(mut self, id: Uuid) -> Self {
        self.actor_device_id = Some(id);
        self
    }
    pub fn org(mut self, id: Option<Uuid>) -> Self {
        self.org_id = id;
        self
    }
    pub fn target_user(mut self, id: Uuid) -> Self {
        self.target_user_id = Some(id);
        self
    }
    pub fn target_device(mut self, id: Uuid) -> Self {
        self.target_device_id = Some(id);
        self
    }
    pub fn session(mut self, id: Uuid) -> Self {
        self.session_id = Some(id);
        self
    }
    pub fn client(mut self, ip: Option<&str>, request_id: Option<&str>) -> Self {
        self.client_ip = ip.map(str::to_owned);
        self.request_id = request_id.map(str::to_owned);
        self
    }
    pub fn details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    pub async fn write<'e>(self, db: impl PgExecutor<'e>) -> Result<(), sqlx::Error> {
        tracing::info!(
            target: "aionix::audit",
            category = match self.category { Category::Audit => "audit", Category::Security => "security" },
            action = self.action,
            outcome = outcome_str(self.outcome),
            user_id = ?self.actor_user_id,
            device_id = ?self.actor_device_id,
            org_id = ?self.org_id,
            session_id = ?self.session_id,
            request_id = ?self.request_id,
            "audit event"
        );
        sqlx::query(
            "INSERT INTO audit_events (category, action, outcome, actor_user_id, actor_device_id,
                 org_id, target_user_id, target_device_id, session_id, client_ip, request_id, details)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(match self.category {
            Category::Audit => "audit",
            Category::Security => "security",
        })
        .bind(self.action)
        .bind(outcome_str(self.outcome))
        .bind(self.actor_user_id)
        .bind(self.actor_device_id)
        .bind(self.org_id)
        .bind(self.target_user_id)
        .bind(self.target_device_id)
        .bind(self.session_id)
        .bind(self.client_ip)
        .bind(self.request_id)
        .bind(self.details)
        .execute(db)
        .await?;
        Ok(())
    }
}

fn outcome_str(o: Outcome) -> &'static str {
    match o {
        Outcome::Success => "success",
        Outcome::Denied => "denied",
        Outcome::Failure => "failure",
    }
}
