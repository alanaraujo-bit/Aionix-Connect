//! Device enrollment and device login against the real API.

mod common;

use aionix_control_api::{
    claims::{DeviceClaims, RelayClaims},
    rest::OrgRole,
};
use aionix_control_plane::{
    admin,
    testing::{TestApi, org_member},
};
use aionix_crypto::{IdentityKey, SignatureDomain, sign_domain};
use aionix_shared::DeviceKind;
use common::{Http, code};
use reqwest::StatusCode;
use serde_json::json;

#[tokio::test]
async fn agent_enrollment_duplicate_and_token_limits() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let (org, _, _) = org_member(&api.state.db, OrgRole::Admin).await;
    let (_, token, _) = admin::insert_enrollment_token(&api.state.db, org, None, 1, 1, None)
        .await
        .unwrap();

    let key = IdentityKey::generate();
    let (s, v) = http
        .register(&key, DeviceKind::Agent, Some(&token), None)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let friendly = v["friendly_id"].as_str().unwrap();
    assert_eq!(friendly.len(), 9);
    let device_id: uuid::Uuid = v["device_id"].as_str().unwrap().parse().unwrap();

    // Only the public key is stored, bound to the organization.
    let (stored_key, stored_org): (String, Option<uuid::Uuid>) =
        sqlx::query_as("SELECT public_key, org_id FROM devices WHERE id = $1")
            .bind(device_id)
            .fetch_one(&api.state.db)
            .await
            .unwrap();
    assert_eq!(stored_key, key.public().to_base64());
    assert_eq!(stored_org, Some(org));

    // Duplicate registration of the same identity.
    let (s, v) = http
        .register(&key, DeviceKind::Agent, Some(&token), None)
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");

    // The single-use token is exhausted.
    let other = IdentityKey::generate();
    let (s, v) = http
        .register(&other, DeviceKind::Agent, Some(&token), None)
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(code(&v), "enrollment_invalid");
    // No token / forged token.
    let (s, v) = http.register(&other, DeviceKind::Agent, None, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(code(&v), "enrollment_invalid");
    let (s, _) = http
        .register(&other, DeviceKind::Agent, Some("axe1_forged"), None)
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let enrolled: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action = 'device.enrolled' AND target_device_id = $1",
    )
    .bind(device_id)
    .fetch_one(&api.state.db)
    .await
    .unwrap();
    assert_eq!(enrolled, 1);
    api.stop().await;
}

#[tokio::test]
async fn invalid_identities_are_refused() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let key = IdentityKey::generate();
    let impostor = IdentityKey::generate();

    // Signature made by another key.
    let nonce = http.challenge().await;
    let sig = sign_domain(
        &impostor,
        SignatureDomain::DeviceRegistration,
        nonce.as_bytes(),
    );
    let body = |nonce: &str, sig: &str| {
        json!({"public_key": key.public(), "kind": "quick_support", "nonce": nonce,
               "signature": sig, "info": {"hostname": "x", "os_name": "", "os_version": "", "app_version": ""}})
    };
    let (s, v) = http
        .post("/v1/devices/register", body(&nonce, &sig), None)
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&v), "invalid_signature");

    // Signature for the login domain is not a registration proof.
    let nonce = http.challenge().await;
    let sig = sign_domain(&key, SignatureDomain::DeviceLogin, nonce.as_bytes());
    let (s, v) = http
        .post("/v1/devices/register", body(&nonce, &sig), None)
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&v), "invalid_signature");

    // A nonce cannot be used twice, nor invented.
    let nonce = http.challenge().await;
    let sig = sign_domain(&key, SignatureDomain::DeviceRegistration, nonce.as_bytes());
    let (s, _) = http
        .post("/v1/devices/register", body(&nonce, &sig), None)
        .await;
    assert_eq!(s, StatusCode::OK);
    let other = IdentityKey::generate();
    let sig2 = sign_domain(
        &other,
        SignatureDomain::DeviceRegistration,
        nonce.as_bytes(),
    );
    let body2 = json!({"public_key": other.public(), "kind": "quick_support", "nonce": nonce,
                       "signature": sig2, "info": {"hostname": "x", "os_name": "", "os_version": "", "app_version": ""}});
    let (s, v) = http.post("/v1/devices/register", body2, None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(code(&v), "challenge_expired");

    // Malformed public key.
    let (s, _) = http
        .post(
            "/v1/devices/register",
            json!({"public_key": "not-a-key", "kind": "quick_support", "nonce": "n", "signature": "s",
                   "info": {"hostname": "", "os_name": "", "os_version": "", "app_version": ""}}),
            None,
        )
        .await;
    assert!(s.is_client_error());

    // Technician workstations need a signed-in user.
    let (s, _) = http
        .register(&IdentityKey::generate(), DeviceKind::Technician, None, None)
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let refused: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action = 'device.enroll_refused' AND category = 'security'",
    )
    .fetch_one(&api.state.db)
    .await
    .unwrap();
    assert!(refused >= 3, "{refused}");
    api.stop().await;
}

#[tokio::test]
async fn device_login_issues_bound_tokens_and_refuses_revoked_devices() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let (org, admin_user, email) = org_member(&api.state.db, OrgRole::Admin).await;
    let (_, token, _) = admin::insert_enrollment_token(&api.state.db, org, None, 1, 5, None)
        .await
        .unwrap();
    let key = IdentityKey::generate();
    let (_, reg) = http
        .register(&key, DeviceKind::Agent, Some(&token), None)
        .await;
    let device_id: uuid::Uuid = reg["device_id"].as_str().unwrap().parse().unwrap();

    // Unregistered keys cannot log in.
    let (s, _) = http.device_login(&IdentityKey::generate()).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let (s, v) = http.device_login(&key).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["friendly_id"], reg["friendly_id"]);
    let dt = api
        .state
        .tokens
        .verify::<DeviceClaims>(v["device_token"].as_str().unwrap())
        .unwrap();
    assert_eq!(dt.claims.sub, device_id);
    assert_eq!(dt.claims.key, key.public());
    let relay = &v["relays"][0];
    assert_eq!(relay["url"], "https://relay.aionix.test/");
    let rt = api
        .state
        .tokens
        .verify::<RelayClaims>(relay["token"].as_str().unwrap())
        .unwrap();
    assert_eq!(rt.claims.key, key.public());
    // A device token is not an access token.
    assert_eq!(
        http.get("/v1/me", v["device_token"].as_str()).await.0,
        StatusCode::UNAUTHORIZED
    );

    // Revocation through the API (admin of the org).
    let access = http.access(&email).await;
    let (s, _) = http
        .post(
            &format!("/v1/devices/{device_id}/revoke"),
            json!({"reason": "stolen laptop"}),
            Some(&access),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, v) = http.device_login(&key).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(code(&v), "device_revoked");
    // Re-registering a revoked identity is refused too.
    let (s, v) = http
        .register(&key, DeviceKind::Agent, Some(&token), None)
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(code(&v), "device_revoked");
    // The relay is told to refuse it.
    let mut c = api.redis.conn.clone();
    let marker: Option<String> = redis::cmd("GET")
        .arg(api.redis.keys.revoked_device(device_id))
        .query_async(&mut c)
        .await
        .unwrap();
    assert_eq!(marker.as_deref(), Some("stolen laptop"));
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action = 'device.revoked' AND actor_user_id = $1",
    )
    .bind(admin_user)
    .fetch_one(&api.state.db)
    .await
    .unwrap();
    assert_eq!(audited, 1);
    api.stop().await;
}

#[tokio::test]
async fn revocation_requires_permission() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let (org, _, _) = org_member(&api.state.db, OrgRole::Admin).await;
    let (_, tech_email) =
        aionix_control_plane::testing::member_of(&api.state.db, org, OrgRole::Technician).await;
    let (_, token, _) = admin::insert_enrollment_token(&api.state.db, org, None, 1, 1, None)
        .await
        .unwrap();
    let key = IdentityKey::generate();
    let (_, reg) = http
        .register(&key, DeviceKind::Agent, Some(&token), None)
        .await;
    let device_id = reg["device_id"].as_str().unwrap();
    let access = http.access(&tech_email).await;
    let (s, _) = http
        .post(
            &format!("/v1/devices/{device_id}/revoke"),
            json!({}),
            Some(&access),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(http.device_login(&key).await.0, StatusCode::OK);
    api.stop().await;
}
