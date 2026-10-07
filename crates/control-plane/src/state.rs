//! Shared state of the API process.

use std::sync::Arc;

use aionix_server_common::http::TrustedProxies;
use sqlx::PgPool;

use crate::{
    bus::SignalBus, config::ApiSettings, presence::PresenceStore, ratelimit::RateLimiter,
    redis_conn::Keys, tokens::Tokens,
};

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub tokens: Arc<Tokens>,
    pub presence: Arc<dyn PresenceStore>,
    pub bus: Arc<dyn SignalBus>,
    pub limiter: Arc<RateLimiter>,
    pub settings: Arc<ApiSettings>,
    pub proxies: TrustedProxies,
    /// Redis connection for relay revocation markers (`None` in memory-only setups).
    pub redis: Option<(redis::aio::ConnectionManager, Keys)>,
}
