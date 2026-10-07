//! `aionix-relay` binary.

use std::process::ExitCode;

use aionix_relay::{RelaySettings, spawn};
use aionix_server_common::{config::ProcessEnv, shutdown, telemetry};
use tracing::error;

#[tokio::main]
async fn main() -> ExitCode {
    telemetry::init("aionix-relay");
    let settings = match RelaySettings::from_env(&ProcessEnv) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            error!(
                problems = e.0.len(),
                "invalid configuration; refusing to start"
            );
            return ExitCode::from(2);
        }
    };
    let relay = match spawn(&settings).await {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "startup failed");
            return ExitCode::from(1);
        }
    };
    shutdown::signal().await;
    relay.shutdown().await;
    ExitCode::SUCCESS
}
