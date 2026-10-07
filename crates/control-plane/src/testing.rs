//! Integration-test support (feature `test-support`).
//!
//! Tests use the real PostgreSQL and Redis of the local Compose stack
//! (`docker compose up -d`, see DEVELOPMENT in SERVER.md):
//!
//! * `AIONIX_TEST_DATABASE_URL` — maintenance connection (default
//!   `postgres://aionix:aionix-dev@127.0.0.1:55432/postgres`). Every test gets its own
//!   freshly created and migrated database, dropped afterwards.
//! * `AIONIX_TEST_REDIS_URL` — default `redis://127.0.0.1:56379`. Every test uses its
//!   own key prefix.
//!
//! When they are unreachable the tests print `SKIPPED` and return, unless
//! `AIONIX_REQUIRE_INTEGRATION=1` (CI, quality gate), which turns that into a failure.

use std::{net::SocketAddr, time::Duration};

use aionix_control_api::rest::OrgRole;
use aionix_crypto::IdentityKey;
use aionix_server_common::{health::Health, http::TrustedProxies};
use redis::aio::ConnectionManager;
use sqlx::{AssertSqlSafe, PgPool, postgres::PgPoolOptions};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::{
    config::ApiSettings,
    db,
    redis_conn::{self, Keys},
    server,
    state::AppState,
    tokens::Tokens,
};

pub const DEFAULT_TEST_DATABASE_URL: &str = "postgres://aionix:aionix-dev@127.0.0.1:55432/postgres";
pub const DEFAULT_TEST_REDIS_URL: &str = "redis://127.0.0.1:56379";

pub fn integration_required() -> bool {
    std::env::var("AIONIX_REQUIRE_INTEGRATION").is_ok_and(|v| v == "1")
}

/// Reports a missing dependency: panics when integration tests are required.
pub fn skip(what: &str, why: &str) {
    if integration_required() {
        panic!("{what} unavailable ({why}) and AIONIX_REQUIRE_INTEGRATION=1");
    }
    eprintln!("SKIPPED: {what} unavailable ({why}); run `docker compose up -d` to enable");
}

fn with_database(url: &str, name: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };
    let cut = base.rfind('/').map(|i| i + 1).unwrap_or(base.len());
    let mut out = format!("{}{}", &base[..cut], name);
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    out
}

pub struct TestDb {
    pub pool: PgPool,
    pub url: String,
    name: String,
    admin_url: String,
}

impl TestDb {
    /// A new empty database with every migration applied.
    pub async fn create() -> Option<Self> {
        let db = Self::create_unmigrated().await?;
        db::migrate(&db.pool)
            .await
            .expect("migrations apply to an empty database");
        Some(db)
    }

    /// A new empty database without migrations (migration tests).
    pub async fn create_unmigrated() -> Option<Self> {
        let admin_url = std::env::var("AIONIX_TEST_DATABASE_URL")
            .unwrap_or_else(|_| DEFAULT_TEST_DATABASE_URL.to_owned());
        Self::create_unmigrated_at(&admin_url).await
    }

    async fn create_unmigrated_at(admin_url: &str) -> Option<Self> {
        let admin = match PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(3))
            .connect(admin_url)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                skip("PostgreSQL", &e.to_string());
                return None;
            }
        };
        let name = format!("aionix_test_{}", Uuid::new_v4().simple());
        sqlx::query(AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&admin)
            .await
            .expect("create test database");
        admin.close().await;
        let url = with_database(admin_url, &name);
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(&url)
            .await
            .expect("connect to test database");
        Some(Self {
            pool,
            url,
            name,
            admin_url: admin_url.to_owned(),
        })
    }

    /// Drops the database (best effort).
    pub async fn cleanup(self) {
        self.pool.close().await;
        if let Ok(admin) = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
        {
            let _ = sqlx::query(AssertSqlSafe(format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                self.name
            )))
            .execute(&admin)
            .await;
        }
    }
}

pub struct TestRedis {
    pub url: String,
    pub client: redis::Client,
    pub conn: ConnectionManager,
    pub keys: Keys,
}

impl TestRedis {
    pub async fn connect() -> Option<Self> {
        let url = std::env::var("AIONIX_TEST_REDIS_URL")
            .unwrap_or_else(|_| DEFAULT_TEST_REDIS_URL.to_owned());
        match tokio::time::timeout(Duration::from_secs(5), redis_conn::connect(&url, 1)).await {
            Ok(Ok((client, conn))) => Some(Self {
                url,
                client,
                conn,
                keys: Keys::new(format!("test-{}", Uuid::new_v4().simple())),
            }),
            Ok(Err(e)) => {
                skip("Redis", &e.to_string());
                None
            }
            Err(_) => {
                skip("Redis", "connection timed out");
                None
            }
        }
    }
}

/// A running `aionix-api` on 127.0.0.1 backed by a fresh database and Redis prefix.
pub struct TestApi {
    pub base: String,
    pub addr: SocketAddr,
    pub state: AppState,
    pub health: Health,
    pub db: TestDb,
    pub redis: TestRedis,
    task: JoinHandle<()>,
}

impl TestApi {
    pub async fn start() -> Option<Self> {
        Self::start_with(ApiSettings {
            offer_timeout: Duration::from_secs(5),
            relay_urls: vec!["https://relay.aionix.test/".into()],
            ..ApiSettings::default()
        })
        .await
    }

    pub async fn start_with(settings: ApiSettings) -> Option<Self> {
        let db = TestDb::create().await?;
        let Some(redis) = TestRedis::connect().await else {
            db.cleanup().await;
            return None;
        };
        let tokens = Tokens::new(IdentityKey::generate(), vec![]);
        let (state, health) = server::state_from_parts(
            db.pool.clone(),
            redis.client.clone(),
            redis.conn.clone(),
            redis.keys.clone(),
            tokens,
            settings,
            TrustedProxies::default(),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = server::app(state.clone(), health.clone());
        let task = tokio::spawn(async move {
            let _ = server::serve(listener, app, std::future::pending()).await;
        });
        // Wait until the bus subscription is active (readiness).
        for _ in 0..50 {
            if health.readiness().await.ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Some(Self {
            base: format!("http://{addr}"),
            addr,
            state,
            health,
            db,
            redis,
            task,
        })
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    pub async fn stop(self) {
        self.task.abort();
        self.db.cleanup().await;
    }
}

pub const TEST_PASSWORD: &str = "correct horse battery staple";

/// Creates an organization and a member with `role`; returns `(org, user, email)`.
pub async fn org_member(db: &PgPool, role: OrgRole) -> (Uuid, Uuid, String) {
    let org = crate::admin::create_org(db, "Suporte Exemplo")
        .await
        .unwrap();
    let (user, email) = member_of(db, org, role).await;
    (org, user, email)
}

/// Adds a new account with [`TEST_PASSWORD`] to `org`.
pub async fn member_of(db: &PgPool, org: Uuid, role: OrgRole) -> (Uuid, String) {
    let email = format!("tec-{}@example.test", Uuid::new_v4().simple());
    let user = crate::admin::create_user(db, &email, "Técnica Ana", TEST_PASSWORD.into())
        .await
        .unwrap();
    crate::admin::set_membership(db, org, user, role)
        .await
        .unwrap();
    (user, email)
}
