//! Manual end-to-end check on one Windows machine: technician and host run in this
//! process, connected through an in-process relay, with the DEVELOPMENT ONLY ticket
//! authority and the real approval window. Nothing is approved automatically.
//!
//! cargo run -p aionix-remote-core --example dev_loopback_session -- [seconds] [approval-timeout-seconds]
//!
//! While connected, text copied on this machine is synchronized to the (in-memory)
//! technician clipboard and printed. No input is sent by this example.

#[cfg(windows)]
#[tokio::main]
async fn main() {
    use std::{
        net::SocketAddr,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        time::Duration,
    };

    use aionix_crypto::IdentityKey;
    use aionix_protocol::ALPN;
    use aionix_remote_core::session::{
        DeviceLookup, HostConfig, HostService, HostServices, TechnicianConfig, TechnicianIdentity,
        TechnicianServices, TechnicianSession, TicketValidator,
        approval::WindowsApprovalPrompt,
        dev_authority::DevelopmentAuthority,
        media::{DecodedVideoFrame, FrameConsumer, MemoryClipboard, SystemInputFactory},
        windows_media::{MfDecoderFactory, ScreenVideoSource, SystemClipboard},
    };
    use aionix_shared::FriendlyId;
    use aionix_transport::{RelayEndpoint, Transport, TransportConfig};
    use iroh_relay::server::{RelayConfig, Server, ServerConfig};
    use uuid::Uuid;

    struct Counter(Arc<AtomicU64>);
    impl FrameConsumer for Counter {
        fn on_frame(&mut self, _: DecodedVideoFrame) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter("warn,aionix_remote_core=info,aionix_platform=info")
        .init();
    aionix_platform::windows::init_dpi_awareness();
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(15);
    let approval_timeout: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(60);

    let mut relay_cfg = ServerConfig::default();
    relay_cfg.relay = Some(RelayConfig::new(
        "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
    ));
    let relay = Server::spawn(relay_cfg).await.expect("relay");
    let relay_url = format!("http://{}", relay.http_addr().unwrap());
    let bind = |url: String| async move {
        let t = Transport::bind(TransportConfig {
            secret_key: *IdentityKey::generate().secret_bytes(),
            relays: vec![RelayEndpoint {
                url,
                token: None,
                qad_port: None,
            }],
            alpns: vec![ALPN.to_vec()],
            port: 0,
            relay_only: false,
        })
        .await
        .expect("transport");
        t.wait_online(Duration::from_secs(10)).await;
        t
    };
    let host_t = bind(relay_url.clone()).await;
    let tech_t = bind(relay_url).await;

    let authority = Arc::new(DevelopmentAuthority::new());
    let host_id = Uuid::new_v4();
    let friendly = FriendlyId::random();
    authority.register_host(host_id, friendly, host_t.local_key(), host_t.addr());
    let display_id = aionix_capture::windows::enumerate_displays()
        .ok()
        .and_then(|d| d.into_iter().find(|d| d.primary).map(|d| d.id))
        .unwrap_or(1);
    let host = HostService::start(
        host_t.clone(),
        HostConfig {
            host_name: aionix_platform::windows::sysinfo::hostname(),
            display_id,
            approval_timeout: Duration::from_secs(approval_timeout),
            ..Default::default()
        },
        HostServices {
            validator: Arc::new(TicketValidator::new(
                authority.verifier(),
                host_id,
                host_t.local_key(),
            )),
            approval: Arc::new(WindowsApprovalPrompt),
            video: Arc::new(ScreenVideoSource::default()),
            input: Arc::new(SystemInputFactory),
            clipboard: Arc::new(SystemClipboard),
        },
    );

    let frames = Arc::new(AtomicU64::new(0));
    let tech_clipboard = Arc::new(MemoryClipboard::new());
    let session = TechnicianSession::start(
        tech_t.clone(),
        TechnicianConfig::new(
            TechnicianIdentity {
                user_id: Uuid::new_v4(),
                device_id: Uuid::new_v4(),
                key: tech_t.local_key(),
                display_name: "Técnico de teste (loopback)".into(),
                email: "dev@localhost".into(),
                org_name: None,
            },
            DeviceLookup::Friendly(friendly),
        ),
        TechnicianServices {
            authority,
            decoder: Arc::new(MfDecoderFactory),
            frames: Box::new(Counter(frames.clone())),
            clipboard: tech_clipboard.clone(),
        },
    );

    println!("host friendly id: {friendly} — answer the prompt on screen");
    let status = session
        .wait_for(Duration::from_secs(approval_timeout + 30), |s| {
            s.state.is_live() || s.state.is_terminal()
        })
        .await
        .unwrap_or_else(|s| s);
    println!("technician: {:?}", status.state);
    if status.state.is_live() {
        let mut last_clip = None;
        for _ in 0..seconds {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let d = session.diagnostics();
            println!(
                "{:?} frames={} fps={:?} res={:?} rtt_ms={:?} kbps={:?} transport={:?} encoder={:?} capture={:?} decode_ms={:?}",
                d.state,
                frames.load(Ordering::Relaxed),
                d.fps.value().map(|v| v.round()),
                d.resolution.value(),
                d.rtt_ms.value().map(|v| (v * 10.0).round() / 10.0),
                d.bitrate_kbps.value().map(|v| v.round()),
                d.transport.value(),
                d.encoder.value(),
                d.capture_backend.value(),
                d.decode_latency_ms
                    .value()
                    .map(|v| (v * 10.0).round() / 10.0),
            );
            let clip = tech_clipboard.get();
            if clip != last_clip {
                println!("technician clipboard <- {clip:?}");
                last_clip = clip;
            }
        }
        session.end();
    }
    let end = session
        .wait_for(Duration::from_secs(10), |s| s.state.is_terminal())
        .await
        .unwrap_or_else(|s| s);
    println!("final: {:?} {:?}", end.state, end.reject_reason);
    host.shutdown().await;
    host_t.close().await;
    tech_t.close().await;
}

#[cfg(not(windows))]
fn main() {}
