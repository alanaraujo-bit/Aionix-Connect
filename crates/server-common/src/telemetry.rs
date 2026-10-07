//! Structured logging and metrics.
//!
//! Logs are JSON lines on stdout by default (`AIONIX_LOG_FORMAT=json|pretty`), filtered
//! by `AIONIX_LOG` / `RUST_LOG`. Docker collects stdout; nothing is written to files by
//! the processes themselves.
//!
//! Logging rules for every server component: never log passwords, private keys, token
//! values, refresh tokens, enrollment tokens, authorization headers, clipboard contents
//! or screen contents. Log identifiers (request id, user id, device id, session id,
//! organization id) instead.

use std::sync::OnceLock;

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

const DEFAULT_FILTER: &str = "info,sqlx=warn,iroh_relay=info,hyper=warn,tower_http=info";

/// Initializes the global subscriber. Safe to call more than once (later calls are
/// ignored), which keeps tests simple.
pub fn init(service: &'static str) {
    let filter = std::env::var("AIONIX_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| DEFAULT_FILTER.to_owned());
    let filter = EnvFilter::try_new(filter).unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let pretty = std::env::var("AIONIX_LOG_FORMAT").is_ok_and(|v| v.eq_ignore_ascii_case("pretty"));
    let registry = tracing_subscriber::registry().with(filter);
    let result = if pretty {
        registry.with(fmt::layer().with_target(true)).try_init()
    } else {
        registry
            .with(
                fmt::layer()
                    .json()
                    .flatten_event(true)
                    .with_current_span(true)
                    .with_span_list(false),
            )
            .try_init()
    };
    if result.is_ok() {
        tracing::info!(service, version = crate::VERSION, "starting");
    }
}

/// Process-wide Prometheus recorder (installed once).
pub fn metrics() -> PrometheusHandle {
    static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
    HANDLE
        .get_or_init(|| {
            PrometheusBuilder::new()
                .install_recorder()
                .unwrap_or_else(|_| {
                    // Another recorder is already installed (tests embedding several
                    // services): fall back to a detached handle.
                    PrometheusBuilder::new().build_recorder().handle()
                })
        })
        .clone()
}
