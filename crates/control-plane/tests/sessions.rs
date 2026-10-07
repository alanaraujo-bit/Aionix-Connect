//! Session authorization and ticket issuance against the real API. The signaling side
//! is played by a bus subscriber that answers offers like a host would.

mod common;

use std::{sync::Arc, time::Duration};

use aionix_control_api::{
    claims::{SessionPermission, SessionTicketClaims},
    rest::{OrgRole, PresenceState, SessionEventKind},
    signal::{EndpointAddrDto, OfferRejectReason, ServerMessage},
};
use aionix_control_plane::{
    admin,
    bus::{DeviceBusMessage, OfferAnswer, RedisBus, SignalBus},
    presence::PresenceRecord,
    sessions::{self, EventOutcome, Reporter},
    testing::{TestApi, member_of, org_member},
};
use aionix_crypto::IdentityKey;
use aionix_shared::{DeviceKind, TransportKind};
use common::{Http, code};
use reqwest::StatusCode;
use serde_json::json;
use tokio::task::JoinHandle;
use uuid::Uuid;

struct Host {
    id: Uuid,
    friendly: String,
    key: IdentityKey,
}

struct Workstation {
    access: String,
    device_token: String,
    key: IdentityKey,
    user: Uuid,
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
        key,
    }
}

async fn workstation(http: &Http, user: Uuid, email: &str) -> Workstation {
    let access = http.access(email).await;
    let key = IdentityKey::generate();
    let (s, v) = http
        .register(&key, DeviceKind::Technician, None, Some(&access))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = http.device_login(&key).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    Workstation {
        access,
        device_token: v["device_token"].as_str().unwrap().to_owned(),
        key,
        user,
    }
}

async fn set_online(api: &TestApi, device: Uuid, busy: bool) {
    let rec = PresenceRecord {
        state: PresenceState::Online,
        busy,
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

/// Plays the host side of signaling: answers offers for `device` with `answer`
/// (`None` = never answers). Returns the offers it saw.
async fn fake_host(
    api: &TestApi,
    device: Uuid,
    answer: Option<OfferAnswer>,
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
            if let DeviceBusMessage::Deliver { message } = msg {
                if let ServerMessage::SessionOffer { session_id, .. } = &message
                    && let Some(a) = &answer
                {
                    bus.publish_offer_answer(*session_id, a).await.unwrap();
                }
                seen2.lock().unwrap().push(message);
            }
        }
    });
    (task, seen)
}

fn accept() -> OfferAnswer {
    OfferAnswer {
        accept: true,
        reason: None,
        addr: EndpointAddrDto {
            relay_urls: vec!["https://relay.aionix.test/".into()],
            direct_addrs: vec!["198.51.100.20:5000".into()],
        },
    }
}

#[tokio::test]
async fn technician_gets_a_bound_short_lived_ticket() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let (org, user, email) = org_member(&api.state.db, OrgRole::Technician).await;
    let host = enroll_host(&api, &http, org).await;
    let ws = workstation(&http, user, &email).await;
    set_online(&api, host.id, false).await;
    let (task, seen) = fake_host(&api, host.id, Some(accept())).await;

    let (s, v) = http
        .request_session(&ws.access, &ws.device_token, &host.friendly, None)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let sid: Uuid = v["session_id"].as_str().unwrap().parse().unwrap();
    assert_eq!(v["host"]["device_id"], host.id.to_string());
    assert_eq!(v["host"]["key"], host.key.public().to_base64());
    assert_eq!(v["host"]["addr"]["direct_addrs"][0], "198.51.100.20:5000");

    // The ticket binds session, both keys, user, mode and permissions.
    let t = api
        .state
        .tokens
        .verify::<SessionTicketClaims>(v["ticket"].as_str().unwrap())
        .unwrap();
    assert_eq!(t.claims.sid, sid);
    assert_eq!(t.claims.host.device_id, host.id);
    assert_eq!(t.claims.host.key, host.key.public());
    assert_eq!(t.claims.viewer.key, ws.key.public());
    assert_eq!(t.claims.viewer.user_id, ws.user);
    assert_eq!(t.claims.viewer.org_name.as_deref(), Some("Suporte Exemplo"));
    assert_eq!(
        t.claims.perms,
        [
            SessionPermission::View,
            SessionPermission::Control,
            SessionPermission::Clipboard
        ]
    );
    assert!(t.expires_at - t.issued_at <= SessionTicketClaims::TTL);

    // The host received the offer with the same ticket.
    let offers = seen.lock().unwrap().clone();
    assert!(
        matches!(&offers[..], [ServerMessage::SessionOffer { session_id, .. }] if *session_id == sid)
    );

    let status: String = sqlx::query_scalar("SELECT status FROM sessions WHERE id = $1")
        .bind(sid)
        .fetch_one(&api.state.db)
        .await
        .unwrap();
    assert_eq!(status, "accepted");
    // Ticket itself is never stored.
    let stored: String = sqlx::query_scalar("SELECT ticket_jti_hash FROM sessions WHERE id = $1")
        .bind(sid)
        .fetch_one(&api.state.db)
        .await
        .unwrap();
    assert_ne!(stored, t.jti);

    // Host reports the user's approval, then the technician the connection.
    let r = sessions::record_event(
        &api.state.db,
        sid,
        Reporter::Host { device: host.id },
        SessionEventKind::AuthGranted,
        None,
        "",
        (None, None),
    )
    .await
    .unwrap();
    assert_eq!(r, EventOutcome::Recorded);
    let (s, _) = http
        .post(
            &format!("/v1/sessions/{sid}/events"),
            json!({"kind": "connected", "transport": "relay", "detail": ""}),
            Some(&ws.access),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    // A stranger cannot report on this session.
    let other = sessions::record_event(
        &api.state.db,
        sid,
        Reporter::Host {
            device: Uuid::new_v4(),
        },
        SessionEventKind::Ended,
        Some(TransportKind::Relay),
        "",
        (None, None),
    )
    .await
    .unwrap();
    assert_eq!(other, EventOutcome::NotAllowed);
    let (status, transport): (String, Option<String>) =
        sqlx::query_as("SELECT status, transport FROM sessions WHERE id = $1")
            .bind(sid)
            .fetch_one(&api.state.db)
            .await
            .unwrap();
    assert_eq!(
        (status.as_str(), transport.as_deref()),
        ("connected", Some("relay"))
    );
    assert_eq!(sessions::active_count(&api.state.db).await.unwrap(), 1);

    let (s, list) = http.get("/v1/sessions", Some(&ws.access)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["sessions"][0]["status"], "connected");

    let actions: Vec<String> =
        sqlx::query_scalar("SELECT action FROM audit_events WHERE session_id = $1 ORDER BY id")
            .bind(sid)
            .fetch_all(&api.state.db)
            .await
            .unwrap();
    for a in [
        "session.requested",
        "session.ticket_issued",
        "session.offer_answered",
        "session.approved",
    ] {
        assert!(actions.iter().any(|x| x == a), "missing {a}: {actions:?}");
    }
    // Requested permissions are intersected with the role.
    let (s, v) = http
        .request_session(
            &ws.access,
            &ws.device_token,
            &host.friendly,
            Some(json!(["view"])),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["permissions"], json!(["view"]));
    let (s, _) = http
        .request_session(
            &ws.access,
            &ws.device_token,
            &host.friendly,
            Some(json!(["control"])),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    task.abort();
    api.stop().await;
}

#[tokio::test]
async fn permission_denials_and_wrong_devices() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let (org, _, _) = org_member(&api.state.db, OrgRole::Owner).await;
    let host = enroll_host(&api, &http, org).await;
    set_online(&api, host.id, false).await;
    let (task, _) = fake_host(&api, host.id, Some(accept())).await;

    // Viewer of the device's organization: no session.request.
    let (viewer, viewer_email) = member_of(&api.state.db, org, OrgRole::Viewer).await;
    let vws = workstation(&http, viewer, &viewer_email).await;
    let (s, _) = http
        .request_session(&vws.access, &vws.device_token, &host.friendly, None)
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Technician of another organization: wrong device.
    let (_, outsider, outsider_email) = org_member(&api.state.db, OrgRole::Technician).await;
    let ows = workstation(&http, outsider, &outsider_email).await;
    let (s, _) = http
        .request_session(&ows.access, &ows.device_token, &host.friendly, None)
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let denied: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action = 'permission.denied' AND target_device_id = $1",
    )
    .bind(host.id)
    .fetch_one(&api.state.db)
    .await
    .unwrap();
    assert_eq!(denied, 2);

    // Legit technician, but with somebody else's workstation token.
    let (tech, tech_email) = member_of(&api.state.db, org, OrgRole::Technician).await;
    let tws = workstation(&http, tech, &tech_email).await;
    let (s, _) = http
        .request_session(&tws.access, &ows.device_token, &host.friendly, None)
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Missing workstation token.
    let (s, _) = http
        .send(
            reqwest::Method::POST,
            "/v1/sessions",
            Some(json!({"target": host.friendly})),
            Some(&tws.access),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // A technician workstation is not a host.
    let ws_friendly: i32 =
        sqlx::query_scalar("SELECT friendly_id FROM devices WHERE public_key = $1")
            .bind(tws.key.public().to_base64())
            .fetch_one(&api.state.db)
            .await
            .unwrap();
    let (s, _) = http
        .request_session(
            &tws.access,
            &tws.device_token,
            &ws_friendly.to_string(),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // Unknown friendly id.
    let (s, _) = http
        .request_session(&tws.access, &tws.device_token, "111111111", None)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // The legit request works.
    let (s, v) = http
        .request_session(&tws.access, &tws.device_token, &host.friendly, None)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // Busy, offline and revoked hosts.
    set_online(&api, host.id, true).await;
    let (s, v) = http
        .request_session(&tws.access, &tws.device_token, &host.friendly, None)
        .await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "device_busy"));
    let offline = enroll_host(&api, &http, org).await;
    let (s, v) = http
        .request_session(&tws.access, &tws.device_token, &offline.friendly, None)
        .await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "device_offline"));
    set_online(&api, host.id, false).await;
    aionix_control_plane::devices::revoke(
        &api.state.db,
        api.state.bus.as_ref(),
        None,
        host.id,
        None,
        "test",
        (None, None),
    )
    .await
    .unwrap();
    let (s, v) = http
        .request_session(&tws.access, &tws.device_token, &host.friendly, None)
        .await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "device_revoked"));

    // A revoked workstation cannot request either.
    let ws_id: Uuid = sqlx::query_scalar("SELECT id FROM devices WHERE public_key = $1")
        .bind(tws.key.public().to_base64())
        .fetch_one(&api.state.db)
        .await
        .unwrap();
    aionix_control_plane::devices::revoke(
        &api.state.db,
        api.state.bus.as_ref(),
        None,
        ws_id,
        None,
        "test",
        (None, None),
    )
    .await
    .unwrap();
    let other_host = enroll_host(&api, &http, org).await;
    set_online(&api, other_host.id, false).await;
    let (s, _) = http
        .request_session(&tws.access, &tws.device_token, &other_host.friendly, None)
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    task.abort();
    api.stop().await;
}

#[tokio::test]
async fn host_refusal_and_offer_timeout() {
    let Some(api) = TestApi::start_with(aionix_control_plane::config::ApiSettings {
        offer_timeout: Duration::from_millis(800),
        ..Default::default()
    })
    .await
    else {
        return;
    };
    let http = Http::new(&api.base);
    let (org, user, email) = org_member(&api.state.db, OrgRole::Technician).await;
    let ws = workstation(&http, user, &email).await;

    let busy_host = enroll_host(&api, &http, org).await;
    set_online(&api, busy_host.id, false).await;
    let (t1, _) = fake_host(
        &api,
        busy_host.id,
        Some(OfferAnswer {
            accept: false,
            reason: Some(OfferRejectReason::Busy),
            addr: EndpointAddrDto::default(),
        }),
    )
    .await;
    let (s, v) = http
        .request_session(&ws.access, &ws.device_token, &busy_host.friendly, None)
        .await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "device_busy"));

    let silent = enroll_host(&api, &http, org).await;
    set_online(&api, silent.id, false).await;
    let (t2, seen) = fake_host(&api, silent.id, None).await;
    let (s, v) = http
        .request_session(&ws.access, &ws.device_token, &silent.friendly, None)
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::GATEWAY_TIMEOUT, "device_timeout")
    );
    assert_eq!(seen.lock().unwrap().len(), 1, "offer was delivered");
    let statuses: Vec<String> =
        sqlx::query_scalar("SELECT status FROM sessions ORDER BY requested_at")
            .fetch_all(&api.state.db)
            .await
            .unwrap();
    assert_eq!(statuses, ["rejected", "failed"]);
    t1.abort();
    t2.abort();
    api.stop().await;
}

#[tokio::test]
async fn janitor_expires_unanswered_and_stale_sessions() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let http = Http::new(&api.base);
    let (org, user, email) = org_member(&api.state.db, OrgRole::Technician).await;
    let ws = workstation(&http, user, &email).await;
    let host = enroll_host(&api, &http, org).await;
    set_online(&api, host.id, false).await;
    let (task, _) = fake_host(&api, host.id, Some(accept())).await;
    let (_, a) = http
        .request_session(&ws.access, &ws.device_token, &host.friendly, None)
        .await;
    let (_, b) = http
        .request_session(&ws.access, &ws.device_token, &host.friendly, None)
        .await;
    let a: Uuid = a["session_id"].as_str().unwrap().parse().unwrap();
    let b: Uuid = b["session_id"].as_str().unwrap().parse().unwrap();
    // `a` never got approved and its ticket is long expired; `b` was connected but the
    // host disappeared.
    sqlx::query("UPDATE sessions SET ticket_expires_at = now() - interval '1 hour' WHERE id = $1")
        .bind(a)
        .execute(&api.state.db)
        .await
        .unwrap();
    sqlx::query("UPDATE sessions SET status = 'connected', connected_at = now() - interval '10 minutes' WHERE id = $1")
        .bind(b)
        .execute(&api.state.db)
        .await
        .unwrap();
    // Host still online: nothing stale yet.
    aionix_control_plane::janitor::run_once(&api.state.db, api.state.presence.as_ref())
        .await
        .unwrap();
    let sb: String = sqlx::query_scalar("SELECT status FROM sessions WHERE id = $1")
        .bind(b)
        .fetch_one(&api.state.db)
        .await
        .unwrap();
    assert_eq!(sb, "connected");
    // Host goes offline.
    let rec = api.state.presence.get(host.id).await.unwrap().unwrap();
    api.state
        .presence
        .release(host.id, rec.conn_id, None)
        .await
        .unwrap();
    aionix_control_plane::janitor::run_once(&api.state.db, api.state.presence.as_ref())
        .await
        .unwrap();
    let rows: Vec<(Uuid, String, Option<String>)> =
        sqlx::query_as("SELECT id, status, end_reason FROM sessions WHERE id = ANY($1)")
            .bind(vec![a, b])
            .fetch_all(&api.state.db)
            .await
            .unwrap();
    for (id, status, reason) in rows {
        if id == a {
            assert_eq!(
                (status.as_str(), reason.as_deref()),
                ("expired", Some("ticket_expired"))
            );
        } else {
            assert_eq!(
                (status.as_str(), reason.as_deref()),
                ("failed", Some("host_offline"))
            );
        }
    }
    task.abort();
    api.stop().await;
}
