//! Organization inventory (customers, groups, device organization) and unattended access
//! (ADR-0015) against the real API, PostgreSQL and Redis.

mod common;

use std::{sync::Arc, time::Duration};

use aionix_control_api::{
    claims::SessionTicketClaims,
    rest::{OrgRole, PresenceState},
    signal::{EndpointAddrDto, ServerMessage},
};
use aionix_control_plane::{
    admin,
    bus::{DeviceBusMessage, OfferAnswer, RedisBus, SignalBus},
    presence::PresenceRecord,
    testing::{TestApi, member_of, org_member},
};
use aionix_crypto::IdentityKey;
use aionix_shared::{AccessMode, DeviceKind};
use common::{Http, code};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use uuid::Uuid;

const DEVICE_PASSWORD: &str = "senha-do-cliente-2026";

struct Host {
    id: Uuid,
    friendly: String,
}

async fn enroll_host(api: &TestApi, http: &Http, org: Uuid) -> Host {
    let (_, token, _) = admin::insert_enrollment_token(&api.state.db, org, None, 1, 1, None)
        .await
        .unwrap();
    let key = IdentityKey::generate();
    let (s, v) = http
        .register(&key, DeviceKind::Agent, Some(&token), None)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    Host {
        id: v["device_id"].as_str().unwrap().parse().unwrap(),
        friendly: v["friendly_id"].as_str().unwrap().to_owned(),
    }
}

/// A technician workstation registered to `email`; returns `(device token, key)`.
async fn workstation(http: &Http, access: &str) -> (String, IdentityKey) {
    let key = IdentityKey::generate();
    let (s, v) = http
        .register(&key, DeviceKind::Technician, None, Some(access))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = http.device_login(&key).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    (v["device_token"].as_str().unwrap().to_owned(), key)
}

/// Marks the host online, with or without the owner's live unattended-access consent.
async fn set_online(api: &TestApi, device: Uuid, unattended_enabled: bool) {
    let rec = PresenceRecord {
        state: PresenceState::Online,
        busy: false,
        unattended_enabled,
        addr: EndpointAddrDto {
            relay_urls: vec!["https://relay.aionix.test/".into()],
            direct_addrs: vec!["192.0.2.10:4433".into()],
        },
        ..PresenceRecord::connecting(Uuid::new_v4(), "test")
    };
    api.state
        .presence
        .claim(device, &rec, Duration::from_secs(60))
        .await
        .unwrap();
}

/// Plays the host's signaling side: accepts every offer for `device`.
async fn fake_host(
    api: &TestApi,
    device: Uuid,
) -> (JoinHandle<()>, Arc<std::sync::Mutex<Vec<ServerMessage>>>) {
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
    assert!(bus.device_listener_active());
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let task = tokio::spawn(async move {
        while let Some((dev, msg)) = rx.recv().await {
            if dev != device {
                continue;
            }
            if let DeviceBusMessage::Deliver { message } = &msg {
                if let ServerMessage::SessionOffer { session_id, .. } = message {
                    let answer = OfferAnswer {
                        accept: true,
                        reason: None,
                        addr: EndpointAddrDto {
                            relay_urls: vec!["https://relay.aionix.test/".into()],
                            direct_addrs: vec!["198.51.100.20:5000".into()],
                        },
                    };
                    bus.publish_offer_answer(*session_id, &answer)
                        .await
                        .unwrap();
                }
                seen2.lock().unwrap().push(message.clone());
            }
        }
    });
    (task, seen)
}

/// `POST /v1/sessions` with an optional unattended-access password.
async fn request(
    http: &Http,
    access: &str,
    device_token: &str,
    target: &str,
    unattended_password: Option<&str>,
) -> (StatusCode, Value) {
    let mut body = json!({"target": target});
    if let Some(p) = unattended_password {
        body["unattended_password"] = json!(p);
    }
    http.send(
        Method::POST,
        "/v1/sessions",
        Some(body),
        Some(access),
        Some(device_token),
    )
    .await
}

/// The reason the last denied session request recorded in the audit log.
async fn last_denial(api: &TestApi) -> String {
    sqlx::query_scalar(
        "SELECT details->>'reason' FROM audit_events
         WHERE action = 'session.requested' AND outcome = 'denied'
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&api.state.db)
    .await
    .unwrap()
}

#[tokio::test]
async fn customers_and_groups_are_unique_per_organization_and_organize_devices() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let (org, _, email) = org_member(&api.state.db, OrgRole::Admin).await;
    let admin = http.access(&email).await;
    let base = format!("/v1/orgs/{org}");
    let host = enroll_host(&api, &http, org).await;

    let (s, v) = http
        .post(
            &format!("{base}/customers"),
            json!({"name": "Clínica Sul", "notes": "Ramal 20"}),
            Some(&admin),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let customer = v["id"].as_str().unwrap().to_owned();

    // Names are unique per organization, ignoring case.
    let (s, v) = http
        .post(
            &format!("{base}/customers"),
            json!({"name": "clínica SUL"}),
            Some(&admin),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(code(&v), "conflict");

    let (s, v) = http
        .post(
            &format!("{base}/groups"),
            json!({"name": "Servidores"}),
            Some(&admin),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let group = v["id"].as_str().unwrap().to_owned();
    let (s, _) = http
        .post(
            &format!("{base}/groups"),
            json!({"name": "servidores"}),
            Some(&admin),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Organize the device: customer, group, description, favorite, display name.
    let (s, v) = http
        .send(
            Method::PATCH,
            &format!("{base}/devices/{}", host.id),
            Some(json!({
                "display_name": "Servidor 01",
                "description": "Arquivos e backup",
                "customer_id": customer,
                "group_id": group,
                "favorite": true,
            })),
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["display_name"], "Servidor 01");
    assert_eq!(v["description"], "Arquivos e backup");
    assert_eq!(v["customer_name"], "Clínica Sul");
    assert_eq!(v["group_name"], "Servidores");
    assert_eq!(v["favorite"], true);
    assert_eq!(v["unattended_password_set"], false);

    let (s, v) = http.get(&format!("{base}/customers"), Some(&admin)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["customers"][0]["device_count"], 1);
    assert_eq!(v["customers"][0]["notes"], "Ramal 20");
    let (s, v) = http.get(&format!("{base}/groups"), Some(&admin)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["groups"][0]["device_count"], 1);

    // A rename is visible in the device list too (joined, not copied).
    let (s, _) = http
        .send(
            Method::PATCH,
            &format!("{base}/customers/{customer}"),
            Some(json!({"name": "Clínica Sul Ltda"})),
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, v) = http.get(&format!("{base}/devices"), Some(&admin)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["devices"][0]["customer_name"], "Clínica Sul Ltda");

    // Deleting a customer or a group keeps the device in the organization, unlinked.
    let (s, _) = http
        .send(
            Method::DELETE,
            &format!("{base}/customers/{customer}"),
            None,
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = http
        .send(
            Method::DELETE,
            &format!("{base}/groups/{group}"),
            None,
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, v) = http.get(&format!("{base}/devices"), Some(&admin)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["devices"].as_array().unwrap().len(), 1);
    assert!(v["devices"][0]["customer_id"].is_null());
    assert!(v["devices"][0]["group_id"].is_null());
    assert_eq!(v["devices"][0]["description"], "Arquivos e backup");

    api.stop().await;
}

#[tokio::test]
async fn device_links_stay_inside_the_organization_and_respect_permissions() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let (org, _, admin_email) = org_member(&api.state.db, OrgRole::Admin).await;
    let (other_org, _, other_email) = org_member(&api.state.db, OrgRole::Admin).await;
    let (tech_user, tech_email) = member_of(&api.state.db, org, OrgRole::Technician).await;
    let admin = http.access(&admin_email).await;
    let other_admin = http.access(&other_email).await;
    let tech = http.access(&tech_email).await;
    let base = format!("/v1/orgs/{org}");
    let host = enroll_host(&api, &http, org).await;

    // A customer of another organization cannot be linked.
    let (s, foreign) = http
        .post(
            &format!("/v1/orgs/{other_org}/customers"),
            json!({"name": "Outra empresa"}),
            Some(&other_admin),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{foreign}");
    let (s, v) = http
        .send(
            Method::PATCH,
            &format!("{base}/devices/{}", host.id),
            Some(json!({"customer_id": foreign["id"]})),
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");

    // A user who is not a member cannot be made responsible for the device.
    let (s, v) = http
        .send(
            Method::PATCH,
            &format!("{base}/devices/{}", host.id),
            Some(json!({"assigned_user_id": Uuid::new_v4()})),
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");

    // A technician may assign by membership, but cannot organize devices or customers.
    let (s, v) = http
        .send(
            Method::PATCH,
            &format!("{base}/devices/{}", host.id),
            Some(json!({"description": "minha"})),
            Some(&tech),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    let (s, _) = http
        .post(
            &format!("{base}/customers"),
            json!({"name": "X"}),
            Some(&tech),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = http
        .send(
            Method::PUT,
            &format!("{base}/devices/{}/unattended-password", host.id),
            Some(json!({"password": DEVICE_PASSWORD})),
            Some(&tech),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // The administrator assigns the technician, and the name is shown.
    let (s, v) = http
        .send(
            Method::PATCH,
            &format!("{base}/devices/{}", host.id),
            Some(json!({"assigned_user_id": tech_user})),
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["assigned_user_id"], tech_user.to_string());
    assert_eq!(v["assigned_user_name"], "Técnica Ana");

    // Another organization's administrator is refused on this organization's path...
    let (s, _) = http
        .send(
            Method::PATCH,
            &format!("{base}/devices/{}", host.id),
            Some(json!({"favorite": true})),
            Some(&other_admin),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // ...and on its own path the device does not exist, so it is not found.
    let (s, _) = http
        .send(
            Method::PATCH,
            &format!("/v1/orgs/{other_org}/devices/{}", host.id),
            Some(json!({"favorite": true})),
            Some(&other_admin),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    api.stop().await;
}

#[tokio::test]
async fn unattended_access_needs_the_owner_consent_and_the_device_password() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let (org, _, admin_email) = org_member(&api.state.db, OrgRole::Admin).await;
    let (_, tech_email) = member_of(&api.state.db, org, OrgRole::Technician).await;
    let admin = http.access(&admin_email).await;
    let tech = http.access(&tech_email).await;
    let (device_token, _) = workstation(&http, &tech).await;
    let host = enroll_host(&api, &http, org).await;
    let base = format!("/v1/orgs/{org}");
    let (task, seen) = fake_host(&api, host.id).await;

    // The password follows the device policy (12+ characters).
    let set = |password: &'static str| {
        let http = &http;
        let admin = admin.clone();
        let path = format!("{base}/devices/{}/unattended-password", host.id);
        async move {
            http.send(
                Method::PUT,
                &path,
                Some(json!({"password": password})),
                Some(&admin),
                None,
            )
            .await
        }
    };
    let (s, v) = set("curta").await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (s, v) = set(DEVICE_PASSWORD).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{v}");
    let (s, v) = http.get(&format!("{base}/devices"), Some(&admin)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["devices"][0]["unattended_password_set"], true);
    // The password itself is never returned.
    assert!(!v.to_string().contains(DEVICE_PASSWORD));

    // The owner has not enabled it on the computer: refused even with the right password.
    set_online(&api, host.id, false).await;
    let (s, v) = request(
        &http,
        &tech,
        &device_token,
        &host.friendly,
        Some(DEVICE_PASSWORD),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(last_denial(&api).await, "unattended_not_enabled");

    // Enabled by the owner, but a wrong password is refused.
    set_online(&api, host.id, true).await;
    let (s, v) = request(
        &http,
        &tech,
        &device_token,
        &host.friendly,
        Some("senha-errada-123"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(last_denial(&api).await, "wrong_unattended_password");

    // Enabled and the right password: an unattended session, offered as such.
    let (s, v) = request(
        &http,
        &tech,
        &device_token,
        &host.friendly,
        Some(DEVICE_PASSWORD),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["mode"], "unattended");
    let sid: Uuid = v["session_id"].as_str().unwrap().parse().unwrap();
    let claims = api
        .state
        .tokens
        .verify::<SessionTicketClaims>(v["ticket"].as_str().unwrap())
        .unwrap()
        .claims;
    assert_eq!(claims.mode, AccessMode::Unattended);
    assert_eq!(claims.sid, sid);
    let stored: String = sqlx::query_scalar("SELECT mode FROM sessions WHERE id = $1")
        .bind(sid)
        .fetch_one(&api.state.db)
        .await
        .unwrap();
    assert_eq!(stored, "unattended");
    let offers = seen.lock().unwrap().clone();
    assert!(matches!(
        offers.last(),
        Some(ServerMessage::SessionOffer {
            mode: AccessMode::Unattended,
            ..
        })
    ));

    // An ordinary request with the same technician still asks the person (assisted).
    let (s, v) = request(&http, &tech, &device_token, &host.friendly, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["mode"], "assisted");

    // Clearing the password stops unattended access at once.
    let (s, _) = http
        .send(
            Method::DELETE,
            &format!("{base}/devices/{}/unattended-password", host.id),
            None,
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, v) = request(
        &http,
        &tech,
        &device_token,
        &host.friendly,
        Some(DEVICE_PASSWORD),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(last_denial(&api).await, "unattended_password_not_set");

    task.abort();
    api.stop().await;
}
