//! Environment configuration.
//!
//! Every server reads its settings from environment variables (populated by Docker
//! Compose from `.env` files). [`Env`] collects *all* problems before failing so an
//! operator sees the complete list at once. Values of variables marked secret are never
//! included in error messages or logs.

use std::{fmt, str::FromStr, time::Duration};

/// One configuration problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigProblem {
    pub variable: String,
    pub problem: String,
}

/// All problems found while loading a configuration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct ConfigError(pub Vec<ConfigProblem>);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "invalid configuration ({} problem(s)):", self.0.len())?;
        for p in &self.0 {
            writeln!(f, "  - {}: {}", p.variable, p.problem)?;
        }
        Ok(())
    }
}

/// Source of variables. The default reads the process environment; tests pass a map.
pub trait VarSource {
    fn get(&self, name: &str) -> Option<String>;
}

pub struct ProcessEnv;

impl VarSource for ProcessEnv {
    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

impl VarSource for std::collections::HashMap<String, String> {
    fn get(&self, name: &str) -> Option<String> {
        self.get(name).cloned()
    }
}

/// Reads variables and accumulates problems.
pub struct Env<'a> {
    source: &'a dyn VarSource,
    problems: Vec<ConfigProblem>,
}

impl<'a> Env<'a> {
    pub fn new(source: &'a dyn VarSource) -> Self {
        Self {
            source,
            problems: Vec::new(),
        }
    }

    fn raw(&self, name: &str) -> Option<String> {
        self.source
            .get(name)
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    }

    pub fn problem(&mut self, name: &str, problem: impl Into<String>) {
        self.problems.push(ConfigProblem {
            variable: name.to_owned(),
            problem: problem.into(),
        });
    }

    /// Required string. Records "not set" when missing.
    pub fn required(&mut self, name: &str) -> String {
        match self.raw(name) {
            Some(v) => v,
            None => {
                self.problem(name, "required but not set");
                String::new()
            }
        }
    }

    /// Required secret: like [`Env::required`], and rejects well-known placeholders
    /// copied from `.env.example`.
    pub fn required_secret(&mut self, name: &str, min_len: usize) -> String {
        let v = self.required(name);
        if v.is_empty() {
            return v;
        }
        let lower = v.to_ascii_lowercase();
        if lower.contains("change-me") || lower.contains("changeme") || lower == "password" {
            self.problem(
                name,
                "still contains the placeholder value from .env.example",
            );
        } else if v.len() < min_len {
            self.problem(name, format!("must be at least {min_len} characters"));
        }
        v
    }

    pub fn optional(&self, name: &str) -> Option<String> {
        self.raw(name)
    }

    pub fn string_or(&self, name: &str, default: &str) -> String {
        self.raw(name).unwrap_or_else(|| default.to_owned())
    }

    /// Parsed value with a default. Invalid values are recorded (the value itself is
    /// shown, so do not use this for secrets).
    pub fn parse_or<T: FromStr>(&mut self, name: &str, default: T) -> T
    where
        T::Err: fmt::Display,
    {
        match self.raw(name) {
            None => default,
            Some(v) => match v.parse() {
                Ok(t) => t,
                Err(e) => {
                    self.problem(name, format!("invalid value {v:?}: {e}"));
                    default
                }
            },
        }
    }

    pub fn required_parse<T: FromStr>(&mut self, name: &str) -> Option<T>
    where
        T::Err: fmt::Display,
    {
        let v = self.required(name);
        if v.is_empty() {
            return None;
        }
        match v.parse() {
            Ok(t) => Some(t),
            Err(e) => {
                self.problem(name, format!("invalid value {v:?}: {e}"));
                None
            }
        }
    }

    pub fn bool_or(&mut self, name: &str, default: bool) -> bool {
        match self.raw(name).map(|v| v.to_ascii_lowercase()) {
            None => default,
            Some(v) if matches!(v.as_str(), "1" | "true" | "yes" | "on") => true,
            Some(v) if matches!(v.as_str(), "0" | "false" | "no" | "off") => false,
            Some(v) => {
                self.problem(name, format!("expected true/false, got {v:?}"));
                default
            }
        }
    }

    /// Seconds with a default.
    pub fn secs_or(&mut self, name: &str, default: u64) -> Duration {
        Duration::from_secs(self.parse_or(name, default))
    }

    /// Comma separated list (empty entries dropped).
    pub fn list(&self, name: &str) -> Vec<String> {
        self.raw(name)
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Fails with every problem recorded so far.
    pub fn finish(self) -> Result<(), ConfigError> {
        if self.problems.is_empty() {
            Ok(())
        } else {
            Err(ConfigError(self.problems))
        }
    }
}

/// Validates a `postgres://` URL without echoing its password.
pub fn check_database_url(env: &mut Env<'_>, name: &str, url: &str) {
    if url.is_empty() {
        return;
    }
    if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
        env.problem(name, "must start with postgres:// or postgresql://");
    }
    if url.to_ascii_lowercase().contains("change-me") {
        env.problem(name, "contains the placeholder password from .env.example");
    }
}

pub fn check_redis_url(env: &mut Env<'_>, name: &str, url: &str) {
    if url.is_empty() {
        return;
    }
    if !(url.starts_with("redis://") || url.starts_with("rediss://")) {
        env.problem(name, "must start with redis:// or rediss://");
    }
    if url.to_ascii_lowercase().contains("change-me") {
        env.problem(name, "contains the placeholder password from .env.example");
    }
}

/// Removes the password from a connection URL for logging.
pub fn redact_url(url: &str) -> String {
    match (url.find("://"), url.rfind('@')) {
        (Some(scheme_end), Some(at)) if at > scheme_end => {
            let creds = &url[scheme_end + 3..at];
            let user = creds.split(':').next().unwrap_or("");
            format!("{}{}:***{}", &url[..scheme_end + 3], user, &url[at..])
        }
        _ => url.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn collects_every_problem_without_leaking_secrets() {
        let src = vars(&[
            ("PORT", "abc"),
            ("SECRET", "change-me-please"),
            ("FLAG", "maybe"),
        ]);
        let mut env = Env::new(&src);
        let _ = env.required("MISSING");
        let _: u16 = env.parse_or("PORT", 8080);
        let s = env.required_secret("SECRET", 8);
        assert!(!s.is_empty());
        let _ = env.bool_or("FLAG", false);
        let err = env.finish().unwrap_err();
        assert_eq!(err.0.len(), 4);
        let text = err.to_string();
        assert!(text.contains("MISSING"));
        assert!(text.contains("placeholder"));
        assert!(
            !text.contains("change-me-please"),
            "secret value leaked: {text}"
        );
    }

    #[test]
    fn defaults_and_lists() {
        let src = vars(&[("LIST", "a, b,,c"), ("ON", "true"), ("SHORT", "x")]);
        let mut env = Env::new(&src);
        assert_eq!(env.list("LIST"), ["a", "b", "c"]);
        assert!(env.bool_or("ON", false));
        assert_eq!(env.parse_or("NOPE", 7u32), 7);
        let _ = env.required_secret("SHORT", 16);
        assert_eq!(env.finish().unwrap_err().0.len(), 1);
    }

    #[test]
    fn redacts_passwords() {
        assert_eq!(
            redact_url("postgres://aionix:s3cret@db:5432/aionix"),
            "postgres://aionix:***@db:5432/aionix"
        );
        assert_eq!(redact_url("redis://redis:6379"), "redis://redis:6379");
    }
}
