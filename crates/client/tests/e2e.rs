//! End to end with the real services: API + signaling (+ PostgreSQL + Redis).
//!
//! host: identity file → enrollment (agent token) → device login → signaling link with
//!       `HostOfferHandler` → presence Online
//! technician: user login → workstation enrollment → `ControlPlaneAuthority`
//!       → ticket → validated by the host's real `TicketValidator` (remote-core)

use std::{net::SocketAddr, sync::Arc, time::Duration};

use aionix_client::{
    ApiClient,
    authority::{ControlPlaneAuthority, UserSession},
    enroll::{DeviceSession, Enrollment, ensure_enrolled},
    host::HostOfferHandler,
    identity::{FileIdentityStore, IdentityStore, Protection},
    signaling::{LinkState, PresenceInfo, SignalingClient, SignalingOptions, StopReason},
};
use aionix_control_api::{
    claims::SessionPermission,
    rest::{DeviceInfo, OrgRole, PresenceState},
    signal::EndpointAddrDto,
};
use aionix_control_plane::{
    admin,
    presence::state_of,
    testing::{TEST_PASSWORD, TestApi, org_member},
};
use aionix_remote_core::session::{
    authority::{AuthorizationProvider, DeviceLookup, SessionRequest, TechnicianIdentity},
    ticket::{TicketRejection, TicketValidator},
};
use aionix_server_common::{http::TrustedProxies, keys::verifier_for};
use aionix_shared::{AccessMode, DeviceKind};
use aionix_signaling::{Signaling, Timing};
use tokio::{net::TcpListener, sync::watch, task::JoinHandle};
use uuid::Uuid;

fn info() -> DeviceInfo {
    DeviceInfo {
        hostname: "PC-CLIENTE".into(),
        os_name: "Windows".into(),
        os_version: "11".into(),
        app_version: "0.1.0".into(),
        username: String::new(),
    }
}

async fn serve_signaling(api: &TestApi, listener: TcpListener) -> (Arc<Signaling>, JoinHandle<()>) {
    let (sig, health) = aionix_signaling::from_parts(
        api.db.pool.clone(),
        api.redis.client.clone(),
        api.redis.conn.clone(),
        api.redis.keys.clone(),
        verifier_for(api.state.tokens.public_keys()),
        Timing {
            ping_interval: Duration::from_secs(1),
            heartbeat_timeout: Duration::from_secs(4),
            reconnect_grace: Duration::from_secs(3),
        },
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
    let app = aionix_signaling::router(sig.clone(), health)
        .into_make_service_with_connect_info::<SocketAddr>();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (sig, task)
}

async fn presence(api: &TestApi, device: Uuid) -> PresenceState {
    state_of(api.state.presence.get(device).await.unwrap().as_ref())
}

#[tokio::test(flavor = "multi_thread")]
async fn enrollment_presence_ticket_restart_and_revocation() {
    let Some(api) = TestApi::start().await else {
        return;
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let signal_addr = listener.local_addr().unwrap();
    let (sig, sig_task) = serve_signaling(&api, listener).await;
    let client = ApiClient::new(&api.base).unwrap();
    let (org, _, email) = org_member(&api.state.db, OrgRole::Technician).await;
    let (_, enroll_token, _) = admin::insert_enrollment_token(&api.state.db, org, None, 1, 1, None)
        .await
        .unwrap();

    // ---- Host enrollment from a fresh identity file.
    let dir = std::env::temp_dir().join(format!("aionix-e2e-{}", Uuid::new_v4()));
    let store = FileIdentityStore::new(dir.join("host.json"), Protection::None);
    let (host_key, reg) = ensure_enrolled(
        &client,
        &store,
        Enrollment::Agent {
            enrollment_token: &enroll_token,
        },
        info(),
    )
    .await
    .unwrap();
    assert_eq!(reg.kind, DeviceKind::Agent);
    assert_eq!(reg.token_keys, api.state.tokens.public_keys());
    // Restart of the host app: same identity, no new registration (the single-use
    // enrollment token would be refused anyway).
    let (again_key, again_reg) = ensure_enrolled(&client, &store, Enrollment::Agent { enrollment_token: "spent" }, info())
        .await
        .unwrap();
    assert_eq!(again_key.public(), host_key.public());
    assert_eq!(again_reg, reg);
    // The file never contains anything the server could not see except the secret;
    // the server never received the secret.
    let stored: String = sqlx::query_scalar("SELECT public_key FROM devices WHERE id = $1")
        .bind(reg.device_id)
        .fetch_one(&api.state.db)
        .await
        .unwrap();
    assert_eq!(stored, host_key.public().to_base64());

    let host_session = Arc::new(DeviceSession::new(client.clone(), host_key, reg.clone(), Some(info())));
    let validator = Arc::new(TicketValidator::new(
        host_session.ticket_verifier(),
        reg.device_id,
        host_session.key().public(),
    ));
    let (presence_tx, presence_rx) = watch::channel(PresenceInfo {
        addr: EndpointAddrDto {
            relay_urls: vec!["https://relay.aionix.test/".into()],
            direct_addrs: vec!["192.0.2.33:7000".into()],
        },
        busy: false,
        unattended_enabled: false,
    });
    let host_link = SignalingClient::spawn(
        SignalingOptions {
            url: format!("ws://{signal_addr}/v1/signal"),
            min_backoff: Duration::from_millis(200),
            max_backoff: Duration::from_secs(1),
        },
        host_session.clone(),
        presence_rx.clone(),
        Arc::new(HostOfferHandler::new(validator.clone(), presence_rx)),
    );
    assert!(host_link.wait_for(Duration::from_secs(10), |s| *s == LinkState::Online).await);
    for _ in 0..30 {
        if presence(&api, reg.device_id).await == PresenceState::Online {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(presence(&api, reg.device_id).await, PresenceState::Online);

    // ---- Technician.
    let user = Arc::new(UserSession::login(client.clone(), &email, TEST_PASSWORD).await.unwrap());
    let access = user.access_token().await.unwrap();
    let ws_store = aionix_client::identity::MemoryIdentityStore::default();
    let (ws_key, ws_reg) = ensure_enrolled(&client, &ws_store, Enrollment::Technician { access_token: &access }, info())
        .await
        .unwrap();
    let ws_public = ws_key.public();
    let workstation = Arc::new(DeviceSession::new(client.clone(), ws_key, ws_reg.clone(), None));
    let authority = ControlPlaneAuthority::new(user.clone(), workstation.clone());
    let profile = user.profile().await;
    let request = SessionRequest {
        technician: TechnicianIdentity {
            user_id: profile.id,
            device_id: ws_reg.device_id,
            key: ws_public,
            display_name: profile.display_name.clone(),
            email: profile.email.clone(),
            org_name: None,
        },
        target: DeviceLookup::Friendly(reg.friendly_id),
        mode: AccessMode::Assisted,
        permissions: vec![SessionPermission::View, SessionPermission::Control],
    };
    let grant = authority.request_session(request.clone()).await.unwrap();
    assert_eq!(grant.host_device_id, reg.device_id);
    assert_eq!(grant.host_key, host_session.key().public());
    assert_eq!(grant.host_addr.direct_addrs, ["192.0.2.33:7000"]);
    assert_eq!(grant.permissions, [SessionPermission::View, SessionPermission::Control]);

    // The host's real validator accepts the Control Plane ticket exactly once, only
    // from the workstation key, and only for the announced session id.
    let thief = aionix_crypto::IdentityKey::generate().public();
    assert_eq!(validator.validate(&grant.ticket, &thief), Err(TicketRejection::WrongTechnician));
    let authorized = validator.validate(&grant.ticket, &ws_public).unwrap();
    assert_eq!(authorized.session_id, grant.session_id);
    assert_eq!(authorized.technician.user_id, profile.id);
    assert!(authorized.permissions.control && !authorized.permissions.clipboard);
    assert_eq!(validator.validate(&grant.ticket, &ws_public), Err(TicketRejection::Replayed));

    // A request naming another key is refused client-side.
    let mut forged = request.clone();
    forged.technician.key = thief;
    assert!(authority.request_session(forged).await.is_err());

    // ---- Signaling restart: the host reconnects by itself.
    sig.drain(Duration::from_secs(3)).await;
    sig_task.abort();
    let _ = sig_task.await;
    assert!(host_link.wait_for(Duration::from_secs(5), |s| *s == LinkState::Connecting).await);
    assert_ne!(presence(&api, reg.device_id).await, PresenceState::Online);
    let listener = TcpListener::bind(signal_addr).await.expect("rebind signaling port");
    let (_sig2, sig_task2) = serve_signaling(&api, listener).await;
    assert!(host_link.wait_for(Duration::from_secs(15), |s| *s == LinkState::Online).await);
    for _ in 0..30 {
        if presence(&api, reg.device_id).await == PresenceState::Online {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(presence(&api, reg.device_id).await, PresenceState::Online);
    // Presence updates flow (busy hosts are refused by the server).
    presence_tx.send_modify(|p| p.busy = true);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(authority.request_session(request.clone()).await.is_err());
    presence_tx.send_modify(|p| p.busy = false);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(authority.request_session(request.clone()).await.is_ok());

    // ---- Revocation stops the link for good.
    aionix_control_plane::devices::revoke(&api.state.db, api.state.bus.as_ref(), None, reg.device_id, None, "test", (None, None))
        .await
        .unwrap();
    assert!(
        host_link
            .wait_for(Duration::from_secs(5), |s| *s == LinkState::Stopped(StopReason::Revoked))
            .await
    );
    assert_eq!(presence(&api, reg.device_id).await, PresenceState::Offline);

    user.logout().await.unwrap();
    sig_task2.abort();
    let _ = store.load();
    let _ = std::fs::remove_dir_all(dir);
    api.stop().await;
}
