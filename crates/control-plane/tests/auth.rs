//! Authentication against the real API (PostgreSQL + Redis).

mod common;

use aionix_control_api::rest::OrgRole;
use aionix_control_plane::testing::{TEST_PASSWORD, TestApi, org_member};
use common::{Http, code};
use reqwest::StatusCode;
use serde_json::json;

#[tokio::test]
async fn login_me_refresh_rotation_and_logout() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let (_, user, email) = org_member(&api.state.db, OrgRole::Technician).await;

    // Wrong password: generic error, audited.
    let (s, v) = http.login(&email, "wrong password!!").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&v), "invalid_credentials");
    // Unknown account: same answer.
    let (s, v) = http.login("nobody@example.test", "whatever password").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&v), "invalid_credentials");

    let (s, tokens) = http.login(&email.to_uppercase(), TEST_PASSWORD).await;
    assert_eq!(s, StatusCode::OK, "{tokens}");
    assert_eq!(tokens["expires_in"], 900);
    assert_eq!(tokens["user"]["memberships"][0]["role"], "technician");
    let access = tokens["access_token"].as_str().unwrap().to_owned();
    let refresh1 = tokens["refresh_token"].as_str().unwrap().to_owned();

    let (s, me) = http.get("/v1/me", Some(&access)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(me["id"], user.to_string());

    // No token / garbage token.
    assert_eq!(http.get("/v1/me", None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(http.get("/v1/me", Some("axt1.x.y.z")).await.0, StatusCode::UNAUTHORIZED);

    // Rotation.
    let (s, rotated) = http
        .post("/v1/auth/refresh", json!({"refresh_token": refresh1}), None)
        .await;
    assert_eq!(s, StatusCode::OK, "{rotated}");
    let refresh2 = rotated["refresh_token"].as_str().unwrap().to_owned();
    let access2 = rotated["access_token"].as_str().unwrap().to_owned();
    assert_ne!(refresh1, refresh2);

    // Reusing the rotated token = theft: the whole login is revoked.
    let (s, _) = http
        .post("/v1/auth/refresh", json!({"refresh_token": refresh1}), None)
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(http.get("/v1/me", Some(&access2)).await.0, StatusCode::UNAUTHORIZED);
    let (s, _) = http
        .post("/v1/auth/refresh", json!({"refresh_token": refresh2}), None)
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let reuse: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action = 'auth.refresh_reuse' AND category = 'security'",
    )
    .fetch_one(&api.state.db)
    .await
    .unwrap();
    assert_eq!(reuse, 1);

    // Fresh login, then logout revokes it.
    let (_, t) = http.login(&email, TEST_PASSWORD).await;
    let access3 = t["access_token"].as_str().unwrap().to_owned();
    let (s, _) = http
        .post("/v1/auth/logout", json!({"refresh_token": t["refresh_token"]}), None)
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(http.get("/v1/me", Some(&access3)).await.0, StatusCode::UNAUTHORIZED);

    let audited: Vec<(String, String)> = sqlx::query_as(
        "SELECT action, outcome FROM audit_events WHERE action LIKE 'auth.%' ORDER BY id",
    )
    .fetch_all(&api.state.db)
    .await
    .unwrap();
    assert!(audited.contains(&("auth.login".into(), "failure".into())));
    assert!(audited.contains(&("auth.login".into(), "success".into())));
    assert!(audited.contains(&("auth.logout".into(), "success".into())));
    // Passwords never reach the audit table.
    let leaked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE details::text LIKE '%horse%' OR details::text LIKE '%wrong password%'",
    )
    .fetch_one(&api.state.db)
    .await
    .unwrap();
    assert_eq!(leaked, 0);
    api.stop().await;
}

#[tokio::test]
async fn expired_revoked_and_disabled_account_sessions_are_refused() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let (_, user, email) = org_member(&api.state.db, OrgRole::Technician).await;
    let (_, t) = http.login(&email, TEST_PASSWORD).await;
    let access = t["access_token"].as_str().unwrap().to_owned();
    let refresh = t["refresh_token"].as_str().unwrap().to_owned();

    // Login session past its absolute lifetime: access refused as expired.
    sqlx::query("UPDATE auth_sessions SET absolute_expires_at = now() - interval '1 minute', expires_at = now() - interval '1 minute' WHERE user_id = $1")
        .bind(user)
        .execute(&api.state.db)
        .await
        .unwrap();
    let (s, v) = http.get("/v1/me", Some(&access)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&v), "token_expired");
    let (s, v) = http
        .post("/v1/auth/refresh", json!({"refresh_token": refresh}), None)
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&v), "token_expired");

    // logout-all revokes every login.
    let (_, a) = http.login(&email, TEST_PASSWORD).await;
    let (_, b) = http.login(&email, TEST_PASSWORD).await;
    let a = a["access_token"].as_str().unwrap().to_owned();
    let b = b["access_token"].as_str().unwrap().to_owned();
    assert_eq!(http.post("/v1/auth/logout-all", json!({}), Some(&a)).await.0, StatusCode::NO_CONTENT);
    assert_eq!(http.get("/v1/me", Some(&b)).await.0, StatusCode::UNAUTHORIZED);

    // Disabled account: existing logins die, new logins fail.
    let (_, c) = http.login(&email, TEST_PASSWORD).await;
    let c = c["access_token"].as_str().unwrap().to_owned();
    aionix_control_plane::admin::disable_user(&api.state.db, user).await.unwrap();
    assert_eq!(http.get("/v1/me", Some(&c)).await.0, StatusCode::UNAUTHORIZED);
    let (s, v) = http.login(&email, TEST_PASSWORD).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&v), "invalid_credentials");
    api.stop().await;
}

#[tokio::test]
async fn repeated_failures_lock_the_account() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let (_, user, email) = org_member(&api.state.db, OrgRole::Viewer).await;
    for _ in 0..10 {
        let (s, _) = http.login(&email, "not the password").await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    let locked: bool = sqlx::query_scalar("SELECT locked_until > now() FROM users WHERE id = $1")
        .bind(user)
        .fetch_one(&api.state.db)
        .await
        .unwrap();
    assert!(locked);
    // Even the right password is refused now (locked, or rate limited first).
    let (s, v) = http.login(&email, TEST_PASSWORD).await;
    assert!(
        s == StatusCode::LOCKED || s == StatusCode::TOO_MANY_REQUESTS,
        "{s} {v}"
    );
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action = 'auth.account_locked' AND target_user_id = $1",
    )
    .bind(user)
    .fetch_one(&api.state.db)
    .await
    .unwrap();
    assert_eq!(n, 1);
    api.stop().await;
}

#[tokio::test]
async fn health_endpoints_distinguish_alive_from_ready() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let (s, v) = http.get("/healthz", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["alive"], true);
    let (s, v) = http.get("/readyz", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["ready"], true);
    let names: Vec<&str> = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["postgres", "redis", "bus"]);
    // Server info publishes the token keys devices pin.
    let (s, info) = http.get("/v1/server/info", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(info["token_keys"].as_array().unwrap().len(), 1);

    // Database gone → still alive, no longer ready.
    api.state.db.close().await;
    let (s, _) = http.get("/healthz", None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = http.get("/readyz", None).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(v["checks"][0]["ok"], false);
    api.stop().await;
}
