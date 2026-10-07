//! Assembly of the `aionix-api` process (also used by integration tests).

use std::{future::Future, net::SocketAddr, sync::Arc, time::Duration};

use aionix_server_common::{
    health::{FnCheck, Health, ReadinessCheck},
    http::with_request_tracing,
    keys::load_signing_key,
};
use axum::Router;
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use tokio::net::TcpListener;
use tracing::info;

use crate::{
    api,
    bus::{RedisBus, SignalBus},
    config::{ApiConfig, ApiSettings},
    db,
    presence::RedisPresence,
    ratelimit::RateLimiter,
    redis_conn::{self, Keys},
    state::AppState,
    tokens::Tokens,
};

/// Readiness checks shared by API and signaling: schema migrated, Redis reachable.
pub fn store_checks(db: PgPool, redis: ConnectionManager) -> Vec<Box<dyn ReadinessCheck>> {
    vec![
        Box::new(FnCheck::new("postgres", move || {
            let db = db.clone();
            Box::pin(async move { db::check_ready(&db).await })
        })),
        Box::new(FnCheck::new("redis", move || {
            let r = redis.clone();
            Box::pin(async move { redis_conn::ping(&r).await })
        })),
    ]
}

/// Builds the API state from already-connected stores.
pub fn state_from_parts(
    db: PgPool,
    redis_client: redis::Client,
    redis: ConnectionManager,
    keys: Keys,
    tokens: Tokens,
    settings: ApiSettings,
    proxies: aionix_server_common::http::TrustedProxies,
) -> (AppState, Health) {
    let bus = Arc::new(RedisBus::new(redis_client, redis.clone(), keys.clone()));
    bus.start_offer_listener();
    let presence = Arc::new(RedisPresence::new(redis.clone(), keys.clone()));
    let limiter = Arc::new(RateLimiter::redis(redis.clone(), keys.clone()));
    let mut checks = store_checks(db.clone(), redis.clone());
    let bus_check = bus.clone();
    checks.push(Box::new(FnCheck::new("bus", move || {
        let ok = bus_check.listening();
        Box::pin(async move {
            if ok {
                Ok(())
            } else {
                Err("offer subscription not active".to_owned())
            }
        })
    })));
    let state = AppState {
        db,
        tokens: Arc::new(tokens),
        presence,
        bus: bus as Arc<dyn SignalBus>,
        limiter,
        settings: Arc::new(settings),
        proxies,
        redis: Some((redis, keys)),
    };
    (state, Health::new("aionix-api", checks))
}

pub async fn build(config: &ApiConfig) -> anyhow::Result<(AppState, Health)> {
    let signing = load_signing_key(&config.signing_key_file)?;
    let tokens = Tokens::new(signing, config.previous_public_keys.clone());
    info!(
        key_id = %tokens.public_keys()[0].key_id(),
        database = %aionix_server_common::config::redact_url(&config.database_url),
        "token signing key loaded"
    );
    let pool = db::connect(&config.database_url, config.db_max_connections, 30).await?;
    let (client, conn) = redis_conn::connect(&config.redis_url, 30).await?;
    Ok(state_from_parts(
        pool,
        client,
        conn,
        Keys::new(config.redis_prefix.clone()),
        tokens,
        config.settings.clone(),
        config.trusted_proxies.clone(),
    ))
}

pub fn app(state: AppState, health: Health) -> Router {
    with_request_tracing(api::router(state).merge(health.router()))
}

pub async fn serve(
    listener: TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

/// Janitor cadence.
pub const JANITOR_EVERY: Duration = Duration::from_secs(30);
