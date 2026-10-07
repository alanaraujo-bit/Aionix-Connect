//! Liveness and readiness.
//!
//! * `GET /healthz` — the process is alive and serving HTTP. Always 200 while running.
//! * `GET /readyz` — every dependency the service needs actually works right now
//!   (PostgreSQL answers and is migrated, Redis answers, the relay listener is bound…).
//!   503 with the failing checks otherwise. Docker health checks and the reverse proxy
//!   use readiness; restarts are left to liveness so a database outage does not cause a
//!   restart loop.
//! * `GET /metrics` — Prometheus text format.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use serde::Serialize;

pub type CheckFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// One readiness dependency.
pub trait ReadinessCheck: Send + Sync {
    fn name(&self) -> &'static str;
    fn check(&self) -> CheckFuture<'_>;
}

/// A check built from a closure.
pub struct FnCheck<F> {
    name: &'static str,
    f: F,
}

impl<F> FnCheck<F>
where
    F: Fn() -> CheckFuture<'static> + Send + Sync,
{
    pub fn new(name: &'static str, f: F) -> Self {
        Self { name, f }
    }
}

impl<F> ReadinessCheck for FnCheck<F>
where
    F: Fn() -> CheckFuture<'static> + Send + Sync,
{
    fn name(&self) -> &'static str {
        self.name
    }
    fn check(&self) -> CheckFuture<'_> {
        (self.f)()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name: &'static str,
    pub ok: bool,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Readiness {
    pub service: &'static str,
    pub version: &'static str,
    pub ready: bool,
    pub checks: Vec<CheckResult>,
}

#[derive(Clone)]
pub struct Health {
    service: &'static str,
    checks: Arc<Vec<Box<dyn ReadinessCheck>>>,
    timeout: Duration,
}

impl Health {
    pub fn new(service: &'static str, checks: Vec<Box<dyn ReadinessCheck>>) -> Self {
        Self {
            service,
            checks: Arc::new(checks),
            timeout: Duration::from_secs(3),
        }
    }

    pub async fn readiness(&self) -> Readiness {
        let mut results = Vec::with_capacity(self.checks.len());
        for c in self.checks.iter() {
            let start = Instant::now();
            let outcome = match tokio::time::timeout(self.timeout, c.check()).await {
                Ok(r) => r,
                Err(_) => Err("timed out".to_owned()),
            };
            results.push(CheckResult {
                name: c.name(),
                ok: outcome.is_ok(),
                latency_ms: start.elapsed().as_millis() as u64,
                error: outcome.err(),
            });
        }
        Readiness {
            service: self.service,
            version: crate::VERSION,
            ready: results.iter().all(|r| r.ok),
            checks: results,
        }
    }

    /// `/healthz`, `/readyz` and `/metrics`.
    pub fn router<S: Clone + Send + Sync + 'static>(self) -> Router<S> {
        Router::new()
            .route("/healthz", get(liveness))
            .route("/readyz", get(readiness))
            .route("/metrics", get(metrics))
            .with_state(self)
    }
}

async fn liveness(State(h): State<Health>) -> impl IntoResponse {
    Json(serde_json::json!({
        "service": h.service,
        "version": crate::VERSION,
        "alive": true,
    }))
}

async fn readiness(State(h): State<Health>) -> impl IntoResponse {
    let r = h.readiness().await;
    let code = if r.ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(r))
}

async fn metrics() -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        crate::telemetry::metrics().render(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn readiness_reports_each_failing_dependency() {
        let ok = FnCheck::new("postgres", || Box::pin(async { Ok(()) }));
        let bad = FnCheck::new("redis", || {
            Box::pin(async { Err("connection refused".into()) })
        });
        let h = Health::new("test", vec![Box::new(ok), Box::new(bad)]);
        let r = h.readiness().await;
        assert!(!r.ready);
        assert!(r.checks[0].ok);
        assert_eq!(r.checks[1].error.as_deref(), Some("connection refused"));
        let all_ok = Health::new("t", vec![]).readiness().await;
        assert!(all_ok.ready);
    }
}
