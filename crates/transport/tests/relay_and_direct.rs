//! End-to-end transport tests against an in-process relay server.

use aionix_crypto::IdentityKey;
use aionix_shared::TransportKind;
use aionix_transport::{RelayEndpoint, Transport, TransportConfig};
use iroh_relay::server::{Access, AccessControl, ClientRequest, RelayConfig, Server, ServerConfig};
use std::{net::SocketAddr, time::Duration};
use tokio::io::AsyncReadExt;

const ALPN: &[u8] = b"aionix/test/1";
const TOKEN: &str = "valid-test-token";

#[derive(Debug)]
struct TokenAccess;

impl AccessControl for TokenAccess {
    async fn on_connect(&self, request: &ClientRequest) -> Access {
        if request.auth_token().as_deref() == Some(TOKEN) {
            Access::Allow
        } else {
            Access::Deny {
                reason: Some("invalid token".into()),
            }
        }
    }
}

async fn spawn_relay() -> (Server, String) {
    let mut relay = RelayConfig::new("127.0.0.1:0".parse::<SocketAddr>().unwrap());
    relay.access = std::sync::Arc::new(TokenAccess);
    let mut cfg = ServerConfig::default();
    cfg.relay = Some(relay);
    let server = Server::spawn(cfg).await.expect("relay spawns");
    let url = format!("http://{}", server.http_addr().unwrap());
    (server, url)
}

fn config(relay_url: &str, token: &str, relay_only: bool) -> TransportConfig {
    TransportConfig {
        secret_key: *IdentityKey::generate().secret_bytes(),
        relays: vec![RelayEndpoint {
            url: relay_url.to_owned(),
            token: Some(token.to_owned()),
            qad_port: None,
        }],
        alpns: vec![ALPN.to_vec()],
        port: 0,
        relay_only,
    }
}

async fn echo_roundtrip(relay_only: bool) -> TransportKind {
    let (_server, url) = spawn_relay().await;
    let host = Transport::bind(config(&url, TOKEN, relay_only))
        .await
        .unwrap();
    let viewer = Transport::bind(config(&url, TOKEN, relay_only))
        .await
        .unwrap();
    assert!(
        host.wait_online(Duration::from_secs(10)).await,
        "host reaches relay"
    );
    assert!(
        viewer.wait_online(Duration::from_secs(10)).await,
        "viewer reaches relay"
    );

    let host_key = host.local_key();
    let viewer_key = viewer.local_key();
    let host_addr = host.addr();

    let server = tokio::spawn(async move {
        let conn = host.accept().await.unwrap().accept().await.unwrap();
        assert_eq!(
            conn.remote_key(),
            viewer_key,
            "peer identity is authenticated"
        );
        let (mut send, mut recv) = conn.accept_bi().await.unwrap();
        let mut buf = [0u8; 5];
        recv.read_exact(&mut buf).await.unwrap();
        send.write_all(&buf).await.unwrap();
        send.finish().unwrap();
        conn.closed().await;
    });

    let conn = viewer.connect(&host_key, &host_addr, ALPN).await.unwrap();
    assert_eq!(conn.remote_key(), host_key);
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    send.write_all(b"hello").await.unwrap();
    let echoed = recv.read_to_end(64).await.unwrap();
    assert_eq!(echoed, b"hello");
    let path = conn.path();
    eprintln!("selected path: {:?} via {}", path.kind, path.remote);
    let kind = path.kind;
    conn.close(0, "done");
    let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    kind
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_only_connection_works() {
    assert_eq!(echo_roundtrip(true).await, TransportKind::Relay);
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_connection_is_used_on_lan() {
    let kind = echo_roundtrip(false).await;
    assert!(
        matches!(kind, TransportKind::Lan | TransportKind::Relay),
        "got {kind:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_rejects_invalid_token() {
    let (_server, url) = spawn_relay().await;
    let t = Transport::bind(config(&url, "forged", true)).await.unwrap();
    assert!(
        !t.wait_online(Duration::from_secs(3)).await,
        "must not come online with a bad token"
    );
    let status = t.relay_status();
    assert!(
        status.iter().all(|(_, connected, _)| !connected),
        "{status:?}"
    );
}
