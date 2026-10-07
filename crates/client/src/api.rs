//! Typed HTTP client for the Control Plane API.

use std::time::Duration;

use aionix_control_api::{
    ApiError, DEVICE_TOKEN_HEADER, ErrorCode,
    rest::{
        AuthTokens, Challenge, CreateSessionRequest, CreateSessionResponse, DeviceLoginRequest,
        DeviceLoginResponse, DeviceLookup, LoginRequest, RefreshRequest, RegisterDeviceRequest,
        RegisterDeviceResponse, ServerInfo, SessionEventReport, UserProfile,
    },
};
use aionix_shared::FriendlyId;
use reqwest::{Method, RequestBuilder};
use serde::{Serialize, de::DeserializeOwned};
use url::Url;
use uuid::Uuid;

#[derive(Debug, Clone, thiserror::Error)]
pub enum ClientError {
    #[error("server refused the request ({status}, {code:?}): {detail}")]
    Api {
        status: u16,
        code: ErrorCode,
        detail: String,
        retry_after: Option<u64>,
    },
    #[error("server unreachable: {0}")]
    Network(String),
    #[error("unexpected server response: {0}")]
    Decode(String),
    #[error("{0}")]
    Local(String),
}

impl ClientError {
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            Self::Api { code, .. } => Some(*code),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ApiClient {
    http: reqwest::Client,
    base: Url,
}

impl ApiClient {
    /// `base` is the public server URL, e.g. `https://connect.example.com`.
    pub fn new(base: &str) -> Result<Self, ClientError> {
        let base =
            Url::parse(base).map_err(|e| ClientError::Local(format!("invalid server URL: {e}")))?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(40))
            .user_agent(concat!("aionix-client/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ClientError::Local(e.to_string()))?;
        Ok(Self { http, base })
    }

    pub fn base(&self) -> &Url {
        &self.base
    }

    /// Default signaling URL: same host, `ws(s)://…/v1/signal`.
    pub fn signal_url(&self) -> String {
        let mut u = self.base.clone();
        let scheme = if u.scheme() == "https" { "wss" } else { "ws" };
        let _ = u.set_scheme(scheme);
        u.set_path("/v1/signal");
        u.to_string()
    }

    fn req(&self, method: Method, path: &str) -> RequestBuilder {
        let url = self.base.join(path).expect("static API paths are valid");
        self.http.request(method, url)
    }

    async fn call<T: DeserializeOwned>(&self, req: RequestBuilder) -> Result<T, ClientError> {
        let resp = req
            .send()
            .await
            .map_err(|e| ClientError::Network(e.to_string()))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ClientError::Network(e.to_string()))?;
        if status.is_success() {
            let body: &[u8] = if bytes.is_empty() { b"null" } else { &bytes };
            return serde_json::from_slice(body).map_err(|e| ClientError::Decode(e.to_string()));
        }
        match serde_json::from_slice::<ApiError>(&bytes) {
            Ok(e) => Err(ClientError::Api {
                status: status.as_u16(),
                code: e.code,
                detail: e.detail,
                retry_after: e.retry_after,
            }),
            Err(_) => Err(ClientError::Api {
                status: status.as_u16(),
                code: if status.is_server_error() {
                    ErrorCode::Unavailable
                } else {
                    ErrorCode::BadRequest
                },
                detail: format!("HTTP {status}"),
                retry_after: None,
            }),
        }
    }

    async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        bearer: Option<&str>,
    ) -> Result<T, ClientError> {
        let mut r = self.req(Method::POST, path).json(body);
        if let Some(b) = bearer {
            r = r.bearer_auth(b);
        }
        self.call(r).await
    }

    pub async fn server_info(&self) -> Result<ServerInfo, ClientError> {
        self.call(self.req(Method::GET, "/v1/server/info")).await
    }

    pub async fn login(&self, email: &str, password: &str) -> Result<AuthTokens, ClientError> {
        self.post(
            "/v1/auth/login",
            &LoginRequest {
                email: email.to_owned(),
                password: password.to_owned(),
            },
            None,
        )
        .await
    }

    pub async fn refresh(&self, refresh_token: &str) -> Result<AuthTokens, ClientError> {
        self.post(
            "/v1/auth/refresh",
            &RefreshRequest {
                refresh_token: refresh_token.to_owned(),
            },
            None,
        )
        .await
    }

    pub async fn logout(&self, refresh_token: &str) -> Result<(), ClientError> {
        let _: serde_json::Value = self
            .post(
                "/v1/auth/logout",
                &RefreshRequest {
                    refresh_token: refresh_token.to_owned(),
                },
                None,
            )
            .await?;
        Ok(())
    }

    pub async fn me(&self, access: &str) -> Result<UserProfile, ClientError> {
        self.call(self.req(Method::GET, "/v1/me").bearer_auth(access))
            .await
    }

    pub async fn challenge(&self) -> Result<Challenge, ClientError> {
        self.post("/v1/devices/challenge", &serde_json::json!({}), None)
            .await
    }

    pub async fn register_device(
        &self,
        req: &RegisterDeviceRequest,
        bearer: Option<&str>,
    ) -> Result<RegisterDeviceResponse, ClientError> {
        self.post("/v1/devices/register", req, bearer).await
    }

    pub async fn device_login(
        &self,
        req: &DeviceLoginRequest,
    ) -> Result<DeviceLoginResponse, ClientError> {
        self.post("/v1/devices/login", req, None).await
    }

    pub async fn lookup(&self, access: &str, f: FriendlyId) -> Result<DeviceLookup, ClientError> {
        self.call(
            self.req(Method::GET, &format!("/v1/devices/lookup/{}", f.compact()))
                .bearer_auth(access),
        )
        .await
    }

    pub async fn create_session(
        &self,
        access: &str,
        device_token: &str,
        req: &CreateSessionRequest,
    ) -> Result<CreateSessionResponse, ClientError> {
        self.call(
            self.req(Method::POST, "/v1/sessions")
                .bearer_auth(access)
                .header(DEVICE_TOKEN_HEADER, device_token)
                .json(req),
        )
        .await
    }

    pub async fn report_session_event(
        &self,
        access: &str,
        session: Uuid,
        report: &SessionEventReport,
    ) -> Result<(), ClientError> {
        let _: serde_json::Value = self
            .post(
                &format!("/v1/sessions/{session}/events"),
                report,
                Some(access),
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_url_follows_the_api_scheme() {
        assert_eq!(
            ApiClient::new("https://connect.example.com")
                .unwrap()
                .signal_url(),
            "wss://connect.example.com/v1/signal"
        );
        assert_eq!(
            ApiClient::new("http://127.0.0.1:8080/")
                .unwrap()
                .signal_url(),
            "ws://127.0.0.1:8080/v1/signal"
        );
        assert!(ApiClient::new("not a url").is_err());
    }
}
