//! Administration panel API against the real API (PostgreSQL + Redis): accounts with
//! temporary passwords, the authorization rules of src/api/admin.rs, the organization's
//! sessions and history.

mod common;

use std::{sync::Arc, time::Duration};

use aionix_control_api::{rest::OrgRole, signal::ServerMessage};
use aionix_control_plane::{
    admin,
    bus::{DeviceBusMessage, RedisBus, SignalBus},
    testing::{TEST_PASSWORD, TestApi, member_of, org_member},
};
use aionix_crypto::IdentityKey;
use aionix_shared::DeviceKind;
use common::{Http, code};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

async fn token(http: &Http, email: &str, password: &str) -> String {
    let (s, v) = http.login(email, password).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["access_token"].as_str().unwrap().to_owned()
}

async fn create(http: &Http, access: &str, org: Uuid, email: &str, role: &str) -> (StatusCode, Value) {
    http.post(
        &format!("/v1/orgs/{org}/accounts"),
        json!({"email": email, "display_name": "Amigo Teste", "role": role}),
        Some(access),
    )
    .await
}

#[tokio::test]
async fn new_account_must_replace_its_temporary_password() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let (org, _, owner_email) = org_member(&api.state.db, OrgRole::Owner).await;
    let owner = http.access(&owner_email).await;

    let (s, v) = create(&http, &owner, org, "  Amigo@Example.TEST ", "technician").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let temporary = v["temporary_password"].as_str().unwrap().to_owned();
    assert_eq!(temporary.len(), 19);
    assert_eq!(v["member"]["role"], "technician");
    assert_eq!(v["member"]["must_change_password"], true);
    // Same e-mail again: 409, the account is not touched.
    let (s, _) = create(&http, &owner, org, "amigo@example.test", "viewer").await;
    assert_eq!(s, StatusCode::CONFLICT);

    // The temporary password signs in, flagged.
    let (s, t1) = http.login("amigo@example.test", &temporary).await;
    assert_eq!(s, StatusCode::OK, "{t1}");
    assert_eq!(t1["user"]["must_change_password"], true);
    let access1 = t1["access_token"].as_str().unwrap().to_owned();
    let access2 = token(&http, "amigo@example.test", &temporary).await;

    // …but cannot register a workstation (nor, therefore, request sessions) yet.
    let key = IdentityKey::generate();
    let (s, v) = http
        .register(&key, DeviceKind::Technician, None, Some(&access1))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    let (s, v) = http
        .post("/v1/sessions", json!({"target": "123456789"}), Some(&access1))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");

    // Changing it needs the current password and the policy.
    let (s, _) = http
        .post(
            "/v1/auth/password",
            json!({"current_password": "not it at all", "new_password": "a brand new secret"}),
            Some(&access1),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = http
        .post(
            "/v1/auth/password",
            json!({"current_password": temporary, "new_password": "short"}),
            Some(&access1),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = http
        .post(
            "/v1/auth/password",
            json!({"current_password": temporary, "new_password": "a brand new secret"}),
            Some(&access1),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // The login that changed it stays; the other one is signed out.
    let (s, me) = http.get("/v1/me", Some(&access1)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(me["must_change_password"], false);
    assert_eq!(http.get("/v1/me", Some(&access2)).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        http.login("amigo@example.test", &temporary).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (s, v) = http
        .register(&key, DeviceKind::Technician, None, Some(&access1))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // No password ever reaches the audit table.
    let leaked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE details::text LIKE $1 OR details::text LIKE '%brand new%'",
    )
    .bind(format!("%{temporary}%"))
    .fetch_one(&api.state.db)
    .await
    .unwrap();
    assert_eq!(leaked, 0);
    api.stop().await;
}

#[tokio::test]
async fn who_may_manage_whom() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let db = &api.state.db;
    let (org, owner_id, owner_email) = org_member(db, OrgRole::Owner).await;
    let (admin_id, admin_email) = member_of(db, org, OrgRole::Admin).await;
    let (admin2_id, _) = member_of(db, org, OrgRole::Admin).await;
    let (tech_id, tech_email) = member_of(db, org, OrgRole::Technician).await;
    let (other_org, stranger_id, _) = org_member(db, OrgRole::Technician).await;
    // A member here who also belongs to another organization.
    let (shared_id, _) = member_of(db, org, OrgRole::Technician).await;
    admin::set_membership(db, other_org, shared_id, OrgRole::Technician)
        .await
        .unwrap();

    let owner = http.access(&owner_email).await;
    let adm = http.access(&admin_email).await;
    let tech = http.access(&tech_email).await;
    let reset = |user: Uuid| format!("/v1/orgs/{org}/members/{user}/reset-password");

    // Technicians see members but manage nothing, and see no sessions or history.
    assert_eq!(http.get(&format!("/v1/orgs/{org}/members"), Some(&tech)).await.0, StatusCode::OK);
    assert_eq!(create(&http, &tech, org, "x@example.test", "viewer").await.0, StatusCode::FORBIDDEN);
    assert_eq!(http.post(&reset(admin_id), json!({}), Some(&tech)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(http.get(&format!("/v1/orgs/{org}/sessions"), Some(&tech)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(http.get(&format!("/v1/orgs/{org}/audit"), Some(&tech)).await.0, StatusCode::FORBIDDEN);

    // Admins: no role above their own, nobody of equal or higher rank, not themselves.
    assert_eq!(create(&http, &adm, org, "y@example.test", "owner").await.0, StatusCode::FORBIDDEN);
    assert_eq!(create(&http, &adm, org, "y@example.test", "admin").await.0, StatusCode::OK);
    assert_eq!(http.post(&reset(admin2_id), json!({}), Some(&adm)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(http.post(&reset(owner_id), json!({}), Some(&adm)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(http.post(&reset(admin_id), json!({}), Some(&adm)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(http.post(&reset(tech_id), json!({}), Some(&adm)).await.0, StatusCode::OK);
    // Owners manage admins.
    assert_eq!(http.post(&reset(admin2_id), json!({}), Some(&owner)).await.0, StatusCode::OK);
    // Not a member here: 404. Member here and elsewhere: refused.
    assert_eq!(http.post(&reset(stranger_id), json!({}), Some(&owner)).await.0, StatusCode::NOT_FOUND);
    let (s, v) = http.post(&reset(shared_id), json!({}), Some(&owner)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code(&v), "forbidden");
    // Another organization's routes: no permission there.
    assert_eq!(
        http.get(&format!("/v1/orgs/{other_org}/members"), Some(&owner)).await.0,
        StatusCode::FORBIDDEN
    );
    // Role changes: admins cannot touch admins; nobody demotes the only owner.
    let (s, _) = http
        .send(
            reqwest::Method::PUT,
            &format!("/v1/orgs/{org}/members"),
            Some(json!({"email": owner_email, "role": "admin"})),
            Some(&adm),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    api.stop().await;
}

#[tokio::test]
async fn disable_sign_out_enable_and_reset() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let db = &api.state.db;
    let (org, _, owner_email) = org_member(db, OrgRole::Owner).await;
    let (tech_id, tech_email) = member_of(db, org, OrgRole::Technician).await;
    let owner = http.access(&owner_email).await;
    let path = |what: &str| format!("/v1/orgs/{org}/members/{tech_id}/{what}");

    // Sign out everywhere: the login dies, signing in again works.
    let t = http.access(&tech_email).await;
    let (s, v) = http.post(&path("sign-out"), json!({}), Some(&owner)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["revoked_logins"], 1);
    assert_eq!(http.get("/v1/me", Some(&t)).await.0, StatusCode::UNAUTHORIZED);
    let t = http.access(&tech_email).await;

    // Disable: login dies and no new one; enable restores.
    let (s, _) = http.post(&path("disable"), json!({}), Some(&owner)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(http.get("/v1/me", Some(&t)).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(http.login(&tech_email, TEST_PASSWORD).await.0, StatusCode::UNAUTHORIZED);
    let (_, members) = http.get(&format!("/v1/orgs/{org}/members"), Some(&owner)).await;
    let me = members["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["user_id"] == tech_id.to_string())
        .unwrap();
    assert_eq!(me["disabled"], true);
    let (s, v) = http.post(&path("enable"), json!({}), Some(&owner)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["disabled"], false);
    assert_eq!(http.login(&tech_email, TEST_PASSWORD).await.0, StatusCode::OK);

    // Reset: old password gone, a temporary one that must be replaced.
    let (s, v) = http.post(&path("reset-password"), json!({}), Some(&owner)).await;
    assert_eq!(s, StatusCode::OK);
    let temporary = v["temporary_password"].as_str().unwrap().to_owned();
    assert_eq!(http.login(&tech_email, TEST_PASSWORD).await.0, StatusCode::UNAUTHORIZED);
    let (s, t) = http.login(&tech_email, &temporary).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t["user"]["must_change_password"], true);
    api.stop().await;
}

async fn device(api: &TestApi, http: &Http, org: Uuid) -> Uuid {
    let (_, token, _) = admin::insert_enrollment_token(&api.state.db, org, None, 1, 1, None)
        .await
        .unwrap();
    let (s, v) = http
        .register(&IdentityKey::generate(), DeviceKind::Agent, Some(&token), None)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["device_id"].as_str().unwrap().parse().unwrap()
}

/// A live session of `org` between `host` and `workstation`, inserted directly.
async fn live_session(api: &TestApi, org: Uuid, host: Uuid, workstation: Uuid, user: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO sessions (id, host_device_id, technician_user_id, technician_device_id, org_id,
             mode, permissions, status, ticket_jti_hash, ticket_expires_at, connected_at)
         VALUES ($1, $2, $3, $4, $5, 'assisted', ARRAY['view'], 'connected', $6, now() + interval '1 minute', now())",
    )
    .bind(id)
    .bind(host)
    .bind(user)
    .bind(workstation)
    .bind(org)
    .bind(id.to_string())
    .execute(&api.state.db)
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn sessions_are_listed_and_terminated_and_devices_told() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let db = &api.state.db;
    let (org, _, owner_email) = org_member(db, OrgRole::Owner).await;
    let (tech_id, tech_email) = member_of(db, org, OrgRole::Technician).await;
    let (other_org, _, other_owner_email) = org_member(db, OrgRole::Owner).await;
    let owner = http.access(&owner_email).await;
    let host = device(&api, &http, org).await;
    let ws = device(&api, &http, org).await;
    let other_host = device(&api, &http, other_org).await;

    // Listen to what the devices are told.
    let bus = Arc::new(RedisBus::new(
        api.redis.client.clone(),
        api.redis.conn.clone(),
        api.redis.keys.clone(),
    ));
    let mut rx = bus.device_messages().await.unwrap();
    for _ in 0..50 {
        if bus.device_listener_active() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let s1 = live_session(&api, org, host, ws, tech_id).await;
    let s_other = live_session(&api, other_org, other_host, other_host, tech_id).await;

    let (s, v) = http.get(&format!("/v1/orgs/{org}/sessions"), Some(&owner)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let list = v["sessions"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], s1.to_string());
    assert_eq!(list[0]["status"], "connected");

    // Another organization's session: not found from here.
    let terminate = |o: Uuid, s: Uuid| format!("/v1/orgs/{o}/sessions/{s}/terminate");
    assert_eq!(http.post(&terminate(org, s_other), json!({}), Some(&owner)).await.0, StatusCode::NOT_FOUND);
    let (s, _) = http.post(&terminate(org, s1), json!({}), Some(&owner)).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (status, reason): (String, Option<String>) =
        sqlx::query_as("SELECT status, end_reason FROM sessions WHERE id = $1")
            .bind(s1)
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!((status.as_str(), reason.as_deref()), ("ended", Some("terminated_by_admin")));
    assert_eq!(http.post(&terminate(org, s1), json!({}), Some(&owner)).await.0, StatusCode::CONFLICT);

    // Both computers of the session were told to stop.
    let mut told = Vec::new();
    while told.len() < 2 {
        let (dev, msg) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("devices notified")
            .unwrap();
        if let DeviceBusMessage::Deliver {
            message: ServerMessage::SessionRevoked { session_id },
        } = msg
            && session_id == s1
        {
            told.push(dev);
        }
    }
    told.sort();
    let mut expected = vec![host, ws];
    expected.sort();
    assert_eq!(told, expected);

    // Disabling the technician ends their live sessions in the organization.
    let s2 = live_session(&api, org, host, ws, tech_id).await;
    let (s, v) = http
        .post(&format!("/v1/orgs/{org}/members/{tech_id}/disable"), json!({}), Some(&owner))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // Account-wide: this organization's session and the one elsewhere.
    assert_eq!(v["ended_sessions"], 2);
    let (status, reason): (String, Option<String>) =
        sqlx::query_as("SELECT status, end_reason FROM sessions WHERE id = $1")
            .bind(s2)
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!((status.as_str(), reason.as_deref()), ("ended", Some("account_disabled")));
    let _ = (tech_email, other_owner_email);
    api.stop().await;
}

#[tokio::test]
async fn history_includes_the_members_account_events_with_names() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let db = &api.state.db;
    let (org, _, owner_email) = org_member(db, OrgRole::Owner).await;
    let (_, tech_email) = member_of(db, org, OrgRole::Technician).await;
    let (_, _, stranger_email) = org_member(db, OrgRole::Owner).await;
    let owner = http.access(&owner_email).await;
    let _ = http.access(&tech_email).await;
    let _ = http.access(&stranger_email).await;
    let (s, v) = create(&http, &owner, org, "novo@example.test", "viewer").await;
    assert_eq!(s, StatusCode::OK, "{v}");

    let (s, v) = http.get(&format!("/v1/orgs/{org}/audit?limit=200"), Some(&owner)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let events = v["events"].as_array().unwrap();
    let logins = events.iter().filter(|e| e["action"] == "auth.login").count();
    assert_eq!(logins, 2, "owner and technician logins, not the stranger's");
    let created = events
        .iter()
        .find(|e| e["action"] == "user.created")
        .expect("account creation recorded");
    assert_eq!(created["actor_name"], "Técnica Ana");
    assert_eq!(created["target_name"], "Amigo Teste");
    api.stop().await;
}
