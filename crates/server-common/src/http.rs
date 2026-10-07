//! HTTP plumbing shared by the API, signaling and relay status servers.

use std::net::{IpAddr, SocketAddr};

use axum::{Router, http::Request};
use http::HeaderMap;
use ipnet::IpNet;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::{DefaultOnFailure, DefaultOnResponse, TraceLayer},
};
use tracing::Level;

pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// Adds request ids (generated unless a trusted proxy sent one) and a tracing span per
/// request carrying `request_id`, method and path. Query strings and headers are not
/// logged: they can carry tokens.
pub fn with_request_tracing<S: Clone + Send + Sync + 'static>(router: Router<S>) -> Router<S> {
    router
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|req: &Request<_>| {
                    let request_id = req
                        .headers()
                        .get(REQUEST_ID_HEADER)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("-");
                    tracing::info_span!(
                        "http",
                        request_id = %request_id,
                        method = %req.method(),
                        path = %req.uri().path(),
                    )
                })
                .on_request(())
                .on_response(DefaultOnResponse::new().level(Level::INFO))
                .on_failure(DefaultOnFailure::new().level(Level::WARN)),
        )
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
}

/// Request id of the current request, for audit records.
pub fn request_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.chars().take(64).collect())
}

/// Networks whose `X-Forwarded-For` header is believed (the reverse proxy).
#[derive(Debug, Clone, Default)]
pub struct TrustedProxies(Vec<IpNet>);

impl TrustedProxies {
    pub fn parse(list: &[String]) -> Result<Self, String> {
        list.iter()
            .map(|s| {
                s.parse::<IpNet>()
                    .or_else(|_| s.parse::<IpAddr>().map(IpNet::from))
                    .map_err(|_| format!("invalid network {s:?}"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = canonical(ip);
        self.0.iter().any(|n| n.contains(&ip))
    }

    /// The client address: the peer itself, or — when the peer is a trusted proxy —
    /// the right-most address in `X-Forwarded-For` that is not a trusted proxy.
    pub fn client_ip(&self, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
        let peer_ip = canonical(peer.ip());
        if !self.contains(peer_ip) {
            return peer_ip;
        }
        let forwarded: Vec<IpAddr> = headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .filter_map(|s| s.trim().parse::<IpAddr>().ok())
            .map(canonical)
            .collect();
        forwarded
            .into_iter()
            .rev()
            .find(|ip| !self.contains(*ip))
            .unwrap_or(peer_ip)
    }
}

fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarded_for_is_only_trusted_from_proxies() {
        let trusted = TrustedProxies::parse(&["172.16.0.0/12".into(), "127.0.0.1".into()]).unwrap();
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "1.2.3.4, 203.0.113.9".parse().unwrap());
        // From the proxy: right-most untrusted entry.
        let via_proxy: SocketAddr = "172.18.0.5:5555".parse().unwrap();
        assert_eq!(
            trusted.client_ip(via_proxy, &h),
            "203.0.113.9".parse::<IpAddr>().unwrap()
        );
        // Directly from the internet: the header is ignored (spoofable).
        let direct: SocketAddr = "198.51.100.7:4444".parse().unwrap();
        assert_eq!(
            trusted.client_ip(direct, &h),
            "198.51.100.7".parse::<IpAddr>().unwrap()
        );
        assert!(TrustedProxies::parse(&["nope".into()]).is_err());
    }
}
