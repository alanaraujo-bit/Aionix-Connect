//! `aionix-api` — Control Plane REST API.

use std::process::ExitCode;

use aionix_control_plane::{config::ApiConfig, janitor, server};
use aionix_server_common::{config::ProcessEnv, shutdown, telemetry};
use tracing::{error, info};

#[tokio::main]
async fn main() -> ExitCode {
    telemetry::init("aionix-api");
    let config = match ApiConfig::from_env(&ProcessEnv) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            error!(problems = e.0.len(), "invalid configuration; refusing to start");
            return ExitCode::from(2);
        }
    };
    let (state, health) = match server::build(&config).await {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "startup failed");
            return ExitCode::from(1);
        }
    };
    if config.settings.relay_urls.is_empty() {
        tracing::warn!("AIONIX_RELAY_URLS is empty: devices get no relay and only direct paths work");
    }
    janitor::spawn(state.db.clone(), state.presence.clone(), server::JANITOR_EVERY);
    let listener = match tokio::net::TcpListener::bind(config.bind).await {
        Ok(l) => l,
        Err(e) => {
            error!(error = %e, bind = %config.bind, "cannot bind");
            return ExitCode::from(1);
        }
    };
    info!(bind = %config.bind, "aionix-api listening");
    let app = server::app(state.clone(), health);
    if let Err(e) = server::serve(listener, app, shutdown::signal()).await {
        error!(error = %e, "server error");
        return ExitCode::from(1);
    }
    state.db.close().await;
    info!("aionix-api stopped");
    ExitCode::SUCCESS
}
