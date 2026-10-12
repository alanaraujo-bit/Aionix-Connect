//! Organization inventory management: customers, device groups, device organization and
//! the unattended-access password (ADR-0015).
//!
//! Every handler checks its permission in the path's organization, and every referenced
//! row (customer, group, device, technician) must belong to that same organization.
//! Passwords and other secrets never appear in responses or audit details.

use aionix_control_api::rest::{
    CreateCustomerRequest, CreateGroupRequest, Customer, CustomerList, DeviceGroup, DeviceSummary,
    GroupList, PresenceState, SetUnattendedPasswordRequest, UpdateCustomerRequest,
    UpdateDeviceRequest, UpdateGroupRequest,
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde_json::json;
use uuid::Uuid;

use super::{AuthUser, ClientCtx, clean_text, limit, orgs::require};
use crate::{
    audit::{AuditRecord, Outcome, action},
    authz::Permission,
    devices,
    error::{ApiFailure, ApiResult},
    password,
    ratelimit::limits,
    state::AppState,
};

const CUSTOMER_NAME_MAX: usize = 120;
const CUSTOMER_NOTES_MAX: usize = 2000;
const GROUP_NAME_MAX: usize = 80;
const DEVICE_NAME_MAX: usize = 80;
const DEVICE_DESCRIPTION_MAX: usize = 500;

/// Trims a required name and rejects empty ones.
fn required_name(raw: &str, max: usize) -> ApiResult<String> {
    let name = clean_text(raw, max);
    if name.is_empty() {
        return Err(ApiFailure::bad_request("name is required"));
    }
    Ok(name)
}

/// Maps a unique-index violation (case-insensitive name already used) to 409.
fn unique_name(e: sqlx::Error, what: &'static str) -> ApiFailure {
    match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            ApiFailure::conflict(format!("a {what} with this name already exists"))
        }
        _ => e.into(),
    }
}

async fn exists_in_org(
    db: &sqlx::PgPool,
    table: &'static str,
    id: Uuid,
    org: Uuid,
) -> Result<bool, sqlx::Error> {
    // `table` is one of a fixed set of literals in this module, never user input.
    let sql = match table {
        "customers" => "SELECT EXISTS (SELECT 1 FROM customers WHERE id = $1 AND org_id = $2)",
        "device_groups" => {
            "SELECT EXISTS (SELECT 1 FROM device_groups WHERE id = $1 AND org_id = $2)"
        }
        _ => "SELECT EXISTS (SELECT 1 FROM memberships WHERE user_id = $1 AND org_id = $2)",
    };
    sqlx::query_scalar(sql)
        .bind(id)
        .bind(org)
        .fetch_one(db)
        .await
}

/// `GET /v1/orgs/{org}/customers`
pub async fn customers_list(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path(org): Path<Uuid>,
) -> ApiResult<Json<CustomerList>> {
    require(&state, &ctx, &user, org, Permission::DeviceView).await?;
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, name, notes FROM customers WHERE org_id = $1 ORDER BY lower(name)",
    )
    .bind(org)
    .fetch_all(&state.db)
    .await?;
    let org_devices = devices::by_org(&state.db, org).await?;
    let ids: Vec<Uuid> = org_devices.iter().map(|d| d.id).collect();
    let presence = state.presence.get_many(&ids).await?;

    let customers = rows
        .into_iter()
        .map(|(id, name, notes)| {
            let mine: Vec<_> = org_devices
                .iter()
                .filter(|d| d.customer_id == Some(id))
                .collect();
            let online = mine
                .iter()
                .filter(|d| crate::presence::state_of(presence.get(&d.id)) == PresenceState::Online)
                .count();
            Customer {
                id,
                name,
                notes,
                device_count: mine.len() as i64,
                online_count: online as i64,
            }
        })
        .collect();
    Ok(Json(CustomerList { customers }))
}

/// `POST /v1/orgs/{org}/customers`
pub async fn customers_create(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path(org): Path<Uuid>,
    Json(req): Json<CreateCustomerRequest>,
) -> ApiResult<Json<Customer>> {
    require(&state, &ctx, &user, org, Permission::DeviceManage).await?;
    let name = required_name(&req.name, CUSTOMER_NAME_MAX)?;
    let notes = clean_text(&req.notes, CUSTOMER_NOTES_MAX);
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO customers (id, org_id, name, notes) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(org)
        .bind(&name)
        .bind(&notes)
        .execute(&state.db)
        .await
        .map_err(|e| unique_name(e, "customer"))?;
    AuditRecord::new(action::CUSTOMER_CHANGED, Outcome::Success)
        .user(user.user_id)
        .org(Some(org))
        .client(ctx.ip(), ctx.rid())
        .details(json!({"change": "created", "customer_id": id}))
        .write(&state.db)
        .await?;
    Ok(Json(Customer {
        id,
        name,
        notes,
        device_count: 0,
        online_count: 0,
    }))
}

/// `PATCH /v1/orgs/{org}/customers/{id}`
pub async fn customers_update(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path((org, id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateCustomerRequest>,
) -> ApiResult<StatusCode> {
    require(&state, &ctx, &user, org, Permission::DeviceManage).await?;
    let name = match req.name.as_deref() {
        Some(n) => Some(required_name(n, CUSTOMER_NAME_MAX)?),
        None => None,
    };
    let notes = req
        .notes
        .as_deref()
        .map(|n| clean_text(n, CUSTOMER_NOTES_MAX));
    let updated = sqlx::query(
        "UPDATE customers SET name = COALESCE($3, name), notes = COALESCE($4, notes)
         WHERE id = $1 AND org_id = $2",
    )
    .bind(id)
    .bind(org)
    .bind(name.as_deref())
    .bind(notes.as_deref())
    .execute(&state.db)
    .await
    .map_err(|e| unique_name(e, "customer"))?;
    if updated.rows_affected() == 0 {
        return Err(ApiFailure::not_found("customer not found"));
    }
    AuditRecord::new(action::CUSTOMER_CHANGED, Outcome::Success)
        .user(user.user_id)
        .org(Some(org))
        .client(ctx.ip(), ctx.rid())
        .details(json!({"change": "updated", "customer_id": id}))
        .write(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/orgs/{org}/customers/{id}` — its devices stay in the organization, unlinked.
pub async fn customers_delete(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path((org, id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    require(&state, &ctx, &user, org, Permission::DeviceManage).await?;
    let deleted = sqlx::query("DELETE FROM customers WHERE id = $1 AND org_id = $2")
        .bind(id)
        .bind(org)
        .execute(&state.db)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(ApiFailure::not_found("customer not found"));
    }
    AuditRecord::new(action::CUSTOMER_CHANGED, Outcome::Success)
        .user(user.user_id)
        .org(Some(org))
        .client(ctx.ip(), ctx.rid())
        .details(json!({"change": "deleted", "customer_id": id}))
        .write(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /v1/orgs/{org}/groups`
pub async fn groups_list(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path(org): Path<Uuid>,
) -> ApiResult<Json<GroupList>> {
    require(&state, &ctx, &user, org, Permission::DeviceView).await?;
    let rows: Vec<(Uuid, String, i64)> = sqlx::query_as(
        "SELECT g.id, g.name, COUNT(d.id)
         FROM device_groups g
         LEFT JOIN devices d ON d.group_id = g.id AND d.revoked_at IS NULL
         WHERE g.org_id = $1
         GROUP BY g.id, g.name
         ORDER BY lower(g.name)",
    )
    .bind(org)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(GroupList {
        groups: rows
            .into_iter()
            .map(|(id, name, device_count)| DeviceGroup {
                id,
                name,
                device_count,
            })
            .collect(),
    }))
}

/// `POST /v1/orgs/{org}/groups`
pub async fn groups_create(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path(org): Path<Uuid>,
    Json(req): Json<CreateGroupRequest>,
) -> ApiResult<Json<DeviceGroup>> {
    require(&state, &ctx, &user, org, Permission::DeviceManage).await?;
    let name = required_name(&req.name, GROUP_NAME_MAX)?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO device_groups (id, org_id, name) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(org)
        .bind(&name)
        .execute(&state.db)
        .await
        .map_err(|e| unique_name(e, "group"))?;
    AuditRecord::new(action::GROUP_CHANGED, Outcome::Success)
        .user(user.user_id)
        .org(Some(org))
        .client(ctx.ip(), ctx.rid())
        .details(json!({"change": "created", "group_id": id}))
        .write(&state.db)
        .await?;
    Ok(Json(DeviceGroup {
        id,
        name,
        device_count: 0,
    }))
}

/// `PATCH /v1/orgs/{org}/groups/{id}`
pub async fn groups_update(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path((org, id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateGroupRequest>,
) -> ApiResult<StatusCode> {
    require(&state, &ctx, &user, org, Permission::DeviceManage).await?;
    let name = required_name(&req.name, GROUP_NAME_MAX)?;
    let updated = sqlx::query("UPDATE device_groups SET name = $3 WHERE id = $1 AND org_id = $2")
        .bind(id)
        .bind(org)
        .bind(&name)
        .execute(&state.db)
        .await
        .map_err(|e| unique_name(e, "group"))?;
    if updated.rows_affected() == 0 {
        return Err(ApiFailure::not_found("group not found"));
    }
    AuditRecord::new(action::GROUP_CHANGED, Outcome::Success)
        .user(user.user_id)
        .org(Some(org))
        .client(ctx.ip(), ctx.rid())
        .details(json!({"change": "renamed", "group_id": id}))
        .write(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/orgs/{org}/groups/{id}` — its devices stay in the organization, ungrouped.
pub async fn groups_delete(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path((org, id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    require(&state, &ctx, &user, org, Permission::DeviceManage).await?;
    let deleted = sqlx::query("DELETE FROM device_groups WHERE id = $1 AND org_id = $2")
        .bind(id)
        .bind(org)
        .execute(&state.db)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(ApiFailure::not_found("group not found"));
    }
    AuditRecord::new(action::GROUP_CHANGED, Outcome::Success)
        .user(user.user_id)
        .org(Some(org))
        .client(ctx.ip(), ctx.rid())
        .details(json!({"change": "deleted", "group_id": id}))
        .write(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Loads a live device that belongs to `org`, or fails with 404.
async fn device_in_org(state: &AppState, org: Uuid, id: Uuid) -> ApiResult<devices::DeviceRow> {
    match devices::by_id(&state.db, id).await? {
        Some(d) if d.org_id == Some(org) && !d.is_revoked() => Ok(d),
        _ => Err(ApiFailure::not_found("device not found")),
    }
}

/// `PATCH /v1/orgs/{org}/devices/{id}` — organize a device.
pub async fn device_update(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path((org, id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateDeviceRequest>,
) -> ApiResult<Json<DeviceSummary>> {
    require(&state, &ctx, &user, org, Permission::DeviceManage).await?;
    device_in_org(&state, org, id).await?;

    let display_name = req
        .display_name
        .as_deref()
        .map(|n| clean_text(n, DEVICE_NAME_MAX));
    let description = req
        .description
        .as_deref()
        .map(|d| clean_text(d, DEVICE_DESCRIPTION_MAX));
    // Each link is `None` (unchanged), `Some(None)` (cleared) or `Some(Some(id))` (set).
    if let Some(Some(c)) = req.customer_id
        && !exists_in_org(&state.db, "customers", c, org).await?
    {
        return Err(ApiFailure::bad_request("unknown customer"));
    }
    if let Some(Some(g)) = req.group_id
        && !exists_in_org(&state.db, "device_groups", g, org).await?
    {
        return Err(ApiFailure::bad_request("unknown group"));
    }
    if let Some(Some(t)) = req.assigned_user_id
        && !exists_in_org(&state.db, "memberships", t, org).await?
    {
        return Err(ApiFailure::bad_request(
            "the technician must be a member of the organization",
        ));
    }

    // One statement with per-field flags: all changes apply together or not at all.
    sqlx::query(
        "UPDATE devices SET
             display_name     = CASE WHEN $2 THEN $3 ELSE display_name END,
             description      = CASE WHEN $4 THEN $5 ELSE description END,
             customer_id      = CASE WHEN $6 THEN $7 ELSE customer_id END,
             group_id         = CASE WHEN $8 THEN $9 ELSE group_id END,
             assigned_user_id = CASE WHEN $10 THEN $11 ELSE assigned_user_id END,
             favorite         = CASE WHEN $12 THEN $13 ELSE favorite END,
             updated_at       = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(display_name.is_some())
    .bind(display_name.clone().unwrap_or_default())
    .bind(description.is_some())
    .bind(description.clone().unwrap_or_default())
    .bind(req.customer_id.is_some())
    .bind(req.customer_id.flatten())
    .bind(req.group_id.is_some())
    .bind(req.group_id.flatten())
    .bind(req.assigned_user_id.is_some())
    .bind(req.assigned_user_id.flatten())
    .bind(req.favorite.is_some())
    .bind(req.favorite.unwrap_or(false))
    .execute(&state.db)
    .await?;

    let changed: Vec<&str> = [
        ("display_name", display_name.is_some()),
        ("description", description.is_some()),
        ("customer_id", req.customer_id.is_some()),
        ("group_id", req.group_id.is_some()),
        ("assigned_user_id", req.assigned_user_id.is_some()),
        ("favorite", req.favorite.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, set)| set.then_some(name))
    .collect();
    AuditRecord::new(action::DEVICE_UPDATED, Outcome::Success)
        .user(user.user_id)
        .org(Some(org))
        .target_device(id)
        .client(ctx.ip(), ctx.rid())
        .details(json!({"fields": changed}))
        .write(&state.db)
        .await?;

    let row = devices::view_by_id(&state.db, id)
        .await?
        .ok_or_else(|| ApiFailure::not_found("device not found"))?;
    let presence = state.presence.get(id).await?;
    Ok(Json(row.summary(presence.as_ref())))
}

/// `PUT /v1/orgs/{org}/devices/{id}/unattended-password`
///
/// Sets the password that lets a technician connect without the person at the computer
/// accepting. It only works while the device owner has enabled unattended access on that
/// computer (checked live on every request, see `sessions::create`).
pub async fn unattended_password_set(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path((org, id)): Path<(Uuid, Uuid)>,
    Json(req): Json<SetUnattendedPasswordRequest>,
) -> ApiResult<StatusCode> {
    require(&state, &ctx, &user, org, Permission::DeviceUnattended).await?;
    limit(
        &state,
        "unattended-password-set",
        &user.user_id.to_string(),
        limits::ADMIN_PER_USER,
    )
    .await?;
    let dev = device_in_org(&state, org, id).await?;
    if dev.kind() != aionix_shared::DeviceKind::Agent {
        return Err(ApiFailure::bad_request(
            "unattended access is only available for managed computers (agents)",
        ));
    }
    password::check_policy(&req.password).map_err(|e| ApiFailure::bad_request(e.to_string()))?;
    let hash = password::hash(req.password).await;
    sqlx::query(
        "UPDATE devices SET unattended_password_hash = $2, unattended_password_set_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(hash)
    .execute(&state.db)
    .await?;
    AuditRecord::new(action::DEVICE_UNATTENDED_PASSWORD_SET, Outcome::Success)
        .user(user.user_id)
        .org(Some(org))
        .target_device(id)
        .client(ctx.ip(), ctx.rid())
        .write(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/orgs/{org}/devices/{id}/unattended-password` — unattended access stops
/// working for this device until a new password is set.
pub async fn unattended_password_clear(
    State(state): State<AppState>,
    ctx: ClientCtx,
    user: AuthUser,
    Path((org, id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    require(&state, &ctx, &user, org, Permission::DeviceUnattended).await?;
    device_in_org(&state, org, id).await?;
    sqlx::query(
        "UPDATE devices SET unattended_password_hash = NULL, unattended_password_set_at = NULL
         WHERE id = $1",
    )
    .bind(id)
    .execute(&state.db)
    .await?;
    AuditRecord::new(action::DEVICE_UNATTENDED_PASSWORD_CLEARED, Outcome::Success)
        .user(user.user_id)
        .org(Some(org))
        .target_device(id)
        .client(ctx.ip(), ctx.rid())
        .write(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
