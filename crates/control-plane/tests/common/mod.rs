//! HTTP helpers for the Control Plane integration tests.
#![allow(dead_code)]

use aionix_control_api::DEVICE_TOKEN_HEADER;
use aionix_crypto::{IdentityKey, SignatureDomain, sign_domain};
use aionix_shared::DeviceKind;
use reqwest::StatusCode;
use serde_json::{Value, json};

pub struct Http {
    pub client: reqwest::Client,
    pub base: String,
}

impl Http {
    pub fn new(base: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base: base.to_owned(),
        }
    }

    pub async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        bearer: Option<&str>,
        device_token: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut req = self
            .client
            .request(method, format!("{}{}", self.base, path));
        if let Some(b) = bearer {
            req = req.bearer_auth(b);
        }
        if let Some(t) = device_token {
            req = req.header(DEVICE_TOKEN_HEADER, t);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.expect("request");
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let value = serde_json::from_str(&text).unwrap_or(Value::Null);
        (status, value)
    }

    pub async fn post(&self, path: &str, body: Value, bearer: Option<&str>) -> (StatusCode, Value) {
        self.send(reqwest::Method::POST, path, Some(body), bearer, None)
            .await
    }

    pub async fn get(&self, path: &str, bearer: Option<&str>) -> (StatusCode, Value) {
        self.send(reqwest::Method::GET, path, None, bearer, None)
            .await
    }

    pub async fn login(&self, email: &str, password: &str) -> (StatusCode, Value) {
        self.post(
            "/v1/auth/login",
            json!({"email": email, "password": password}),
            None,
        )
        .await
    }

    /// Logs in and returns the access token.
    pub async fn access(&self, email: &str) -> String {
        let (s, v) = self
            .login(email, aionix_control_plane::testing::TEST_PASSWORD)
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["access_token"].as_str().unwrap().to_owned()
    }

    pub async fn challenge(&self) -> String {
        let (s, v) = self.post("/v1/devices/challenge", json!({}), None).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["nonce"].as_str().unwrap().to_owned()
    }

    pub async fn register(
        &self,
        key: &IdentityKey,
        kind: DeviceKind,
        enrollment_token: Option<&str>,
        bearer: Option<&str>,
    ) -> (StatusCode, Value) {
        let nonce = self.challenge().await;
        let signature = sign_domain(key, SignatureDomain::DeviceRegistration, nonce.as_bytes());
        self.post(
            "/v1/devices/register",
            json!({
                "public_key": key.public(),
                "kind": kind,
                "nonce": nonce,
                "signature": signature,
                "info": {"hostname": "PC-TESTE", "os_name": "Windows", "os_version": "11",
                         "app_version": "0.1.0"},
                "enrollment_token": enrollment_token,
            }),
            bearer,
        )
        .await
    }

    pub async fn device_login(&self, key: &IdentityKey) -> (StatusCode, Value) {
        let nonce = self.challenge().await;
        let signature = sign_domain(key, SignatureDomain::DeviceLogin, nonce.as_bytes());
        self.post(
            "/v1/devices/login",
            json!({"public_key": key.public(), "nonce": nonce, "signature": signature}),
            None,
        )
        .await
    }

    pub async fn request_session(
        &self,
        access: &str,
        device_token: &str,
        friendly_id: &str,
        permissions: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut body = json!({"target": friendly_id});
        if let Some(p) = permissions {
            body["permissions"] = p;
        }
        self.send(
            reqwest::Method::POST,
            "/v1/sessions",
            Some(body),
            Some(access),
            Some(device_token),
        )
        .await
    }
}

pub fn code(v: &Value) -> &str {
    v["code"].as_str().unwrap_or("")
}
