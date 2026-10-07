//! Role-based permissions stored in PostgreSQL (`roles`, `permissions`,
//! `role_permissions`, `memberships`). Every check reads the database, so a role change
//! or a removed membership takes effect on the next request.

use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    DeviceView,
    DeviceManage,
    DeviceRevoke,
    EnrollmentCreate,
    SessionRequest,
    SessionControl,
    SessionClipboard,
    MemberView,
    MemberManage,
    AuditView,
}

impl Permission {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DeviceView => "device.view",
            Self::DeviceManage => "device.manage",
            Self::DeviceRevoke => "device.revoke",
            Self::EnrollmentCreate => "enrollment.create",
            Self::SessionRequest => "session.request",
            Self::SessionControl => "session.control",
            Self::SessionClipboard => "session.clipboard",
            Self::MemberView => "member.view",
            Self::MemberManage => "member.manage",
            Self::AuditView => "audit.view",
        }
    }
}

/// Whether `user` holds `perm` in `org` (active organization, existing membership).
pub async fn has_permission(
    db: &PgPool,
    user: Uuid,
    org: Uuid,
    perm: Permission,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM memberships m
             JOIN organizations o ON o.id = m.org_id AND o.disabled_at IS NULL
             JOIN role_permissions rp ON rp.role = m.role
             WHERE m.user_id = $1 AND m.org_id = $2 AND rp.permission = $3)",
    )
    .bind(user)
    .bind(org)
    .bind(perm.as_str())
    .fetch_one(db)
    .await
}

/// All permissions of `user` in `org`.
pub async fn permissions_in(
    db: &PgPool,
    user: Uuid,
    org: Uuid,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT rp.permission FROM memberships m
         JOIN organizations o ON o.id = m.org_id AND o.disabled_at IS NULL
         JOIN role_permissions rp ON rp.role = m.role
         WHERE m.user_id = $1 AND m.org_id = $2
         ORDER BY rp.permission",
    )
    .bind(user)
    .bind(org)
    .fetch_all(db)
    .await
}

/// Organizations (oldest membership first) where `user` holds `perm`.
pub async fn orgs_with_permission(
    db: &PgPool,
    user: Uuid,
    perm: Permission,
) -> Result<Vec<(Uuid, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT o.id, o.name FROM memberships m
         JOIN organizations o ON o.id = m.org_id AND o.disabled_at IS NULL
         JOIN role_permissions rp ON rp.role = m.role
         WHERE m.user_id = $1 AND rp.permission = $2
         ORDER BY m.created_at, o.id",
    )
    .bind(user)
    .bind(perm.as_str())
    .fetch_all(db)
    .await
}
