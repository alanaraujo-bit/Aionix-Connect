//! The standalone relay with real iroh endpoints (the production transport).

use std::time::Duration;

use aionix_control_api::claims::{DeviceClaims, RelayClaims};
use aionix_crypto::{IdentityKey, token::TokenIssuer};
use aionix_relay::{RelaySettings, RunningRelay, spawn_with};
use aionix_shared::{DeviceKind, TransportKind};
use aionix_transport::{RelayEndpoint, Transport, TransportConfig};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

const ALPN: &[u8] = b"aionix/test/1";

fn settings(issuer: &TokenIssuer) -> RelaySettings {
    RelaySettings {
        bind: "127.0.0.1:0".parse().unwrap(),
        status_bind: "127.0.0.1:0".parse().unwrap(),
        token_keys: vec![issuer.public_key()],
        redis_url: None,
        redis_prefix: "aionix".into(),
        client_rx_bytes_per_sec: None,
        connect_limit_per_minute: 1000,
        max_connections: 100,
    }
}

async fn endpoint(relay: &RunningRelay, key: &IdentityKey, token: String) -> Transport {
    Transport::bind(TransportConfig {
        secret_key: *key.secret_bytes(),
        relays: vec![RelayEndpoint {
            url: relay.relay_url(),
            token: Some(token),
            qad_port: None,
        }],
        alpns: vec![ALPN.to_vec()],
        port: 0,
        relay_only: true,
    })
    .await
    .unwrap()
}

fn grant(issuer: &TokenIssuer, key: &IdentityKey) -> String {
    issuer
        .issue(
            &RelayClaims {
                sub: Uuid::new_v4(),
                key: key.public(),
            },
            RelayClaims::TTL,
        )
        .token
}

async fn status(relay: &RunningRelay) -> serde_json::Value {
    reqwest::get(format!("http://{}/status", relay.status_addr))
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn authorized_endpoints_exchange_end_to_end_encrypted_traffic() {
    let issuer = TokenIssuer::new(IdentityKey::generate());
    let relay = spawn_with(&settings(&issuer), None).await.unwrap();
    let (hk, vk) = (IdentityKey::generate(), IdentityKey::generate());
    let host = endpoint(&relay, &hk, grant(&issuer, &hk)).await;
    let viewer = endpoint(&relay, &vk, grant(&issuer, &vk)).await;
    assert!(host.wait_online(Duration::from_secs(10)).await, "host admitted");
    assert!(viewer.wait_online(Duration::from_secs(10)).await, "viewer admitted");

    let host_key = host.local_key();
    let host_addr = host.addr();
    let server = tokio::spawn(async move {
        let conn = host.accept().await.unwrap().accept().await.unwrap();
        let (mut send, mut recv) = conn.accept_bi().await.unwrap();
        let mut buf = [0u8; 5];
        recv.read_exact(&mut buf).await.unwrap();
        send.write_all(&buf).await.unwrap();
        send.finish().unwrap();
        conn.closed().await;
    });
    let conn = viewer.connect(&host_key, &host_addr, ALPN).await.unwrap();
    assert_eq!(conn.remote_key(), host_key, "peer authenticated end-to-end");
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    send.write_all(b"hello").await.unwrap();
    assert_eq!(recv.read_to_end(64).await.unwrap(), b"hello");
    assert_eq!(conn.path().kind, TransportKind::Relay);

    let s = status(&relay).await;
    assert_eq!(s["connections_active"], 2);
    assert!(s["bytes_received_total"].as_u64().unwrap() > 0, "{s}");
    let metrics = reqwest::get(format!("http://{}/metrics", relay.status_addr))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(metrics.contains("aionix_relay_connections_active 2"), "{metrics}");
    let ready = reqwest::get(format!("http://{}/readyz", relay.status_addr)).await.unwrap();
    assert_eq!(ready.status(), 200);
    conn.close(0, "done");
    let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_grants_are_refused() {
    let issuer = TokenIssuer::new(IdentityKey::generate());
    let relay = spawn_with(&settings(&issuer), None).await.unwrap();
    let key = IdentityKey::generate();

    // Grant for another key (stolen token).
    let thief = endpoint(&relay, &key, grant(&issuer, &IdentityKey::generate())).await;
    assert!(!thief.wait_online(Duration::from_secs(3)).await);
    // Grant signed by an untrusted issuer.
    let rogue = TokenIssuer::new(IdentityKey::generate());
    let forged = endpoint(&relay, &IdentityKey::generate(), {
        let k = IdentityKey::generate();
        grant(&rogue, &k)
    })
    .await;
    assert!(!forged.wait_online(Duration::from_secs(3)).await);
    // Expired grant.
    let k = IdentityKey::generate();
    let expired = issuer
        .issue_at(
            &RelayClaims {
                sub: Uuid::new_v4(),
                key: k.public(),
            },
            RelayClaims::TTL,
            time::OffsetDateTime::now_utc() - time::Duration::days(1),
        )
        .token;
    let old = endpoint(&relay, &k, expired).await;
    assert!(!old.wait_online(Duration::from_secs(3)).await);
    // A device token is not a relay grant.
    let k = IdentityKey::generate();
    let wrong_type = issuer
        .issue(
            &DeviceClaims {
                sub: Uuid::new_v4(),
                key: k.public(),
                kind: DeviceKind::Agent,
            },
            DeviceClaims::TTL,
        )
        .token;
    let wt = endpoint(&relay, &k, wrong_type).await;
    assert!(!wt.wait_online(Duration::from_secs(3)).await);

    let s = status(&relay).await;
    assert_eq!(s["connections_active"], 0, "{s}");
    assert!(s["denied"]["key_mismatch"].as_u64().unwrap() >= 1, "{s}");
    assert!(s["denied"]["invalid_grant"].as_u64().unwrap() >= 3, "{s}");
    relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn connection_attempts_are_rate_limited_and_capped() {
    let issuer = TokenIssuer::new(IdentityKey::generate());
    let mut s = settings(&issuer);
    s.max_connections = 1;
    let relay = spawn_with(&s, None).await.unwrap();
    let a = IdentityKey::generate();
    let b = IdentityKey::generate();
    let first = endpoint(&relay, &a, grant(&issuer, &a)).await;
    assert!(first.wait_online(Duration::from_secs(10)).await);
    let second = endpoint(&relay, &b, grant(&issuer, &b)).await;
    assert!(!second.wait_online(Duration::from_secs(3)).await, "over capacity");
    let st = status(&relay).await;
    assert!(st["denied"]["capacity"].as_u64().unwrap() >= 1, "{st}");
    relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revoked_devices_are_refused_before_their_grant_expires() {
    let Some(r) = aionix_control_plane::testing::TestRedis::connect().await else {
        return;
    };
    let issuer = TokenIssuer::new(IdentityKey::generate());
    let relay = spawn_with(&settings(&issuer), Some((r.conn.clone(), r.keys.prefix().to_owned())))
        .await
        .unwrap();
    let key = IdentityKey::generate();
    let device = Uuid::new_v4();
    let token = issuer
        .issue(&RelayClaims { sub: device, key: key.public() }, RelayClaims::TTL)
        .token;
    let mut c = r.conn.clone();
    let _: () = redis::cmd("SET")
        .arg(r.keys.revoked_device(device))
        .arg("test")
        .query_async(&mut c)
        .await
        .unwrap();
    let t = endpoint(&relay, &key, token).await;
    assert!(!t.wait_online(Duration::from_secs(3)).await);
    let st = status(&relay).await;
    assert!(st["denied"]["revoked"].as_u64().unwrap() >= 1, "{st}");
    relay.shutdown().await;
}
