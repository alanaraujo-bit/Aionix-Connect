//! Signaling service against real PostgreSQL + Redis, with real WebSocket clients.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use aionix_control_api::{
    rest::{OrgRole, PresenceState},
    signal::{ClientMessage, EndpointAddrDto, ServerMessage, close_code},
};
use aionix_control_plane::{
    admin,
    presence::state_of,
    testing::{TEST_PASSWORD, TestApi, org_member},
};
use aionix_crypto::{IdentityKey, SignatureDomain, sign_domain};
use aionix_server_common::{http::TrustedProxies, keys::verifier_for};
use aionix_signaling::{Signaling, Timing};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::HeaderValue},
};
use uuid::Uuid;

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

fn timing() -> Timing {
    Timing {
        ping_interval: Duration::from_millis(500),
        heartbeat_timeout: Duration::from_secs(2),
        reconnect_grace: Duration::from_secs(1),
    }
}

struct SignalServer {
    sig: Arc<Signaling>,
    addr: SocketAddr,
    task: JoinHandle<()>,
}

async fn start_signaling(api: &TestApi) -> SignalServer {
    let (sig, health) = aionix_signaling::from_parts(
        api.db.pool.clone(),
        api.redis.client.clone(),
        api.redis.conn.clone(),
        api.redis.keys.clone(),
        verifier_for(api.state.tokens.public_keys()),
        timing(),
        TrustedProxies::default(),
    )
    .await
    .unwrap();
    for _ in 0..50 {
        if health.readiness().await.ready {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(health.readiness().await.ready, "signaling ready");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = aionix_signaling::router(sig.clone(), health)
        .into_make_service_with_connect_info::<SocketAddr>();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    SignalServer { sig, addr, task }
}

async fn post(api: &TestApi, path: &str, body: Value, bearer: Option<&str>) -> (u16, Value) {
    let mut req = reqwest::Client::new().post(api.url(path)).json(&body);
    if let Some(b) = bearer {
        req = req.bearer_auth(b);
    }
    let r = req.send().await.unwrap();
    let s = r.status().as_u16();
    (s, r.json().await.unwrap_or(Value::Null))
}

async fn nonce(api: &TestApi) -> String {
    post(api, "/v1/devices/challenge", json!({}), None).await.1["nonce"]
        .as_str()
        .unwrap()
        .to_owned()
}

struct Device {
    id: Uuid,
    friendly: String,
    key: IdentityKey,
}

async fn enroll_agent(api: &TestApi, org: Uuid) -> Device {
    let (_, token, _) = admin::insert_enrollment_token(&api.state.db, org, None, 1, 1, None)
        .await
        .unwrap();
    let key = IdentityKey::generate();
    let n = nonce(api).await;
    let (s, v) = post(
        api,
        "/v1/devices/register",
        json!({"public_key": key.public(), "kind": "agent", "nonce": n,
               "signature": sign_domain(&key, SignatureDomain::DeviceRegistration, n.as_bytes()),
               "info": {"hostname": "PC-HOST", "os_name": "Windows", "os_version": "11", "app_version": "0.1.0"},
               "enrollment_token": token}),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    Device {
        id: v["device_id"].as_str().unwrap().parse().unwrap(),
        friendly: v["friendly_id"].as_str().unwrap().to_owned(),
        key,
    }
}

async fn device_token(api: &TestApi, key: &IdentityKey) -> String {
    let n = nonce(api).await;
    let (s, v) = post(
        api,
        "/v1/devices/login",
        json!({"public_key": key.public(), "nonce": n,
               "signature": sign_domain(key, SignatureDomain::DeviceLogin, n.as_bytes())}),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    v["device_token"].as_str().unwrap().to_owned()
}

async fn connect(addr: SocketAddr, token: Option<&str>) -> Result<Ws, u16> {
    let mut req = format!("ws://{addr}/v1/signal").into_client_request().unwrap();
    if let Some(t) = token {
        req.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {t}")).unwrap(),
        );
    }
    match connect_async(req).await {
        Ok((ws, _)) => Ok(ws),
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => Err(resp.status().as_u16()),
        Err(e) => panic!("unexpected connect error: {e}"),
    }
}

async fn send(ws: &mut Ws, msg: &ClientMessage) {
    ws.send(Message::Text(serde_json::to_string(msg).unwrap().into()))
        .await
        .unwrap();
}

/// Next server message, skipping pings unless `keep_pings`. `Err(close code)` on close.
async fn recv(ws: &mut Ws, answer_pings: bool) -> Result<ServerMessage, u16> {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("server message in time");
        match m {
            Some(Ok(Message::Text(t))) => {
                let msg: ServerMessage = serde_json::from_str(t.as_str()).unwrap();
                if let ServerMessage::Ping { id } = msg {
                    if answer_pings {
                        send(ws, &ClientMessage::Pong { id }).await;
                    }
                    continue;
                }
                return Ok(msg);
            }
            Some(Ok(Message::Close(frame))) => {
                return Err(frame.map(|f| u16::from(f.code)).unwrap_or(1005));
            }
            Some(Ok(_)) => continue,
            Some(Err(_)) | None => return Err(1006),
        }
    }
}

fn presence_msg() -> ClientMessage {
    ClientMessage::Presence {
        addr: EndpointAddrDto {
            relay_urls: vec!["https://relay.aionix.test/".into()],
            direct_addrs: vec!["192.0.2.50:4433".into()],
        },
        unattended_enabled: false,
        busy: false,
    }
}

async fn presence_of(api: &TestApi, device: Uuid) -> PresenceState {
    state_of(api.state.presence.get(device).await.unwrap().as_ref())
}

async fn wait_presence(api: &TestApi, device: Uuid, want: PresenceState, within: Duration) {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let now = presence_of(api, device).await;
        if now == want {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "presence stayed {now:?}, wanted {want:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn signaling_authentication() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let srv = start_signaling(&api).await;
    let (org, _, email) = org_member(&api.state.db, OrgRole::Admin).await;
    let dev = enroll_agent(&api, org).await;

    assert_eq!(connect(srv.addr, None).await.err(), Some(401));
    assert_eq!(connect(srv.addr, Some("axt1.forged.token.x")).await.err(), Some(401));
    // An access token (other type, same issuer) is not a device token.
    let (_, login) = post(&api, "/v1/auth/login", json!({"email": email, "password": TEST_PASSWORD}), None).await;
    assert_eq!(
        connect(srv.addr, login["access_token"].as_str()).await.err(),
        Some(401)
    );
    // Valid device token.
    let token = device_token(&api, &dev.key).await;
    let mut ws = connect(srv.addr, Some(&token)).await.unwrap();
    match recv(&mut ws, true).await.unwrap() {
        ServerMessage::Ready { device_id, friendly_id, ping_interval } => {
            assert_eq!(device_id, dev.id);
            assert_eq!(friendly_id.compact(), dev.friendly);
            assert_eq!(ping_interval, 0); // 500 ms rounds down to 0 s in the announcement
        }
        other => panic!("expected Ready, got {other:?}"),
    }
    send(&mut ws, &ClientMessage::Goodbye).await;
    // Revoked device: refused at the door even with an unexpired token.
    aionix_control_plane::devices::revoke(&api.state.db, api.state.bus.as_ref(), None, dev.id, None, "test", (None, None))
        .await
        .unwrap();
    assert_eq!(connect(srv.addr, Some(&token)).await.err(), Some(403));
    srv.task.abort();
    api.stop().await;
}

#[tokio::test]
async fn presence_follows_the_link_heartbeat_timeout_and_goodbye() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let srv = start_signaling(&api).await;
    let (org, _, _) = org_member(&api.state.db, OrgRole::Admin).await;
    let dev = enroll_agent(&api, org).await;
    let token = device_token(&api, &dev.key).await;
    assert_eq!(presence_of(&api, dev.id).await, PresenceState::Offline);

    // Authenticated but no reachability yet: connecting.
    let mut ws = connect(srv.addr, Some(&token)).await.unwrap();
    assert!(matches!(recv(&mut ws, false).await, Ok(ServerMessage::Ready { .. })));
    assert_eq!(presence_of(&api, dev.id).await, PresenceState::Connecting);
    send(&mut ws, &presence_msg()).await;
    wait_presence(&api, dev.id, PresenceState::Online, Duration::from_secs(3)).await;
    let rec = api.state.presence.get(dev.id).await.unwrap().unwrap();
    assert_eq!(rec.addr.direct_addrs, ["192.0.2.50:4433"]);

    // Stop answering pings: the server closes with 4000, device goes connecting → offline.
    let code = loop {
        match recv(&mut ws, false).await {
            Err(code) => break code,
            Ok(_) => continue,
        }
    };
    assert_eq!(code, close_code::HEARTBEAT_TIMEOUT);
    assert_eq!(presence_of(&api, dev.id).await, PresenceState::Connecting);
    wait_presence(&api, dev.id, PresenceState::Offline, Duration::from_secs(3)).await;

    // Reconnect, answer pings for a while (stays online), then say goodbye.
    let mut ws = connect(srv.addr, Some(&token)).await.unwrap();
    let _ = recv(&mut ws, true).await;
    send(&mut ws, &presence_msg()).await;
    wait_presence(&api, dev.id, PresenceState::Online, Duration::from_secs(3)).await;
    let keep = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < keep {
        if let Ok(Some(Ok(Message::Text(t)))) =
            tokio::time::timeout(Duration::from_millis(200), ws.next()).await
            && let Ok(ServerMessage::Ping { id }) = serde_json::from_str(t.as_str())
        {
            send(&mut ws, &ClientMessage::Pong { id }).await;
        }
    }
    assert_eq!(presence_of(&api, dev.id).await, PresenceState::Online, "heartbeats keep it online");
    send(&mut ws, &ClientMessage::Goodbye).await;
    wait_presence(&api, dev.id, PresenceState::Offline, Duration::from_millis(800)).await;

    // Abrupt drop (no close handshake): connecting for the grace period, then offline.
    let mut ws = connect(srv.addr, Some(&token)).await.unwrap();
    let _ = recv(&mut ws, true).await;
    send(&mut ws, &presence_msg()).await;
    wait_presence(&api, dev.id, PresenceState::Online, Duration::from_secs(3)).await;
    drop(ws);
    wait_presence(&api, dev.id, PresenceState::Connecting, Duration::from_secs(3)).await;
    wait_presence(&api, dev.id, PresenceState::Offline, Duration::from_secs(3)).await;
    srv.task.abort();
    api.stop().await;
}

#[tokio::test]
async fn newer_link_supersedes_older_and_presence_survives_reconnect() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let srv = start_signaling(&api).await;
    let (org, _, _) = org_member(&api.state.db, OrgRole::Admin).await;
    let dev = enroll_agent(&api, org).await;
    let token = device_token(&api, &dev.key).await;

    let mut first = connect(srv.addr, Some(&token)).await.unwrap();
    let _ = recv(&mut first, true).await;
    send(&mut first, &presence_msg()).await;
    wait_presence(&api, dev.id, PresenceState::Online, Duration::from_secs(3)).await;

    let mut second = connect(srv.addr, Some(&token)).await.unwrap();
    let _ = recv(&mut second, true).await;
    let code = loop {
        match recv(&mut first, true).await {
            Err(c) => break c,
            Ok(_) => continue,
        }
    };
    assert_eq!(code, close_code::SUPERSEDED);
    send(&mut second, &presence_msg()).await;
    wait_presence(&api, dev.id, PresenceState::Online, Duration::from_secs(3)).await;
    // The superseded link's departure did not mark the device offline.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(presence_of(&api, dev.id).await, PresenceState::Online);
    assert_eq!(srv.sig.link_count(), 1);
    srv.task.abort();
    api.stop().await;
}

#[tokio::test]
async fn full_offer_flow_events_and_revocation_over_real_signaling() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let srv = start_signaling(&api).await;
    let (org, user, email) = org_member(&api.state.db, OrgRole::Admin).await;
    let host = enroll_agent(&api, org).await;
    let host_token = device_token(&api, &host.key).await;

    // Host online over real signaling, answering offers.
    let mut ws = connect(srv.addr, Some(&host_token)).await.unwrap();
    let _ = recv(&mut ws, true).await;
    send(&mut ws, &presence_msg()).await;
    wait_presence(&api, host.id, PresenceState::Online, Duration::from_secs(3)).await;

    // Technician workstation.
    let (_, login) = post(&api, "/v1/auth/login", json!({"email": email, "password": TEST_PASSWORD}), None).await;
    let access = login["access_token"].as_str().unwrap().to_owned();
    let ws_key = IdentityKey::generate();
    let n = nonce(&api).await;
    let (s, _) = post(
        &api,
        "/v1/devices/register",
        json!({"public_key": ws_key.public(), "kind": "technician", "nonce": n,
               "signature": sign_domain(&ws_key, SignatureDomain::DeviceRegistration, n.as_bytes()),
               "info": {"hostname": "TEC", "os_name": "Windows", "os_version": "11", "app_version": "0.1.0"}}),
        Some(&access),
    )
    .await;
    assert_eq!(s, 200);
    let ws_token = device_token(&api, &ws_key).await;

    let api_url = api.url("/v1/sessions");
    let friendly = host.friendly.clone();
    let request = tokio::spawn(async move {
        let r = reqwest::Client::new()
            .post(api_url)
            .bearer_auth(access)
            .header(aionix_control_api::DEVICE_TOKEN_HEADER, ws_token)
            .json(&json!({"target": friendly}))
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.json::<Value>().await.unwrap())
    });

    // The host receives the offer and accepts with its address.
    let (sid, ticket) = loop {
        match recv(&mut ws, true).await.unwrap() {
            ServerMessage::SessionOffer { session_id, ticket, viewer, .. } => {
                assert_eq!(viewer.key, ws_key.public());
                break (session_id, ticket);
            }
            _ => continue,
        }
    };
    send(
        &mut ws,
        &ClientMessage::SessionOfferResponse {
            session_id: sid,
            accept: true,
            reason: None,
            addr: EndpointAddrDto {
                relay_urls: vec!["https://relay.aionix.test/".into()],
                direct_addrs: vec!["203.0.113.7:6000".into()],
            },
        },
    )
    .await;
    let (status, body) = request.await.unwrap();
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["session_id"], sid.to_string());
    assert_eq!(body["ticket"], ticket);
    assert_eq!(body["host"]["addr"]["direct_addrs"][0], "203.0.113.7:6000");

    // Host reports the consent decision (metadata only).
    send(
        &mut ws,
        &ClientMessage::SessionEvent {
            session_id: sid,
            kind: aionix_control_api::rest::SessionEventKind::AuthGranted,
            transport: None,
            detail: String::new(),
        },
    )
    .await;
    let mut approved = false;
    for _ in 0..30 {
        let s: String = sqlx::query_scalar("SELECT status FROM sessions WHERE id = $1")
            .bind(sid)
            .fetch_one(&api.state.db)
            .await
            .unwrap();
        if s == "approved" {
            approved = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(approved);

    // Revocation reaches the live link: DeviceRevoked + close 4003, offline at once.
    aionix_control_plane::devices::revoke(&api.state.db, api.state.bus.as_ref(), None, host.id, Some(user), "test", (None, None))
        .await
        .unwrap();
    let mut got_revoked = false;
    let code = loop {
        match recv(&mut ws, true).await {
            Ok(ServerMessage::DeviceRevoked) => got_revoked = true,
            Ok(_) => {}
            Err(c) => break c,
        }
    };
    assert!(got_revoked);
    assert_eq!(code, close_code::DEVICE_REVOKED);
    wait_presence(&api, host.id, PresenceState::Offline, Duration::from_secs(2)).await;
    let s: String = sqlx::query_scalar("SELECT status FROM sessions WHERE id = $1")
        .bind(sid)
        .fetch_one(&api.state.db)
        .await
        .unwrap();
    assert_eq!(s, "failed");
    srv.task.abort();
    api.stop().await;
}

#[tokio::test]
async fn signaling_restart_devices_reconnect_to_the_new_instance() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let first = start_signaling(&api).await;
    let (org, _, _) = org_member(&api.state.db, OrgRole::Admin).await;
    let dev = enroll_agent(&api, org).await;
    let token = device_token(&api, &dev.key).await;
    let mut ws = connect(first.addr, Some(&token)).await.unwrap();
    let _ = recv(&mut ws, true).await;
    send(&mut ws, &presence_msg()).await;
    wait_presence(&api, dev.id, PresenceState::Online, Duration::from_secs(3)).await;

    // Graceful shutdown: 1012 to the client, device becomes connecting (not offline).
    let drain = first.sig.clone();
    tokio::spawn(async move { drain.drain(Duration::from_secs(3)).await });
    let code = loop {
        match recv(&mut ws, true).await {
            Err(c) => break c,
            Ok(_) => continue,
        }
    };
    assert_eq!(code, close_code::SERVICE_RESTART);
    wait_presence(&api, dev.id, PresenceState::Connecting, Duration::from_secs(2)).await;
    // The draining instance refuses new links.
    assert_eq!(connect(first.addr, Some(&token)).await.err(), Some(503));
    first.task.abort();

    // New instance; the device reconnects and is online again.
    let second = start_signaling(&api).await;
    let mut ws = connect(second.addr, Some(&token)).await.unwrap();
    let _ = recv(&mut ws, true).await;
    send(&mut ws, &presence_msg()).await;
    wait_presence(&api, dev.id, PresenceState::Online, Duration::from_secs(3)).await;
    second.task.abort();
    api.stop().await;
}
