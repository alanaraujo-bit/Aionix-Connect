use aionix_control_api::signal::EndpointAddrDto;
use aionix_shared::TransportKind;
use iroh::{EndpointAddr, RelayUrl, TransportAddr};
use std::net::{IpAddr, SocketAddr};

use crate::TransportError;

/// Converts a backend address into the transport-agnostic DTO used by signaling.
pub fn to_dto(addr: &EndpointAddr) -> EndpointAddrDto {
    let mut dto = EndpointAddrDto::default();
    for a in &addr.addrs {
        match a {
            TransportAddr::Relay(url) => dto.relay_urls.push(url.to_string()),
            TransportAddr::Ip(sock) => dto.direct_addrs.push(sock.to_string()),
            _ => {}
        }
    }
    dto
}

pub(crate) fn from_dto(
    id: iroh::EndpointId,
    dto: &EndpointAddrDto,
) -> Result<EndpointAddr, TransportError> {
    let mut addrs = Vec::new();
    for url in &dto.relay_urls {
        let url: RelayUrl = url
            .parse()
            .map_err(|_| TransportError::InvalidAddress(format!("relay url {url}")))?;
        addrs.push(TransportAddr::Relay(url));
    }
    for sock in &dto.direct_addrs {
        let sock: SocketAddr = sock
            .parse()
            .map_err(|_| TransportError::InvalidAddress(format!("socket {sock}")))?;
        addrs.push(TransportAddr::Ip(sock));
    }
    Ok(EndpointAddr::from_parts(id, addrs))
}

/// Classifies a direct path by the peer's IP: private/link-local ranges are LAN.
pub fn classify_ip(ip: IpAddr) -> TransportKind {
    let lan = match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local() || v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_private() || v4.is_link_local())
        }
    };
    if lan { TransportKind::Lan } else { TransportKind::Direct }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_private_ranges_as_lan() {
        for ip in ["192.168.0.10", "10.1.2.3", "172.16.5.4", "169.254.1.1", "127.0.0.1", "fd00::1", "fe80::1"] {
            assert_eq!(classify_ip(ip.parse().unwrap()), TransportKind::Lan, "{ip}");
        }
        // CGNAT shared space and public addresses are not LAN.
        for ip in ["100.64.1.1", "8.8.8.8", "2001:4860::8888"] {
            assert_eq!(classify_ip(ip.parse().unwrap()), TransportKind::Direct, "{ip}");
        }
    }

    #[test]
    fn dto_roundtrip() {
        let key = iroh::SecretKey::from_bytes(&[7u8; 32]).public();
        let dto = EndpointAddrDto {
            relay_urls: vec!["https://relay.example.com./".into()],
            direct_addrs: vec!["192.168.1.5:4433".into(), "[2001:db8::1]:4433".into()],
        };
        let addr = from_dto(key, &dto).unwrap();
        let back = to_dto(&addr);
        assert_eq!(back.direct_addrs.len(), 2);
        assert_eq!(back.relay_urls.len(), 1);
        assert!(from_dto(key, &EndpointAddrDto { relay_urls: vec![], direct_addrs: vec!["nope".into()] }).is_err());
    }
}
