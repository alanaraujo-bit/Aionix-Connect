//! Session diagnostics.
//!
//! Every value is either measured ([`Metric::Available`]) or explicitly
//! [`Metric::Unavailable`] with the reason. Nothing is estimated or filled with
//! placeholders.

use std::time::Duration;

use aionix_shared::TransportKind;
use serde::Serialize;
use uuid::Uuid;

use super::state::SessionState;

/// Why a metric has no value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unavailable {
    /// No live transport (before connection, while reconnecting, after the end).
    NotConnected,
    /// The pipeline is running but has not produced a sample yet.
    NotMeasuredYet,
    /// The value is measured on the other peer, which has not reported it.
    NotReportedByPeer,
    /// Does not apply to this side of the session.
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "value")]
pub enum Metric<T> {
    Available(T),
    Unavailable(Unavailable),
}

impl<T> Metric<T> {
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Available(v) => Some(v),
            Self::Unavailable(_) => None,
        }
    }

    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available(_))
    }

    /// `Available` when `v` is `Some`, otherwise `Unavailable(reason)`.
    pub fn from_option(v: Option<T>, reason: Unavailable) -> Self {
        match v {
            Some(v) => Self::Available(v),
            None => Self::Unavailable(reason),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRole {
    Host,
    Technician,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionDiagnostics {
    pub role: SessionRole,
    pub session_id: Metric<Uuid>,
    pub state: SessionState,
    pub transport: Metric<TransportKind>,
    /// Time since the session first reached `Connected`.
    pub duration: Metric<Duration>,
    pub reconnect_count: u32,
    pub capture_backend: Metric<String>,
    pub encoder: Metric<String>,
    pub hardware_encoder: Metric<bool>,
    pub decoder: Metric<String>,
    /// Encoded stream resolution.
    pub resolution: Metric<Resolution>,
    /// Frames per second actually produced (host) or decoded (technician).
    pub fps: Metric<f64>,
    pub rtt_ms: Metric<f64>,
    /// Video bitrate actually sent (host) or received (technician).
    pub bitrate_kbps: Metric<f64>,
    /// Lost / sent QUIC packets on this side during the last interval.
    pub packet_loss: Metric<f64>,
    pub encode_latency_ms: Metric<f64>,
    pub decode_latency_ms: Metric<f64>,
    /// Frames dropped by the receiving pipeline since the session started.
    pub dropped_frames: Metric<u64>,
}

impl SessionDiagnostics {
    pub fn new(role: SessionRole) -> Self {
        use Unavailable::*;
        Self {
            role,
            session_id: Metric::Unavailable(NotMeasuredYet),
            state: SessionState::Idle,
            transport: Metric::Unavailable(NotConnected),
            duration: Metric::Unavailable(NotConnected),
            reconnect_count: 0,
            capture_backend: Metric::Unavailable(NotMeasuredYet),
            encoder: Metric::Unavailable(NotMeasuredYet),
            hardware_encoder: Metric::Unavailable(NotMeasuredYet),
            decoder: Metric::Unavailable(NotMeasuredYet),
            resolution: Metric::Unavailable(NotMeasuredYet),
            fps: Metric::Unavailable(NotMeasuredYet),
            rtt_ms: Metric::Unavailable(NotConnected),
            bitrate_kbps: Metric::Unavailable(NotMeasuredYet),
            packet_loss: Metric::Unavailable(NotConnected),
            encode_latency_ms: Metric::Unavailable(NotMeasuredYet),
            decode_latency_ms: Metric::Unavailable(NotMeasuredYet),
            dropped_frames: Metric::Unavailable(NotMeasuredYet),
        }
    }

    /// Clears values that only make sense with a live link.
    pub(crate) fn link_down(&mut self) {
        use Unavailable::NotConnected;
        self.transport = Metric::Unavailable(NotConnected);
        self.rtt_ms = Metric::Unavailable(NotConnected);
        self.packet_loss = Metric::Unavailable(NotConnected);
    }
}

/// Packet loss ratio between two cumulative transport counters.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct LossMeter {
    last: Option<(u64, u64)>,
}

impl LossMeter {
    /// Returns lost/sent over the interval, or `None` when nothing was sent.
    pub fn update(&mut self, lost: u64, sent: u64) -> Option<f64> {
        let prev = self.last.replace((lost, sent));
        let (pl, ps) = prev?;
        let (dl, ds) = (lost.saturating_sub(pl), sent.saturating_sub(ps));
        (ds > 0).then(|| dl as f64 / ds as f64)
    }

    pub fn reset(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_everything_unavailable() {
        let d = SessionDiagnostics::new(SessionRole::Technician);
        assert!(!d.rtt_ms.is_available());
        assert!(!d.encoder.is_available());
        assert_eq!(d.transport, Metric::Unavailable(Unavailable::NotConnected));
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["rtt_ms"]["status"], "unavailable");
        assert_eq!(json["rtt_ms"]["value"], "not_connected");
    }

    #[test]
    fn loss_meter_uses_deltas() {
        let mut m = LossMeter::default();
        assert_eq!(m.update(5, 100), None, "first sample has no interval");
        assert_eq!(m.update(5, 200), Some(0.0));
        assert_eq!(m.update(15, 300), Some(0.1));
        assert_eq!(m.update(15, 300), None, "nothing sent");
    }
}
