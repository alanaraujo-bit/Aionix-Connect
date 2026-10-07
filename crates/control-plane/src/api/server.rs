use aionix_control_api::rest::ServerInfo;
use axum::{Json, extract::State};
use time::OffsetDateTime;

use crate::state::AppState;

/// `GET /v1/server/info` (public): the keys devices pin to verify tokens and session
/// tickets, and the relays they may use.
pub async fn info(State(state): State<AppState>) -> Json<ServerInfo> {
    Json(ServerInfo {
        name: state.settings.server_name.clone(),
        version: aionix_server_common::VERSION.to_owned(),
        token_keys: state.tokens.public_keys().to_vec(),
        relay_urls: state.settings.relay_urls.clone(),
        server_time: OffsetDateTime::now_utc(),
    })
}
