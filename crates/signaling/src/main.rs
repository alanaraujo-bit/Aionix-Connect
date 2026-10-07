//! `aionix-signaling` binary.

use std::{net::SocketAddr, process::ExitCode, time::Duration};

use aionix_server_common::{config::ProcessEnv, shutdown, telemetry};
use aionix_signaling::{SignalingConfig, build, router};
use tracing::{error, info};

#[tokio::main]
async fn main() -> ExitCode {
    telemetry::init("aionix-signaling");
    let config = match SignalingConfig::from_env(&ProcessEnv) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            error!(
                problems = e.0.len(),
                "invalid configuration; refusing to start"
            );
            return ExitCode::from(2);
        }
    };
    let (sig, health) = match build(&config).await {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "startup failed");
            return ExitCode::from(1);
        }
    };
    let listener = match tokio::net::TcpListener::bind(config.bind).await {
        Ok(l) => l,
        Err(e) => {
            error!(error = %e, bind = %config.bind, "cannot bind");
            return ExitCode::from(1);
        }
    };
    info!(bind = %config.bind, "aionix-signaling listening");
    let drain = sig.clone();
    let app = router(sig, health).into_make_service_with_connect_info::<SocketAddr>();
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown::signal().await;
            // Tell every device to reconnect (1012) before the process exits.
            drain.drain(Duration::from_secs(8)).await;
        })
        .await;
    if let Err(e) = result {
        error!(error = %e, "server error");
        return ExitCode::from(1);
    }
    info!("aionix-signaling stopped");
    ExitCode::SUCCESS
}
