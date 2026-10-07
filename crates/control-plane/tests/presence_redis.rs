//! Presence semantics against a real Redis (same scenarios as the in-memory store).

use std::time::Duration;

use aionix_control_plane::{
    presence::{RedisPresence, scenarios},
    testing::TestRedis,
};

async fn sleep(d: Duration) {
    tokio::time::sleep(d).await;
}

#[tokio::test]
async fn redis_presence_times_out() {
    let Some(r) = TestRedis::connect().await else {
        return;
    };
    let store = RedisPresence::new(r.conn.clone(), r.keys.clone());
    scenarios::timeout(&store, Duration::from_millis(800), sleep).await;
}

#[tokio::test]
async fn redis_presence_reconnects_and_ignores_superseded_links() {
    let Some(r) = TestRedis::connect().await else {
        return;
    };
    let store = RedisPresence::new(r.conn.clone(), r.keys.clone());
    scenarios::reconnect(&store, Duration::from_millis(800), sleep).await;
}
