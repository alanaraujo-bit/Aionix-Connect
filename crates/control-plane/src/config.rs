//! `aionix-api` configuration (environment variables; see `infra/env/.env.example`).

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use aionix_crypto::PublicKey;
use aionix_server_common::{
    config::{ConfigError, Env, VarSource, check_database_url, check_redis_url},
    http::TrustedProxies,
    keys::parse_public_keys,
};

pub const DEFAULT_TRUSTED_PROXIES: &str =
    "127.0.0.1/32,::1/128,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16";

/// Settings used by request handlers.
#[derive(Debug, Clone)]
pub struct ApiSettings {
    pub server_name: String,
    /// Public relay URLs handed to devices (with per-device relay grants).
    pub relay_urls: Vec<String>,
    /// How long `POST /v1/sessions` waits for the host to answer the offer.
    pub offer_timeout: Duration,
    /// Whether temporary quick-support executables may register without an
    /// enrollment token (rate limited; they still need visible approval per session).
    pub allow_quick_support: bool,
}

impl Default for ApiSettings {
    fn default() -> Self {
        Self {
            server_name: "Aionix Connect".into(),
            relay_urls: Vec::new(),
            offer_timeout: Duration::from_secs(15),
            allow_quick_support: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApiConfig {
    pub bind: SocketAddr,
    pub database_url: String,
    pub db_max_connections: u32,
    pub redis_url: String,
    pub redis_prefix: String,
    pub signing_key_file: PathBuf,
    pub previous_public_keys: Vec<PublicKey>,
    pub trusted_proxies: TrustedProxies,
    pub settings: ApiSettings,
}

/// Database + Redis settings shared by every service.
pub struct StoreConfig {
    pub database_url: String,
    pub db_max_connections: u32,
    pub redis_url: String,
    pub redis_prefix: String,
}

pub fn store_config(env: &mut Env<'_>) -> StoreConfig {
    let database_url = env.required("AIONIX_DATABASE_URL");
    check_database_url(env, "AIONIX_DATABASE_URL", &database_url);
    let redis_url = env.required("AIONIX_REDIS_URL");
    check_redis_url(env, "AIONIX_REDIS_URL", &redis_url);
    let redis_prefix = env.string_or("AIONIX_REDIS_PREFIX", "aionix");
    if !redis_prefix
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        env.problem("AIONIX_REDIS_PREFIX", "only letters, digits, '-' and '_'");
    }
    StoreConfig {
        database_url,
        db_max_connections: env.parse_or("AIONIX_DB_MAX_CONNECTIONS", 20),
        redis_url,
        redis_prefix,
    }
}

pub fn trusted_proxies(env: &mut Env<'_>) -> TrustedProxies {
    let mut list = env.list("AIONIX_TRUSTED_PROXIES");
    if list.is_empty() {
        list = DEFAULT_TRUSTED_PROXIES
            .split(',')
            .map(str::to_owned)
            .collect();
    }
    match TrustedProxies::parse(&list) {
        Ok(p) => p,
        Err(e) => {
            env.problem("AIONIX_TRUSTED_PROXIES", e);
            TrustedProxies::default()
        }
    }
}

impl ApiConfig {
    pub fn from_env(src: &dyn VarSource) -> Result<Self, ConfigError> {
        let mut env = Env::new(src);
        let store = store_config(&mut env);
        let bind = env.parse_or("AIONIX_API_BIND", "0.0.0.0:8080".parse().unwrap());
        let key_file = env.required("AIONIX_TOKEN_SIGNING_KEY_FILE");
        let previous = match parse_public_keys(&env.list("AIONIX_TOKEN_PREVIOUS_PUBLIC_KEYS")) {
            Ok(k) => k,
            Err(e) => {
                env.problem("AIONIX_TOKEN_PREVIOUS_PUBLIC_KEYS", e.to_string());
                Vec::new()
            }
        };
        let relay_urls = env.list("AIONIX_RELAY_URLS");
        for u in &relay_urls {
            if !(u.starts_with("https://") || u.starts_with("http://")) {
                env.problem("AIONIX_RELAY_URLS", format!("{u:?} is not an http(s) URL"));
            }
        }
        let offer_timeout = env.secs_or("AIONIX_OFFER_TIMEOUT_SECS", 15);
        let allow_quick_support = env.bool_or("AIONIX_ALLOW_QUICK_SUPPORT", true);
        let server_name = env.string_or("AIONIX_SERVER_NAME", "Aionix Connect");
        let trusted_proxies = trusted_proxies(&mut env);
        env.finish()?;
        Ok(Self {
            bind,
            database_url: store.database_url,
            db_max_connections: store.db_max_connections,
            redis_url: store.redis_url,
            redis_prefix: store.redis_prefix,
            signing_key_file: PathBuf::from(key_file),
            previous_public_keys: previous,
            trusted_proxies,
            settings: ApiSettings {
                server_name,
                relay_urls,
                offer_timeout,
                allow_quick_support,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn reports_all_missing_variables_and_bad_urls() {
        let src: HashMap<String, String> = [
            ("AIONIX_DATABASE_URL", "mysql://x"),
            ("AIONIX_RELAY_URLS", "relay.example.com"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        let err = ApiConfig::from_env(&src).unwrap_err();
        let vars: Vec<_> = err.0.iter().map(|p| p.variable.as_str()).collect();
        assert!(vars.contains(&"AIONIX_DATABASE_URL"));
        assert!(vars.contains(&"AIONIX_REDIS_URL"));
        assert!(vars.contains(&"AIONIX_TOKEN_SIGNING_KEY_FILE"));
        assert!(vars.contains(&"AIONIX_RELAY_URLS"));
    }

    #[test]
    fn minimal_valid_configuration() {
        let src: HashMap<String, String> = [
            ("AIONIX_DATABASE_URL", "postgres://a:b@db/aionix"),
            ("AIONIX_REDIS_URL", "redis://redis:6379"),
            ("AIONIX_TOKEN_SIGNING_KEY_FILE", "/run/secrets/token-signing.key"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        let c = ApiConfig::from_env(&src).unwrap();
        assert_eq!(c.bind.port(), 8080);
        assert_eq!(c.redis_prefix, "aionix");
        assert!(c.settings.allow_quick_support);
    }
}
