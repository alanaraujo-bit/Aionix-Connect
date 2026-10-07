//! Adaptive Quality Controller.
//!
//! Runs on the host. Every tick it receives a [`NetworkSample`] (transport statistics,
//! frame delivery feedback, encoder timing and viewer reports) and decides the encoded
//! resolution, frame rate and bitrate.
//!
//! Priorities, in order: input responsiveness, latency, stability, frame rate, image
//! quality. Concretely:
//! * Queueing is the enemy. Any sign of a growing queue (RTT inflation, frames waiting
//!   for acknowledgement, packet loss, a slow decoder) cuts the bitrate multiplicatively
//!   right away.
//! * Increases are additive and cautious, and only happen after a calm period.
//! * Resolution/fps follow a ladder derived from the bitrate, with hysteresis so the
//!   picture does not oscillate between sizes.

use std::time::{Duration, Instant};

use aionix_shared::QualityMode;

/// One rung of the quality ladder.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tier {
    /// Maximum encoded pixels (the source is never upscaled).
    pub max_pixels: u64,
    pub fps: u32,
    /// Bitrate range in which this tier is used.
    pub min_kbps: u32,
    pub max_kbps: u32,
}

const fn tier(w: u64, h: u64, fps: u32, min_kbps: u32, max_kbps: u32) -> Tier {
    Tier {
        max_pixels: w * h,
        fps,
        min_kbps,
        max_kbps,
    }
}

/// Default ladder, best first.
const BALANCED: &[Tier] = &[
    tier(2560, 1440, 60, 9_000, 30_000),
    tier(1920, 1080, 60, 5_000, 16_000),
    tier(1920, 1080, 30, 3_000, 9_000),
    tier(1600, 900, 30, 1_800, 5_000),
    tier(1280, 720, 30, 1_000, 3_000),
    tier(960, 540, 24, 500, 1_600),
    tier(640, 360, 15, 0, 800),
];

/// Responsiveness first: high frame rate, smaller pictures.
const SPEED: &[Tier] = &[
    tier(1280, 720, 60, 2_500, 6_000),
    tier(960, 540, 60, 1_200, 3_000),
    tier(960, 540, 30, 600, 1_600),
    tier(640, 360, 30, 0, 900),
];

/// Sharpness first: high resolution, lower frame rate.
const QUALITY: &[Tier] = &[
    tier(3840, 2160, 30, 10_000, 40_000),
    tier(2560, 1440, 30, 5_000, 20_000),
    tier(1920, 1080, 24, 2_500, 9_000),
    tier(1600, 900, 20, 1_200, 4_000),
    tier(1280, 720, 15, 0, 2_000),
];

/// Native resolution always; only frame rate and bitrate adapt.
const ORIGINAL: &[Tier] = &[
    tier(u32::MAX as u64, 1, 60, 8_000, 40_000),
    tier(u32::MAX as u64, 1, 30, 3_000, 16_000),
    tier(u32::MAX as u64, 1, 15, 1_000, 8_000),
    tier(u32::MAX as u64, 1, 8, 0, 3_000),
];

fn ladder(mode: QualityMode) -> &'static [Tier] {
    match mode {
        QualityMode::Automatic | QualityMode::Balanced => BALANCED,
        QualityMode::PreferSpeed => SPEED,
        QualityMode::PreferQuality => QUALITY,
        QualityMode::Original => ORIGINAL,
    }
}

/// Observations gathered since the previous tick.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NetworkSample {
    /// Smoothed round-trip time of the active path.
    pub rtt_ms: f64,
    /// Packets lost / packets sent during the interval.
    pub loss_ratio: f64,
    /// Encoded frames sent but not yet acknowledged.
    pub in_flight_frames: u32,
    /// Age of the oldest unacknowledged frame.
    pub oldest_in_flight_ms: f64,
    /// Bitrate actually produced by the encoder.
    pub sent_kbps: f64,
    /// Average encoder latency.
    pub encode_ms: f64,
    /// Viewer: average decode latency.
    pub decode_ms: f64,
    /// Viewer: frames waiting to be decoded/rendered.
    pub decode_queue: u32,
    /// Viewer: frames dropped since the previous report.
    pub viewer_dropped: u32,
}

/// What the media pipeline should do now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decision {
    pub bitrate_kbps: u32,
    pub fps: u32,
    pub max_pixels: u64,
    /// Frames allowed in flight before the pipeline skips frames (backpressure).
    pub max_in_flight: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    Good,
    Congested,
    ReceiverLimited,
    EncoderLimited,
}

pub struct AdaptiveController {
    mode: QualityMode,
    tiers: &'static [Tier],
    tier: usize,
    bitrate: f64,
    min_rtt: f64,
    min_rtt_at: Instant,
    last_tick: Option<Instant>,
    last_decrease: Option<Instant>,
    hold_increase_until: Option<Instant>,
    upgrade_since: Option<Instant>,
    last_condition: Condition,
}

/// How long the RTT floor estimate is trusted before it may rise again (route change).
const MIN_RTT_WINDOW: Duration = Duration::from_secs(10);
/// Minimum calm period after a decrease before increasing again.
const HOLD_AFTER_DECREASE: Duration = Duration::from_secs(3);
/// A higher tier must be sustainable for this long before switching up.
const UPGRADE_DWELL: Duration = Duration::from_secs(3);
/// Multiplicative decrease factor.
const DECREASE: f64 = 0.7;
/// Additive increase: fraction of current bitrate per second (with a floor).
const INCREASE_PER_SEC: f64 = 0.10;
const INCREASE_FLOOR_KBPS: f64 = 150.0;
const MIN_KBPS: f64 = 150.0;
/// Hysteresis margin when moving up the ladder.
const UPGRADE_MARGIN: f64 = 1.2;

impl AdaptiveController {
    /// `initial_rtt_ms` comes from the transport at session start (LAN sessions start
    /// at a higher rung).
    pub fn new(mode: QualityMode, initial_rtt_ms: f64, now: Instant) -> Self {
        let tiers = ladder(mode);
        let start_tier = if initial_rtt_ms > 0.0 && initial_rtt_ms < 10.0 {
            1
        } else {
            2
        }
        .min(tiers.len() - 1);
        let t = tiers[start_tier];
        Self {
            mode,
            tiers,
            tier: start_tier,
            bitrate: f64::from(t.min_kbps.max(800) + t.max_kbps) / 2.0,
            min_rtt: if initial_rtt_ms > 0.0 {
                initial_rtt_ms
            } else {
                f64::MAX
            },
            min_rtt_at: now,
            last_tick: None,
            last_decrease: None,
            hold_increase_until: None,
            upgrade_since: None,
            last_condition: Condition::Good,
        }
    }

    pub fn mode(&self) -> QualityMode {
        self.mode
    }

    pub fn condition(&self) -> Condition {
        self.last_condition
    }

    /// Switches the user preference. Keeps the current bitrate as the starting point.
    pub fn set_mode(&mut self, mode: QualityMode) {
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        self.tiers = ladder(mode);
        self.tier = self.tier_for(self.bitrate);
        self.upgrade_since = None;
    }

    pub fn decision(&self) -> Decision {
        let t = self.tiers[self.tier];
        Decision {
            bitrate_kbps: self.bitrate.round() as u32,
            fps: t.fps,
            max_pixels: t.max_pixels,
            // About 150 ms of frames, never fewer than 2.
            max_in_flight: (t.fps * 150 / 1000).max(2),
        }
    }

    fn tier_for(&self, kbps: f64) -> usize {
        self.tiers
            .iter()
            .position(|t| kbps >= f64::from(t.min_kbps))
            .unwrap_or(self.tiers.len() - 1)
    }

    fn classify(&self, s: &NetworkSample) -> Condition {
        let fps = f64::from(self.tiers[self.tier].fps);
        let frame_ms = 1000.0 / fps;
        let queueing = s.rtt_ms > 0.0
            && self.min_rtt < f64::MAX
            && s.rtt_ms > self.min_rtt + (self.min_rtt * 0.5).max(40.0);
        let backlog = s.oldest_in_flight_ms > (2.0 * s.rtt_ms).max(150.0) + 100.0
            || f64::from(s.in_flight_frames) > (fps * 0.25).max(3.0) + 2.0;
        if s.loss_ratio > 0.02 || queueing || backlog {
            return Condition::Congested;
        }
        if s.decode_queue > 3 || s.viewer_dropped > 2 || s.decode_ms > frame_ms * 1.5 {
            return Condition::ReceiverLimited;
        }
        if s.encode_ms > frame_ms * 0.9 {
            return Condition::EncoderLimited;
        }
        Condition::Good
    }

    /// Feeds one observation interval and returns the updated decision.
    pub fn update(&mut self, now: Instant, s: &NetworkSample) -> Decision {
        let dt = self
            .last_tick
            .map(|t| now.duration_since(t).as_secs_f64())
            .unwrap_or(0.0)
            .min(2.0);
        self.last_tick = Some(now);

        if s.rtt_ms > 0.0 {
            if s.rtt_ms < self.min_rtt || now.duration_since(self.min_rtt_at) > MIN_RTT_WINDOW {
                self.min_rtt = s.rtt_ms;
                self.min_rtt_at = now;
            } else if (s.rtt_ms - self.min_rtt).abs() < 2.0 {
                self.min_rtt_at = now;
            }
        }

        let condition = self.classify(s);
        self.last_condition = condition;
        let max_kbps = f64::from(self.tiers[0].max_kbps);
        match condition {
            Condition::Congested | Condition::ReceiverLimited | Condition::EncoderLimited => {
                // At most one decrease per ~2 RTT so a single queue is not punished twice.
                let spacing = Duration::from_millis((2.0 * s.rtt_ms).clamp(200.0, 1000.0) as u64);
                if self
                    .last_decrease
                    .is_none_or(|t| now.duration_since(t) >= spacing)
                {
                    let base = if condition == Condition::Congested && s.sent_kbps > 0.0 {
                        // Never reduce below what is demonstrably getting through.
                        self.bitrate.min(s.sent_kbps.max(self.bitrate * 0.5))
                    } else {
                        self.bitrate
                    };
                    self.bitrate = (base * DECREASE).max(MIN_KBPS);
                    self.last_decrease = Some(now);
                    self.hold_increase_until = Some(now + HOLD_AFTER_DECREASE);
                    // Receiver/encoder limits are about frame cost, not bandwidth:
                    // step down one rung immediately.
                    if condition != Condition::Congested && self.tier + 1 < self.tiers.len() {
                        self.tier += 1;
                    }
                }
                self.upgrade_since = None;
            }
            Condition::Good => {
                let held = self.hold_increase_until.is_some_and(|t| now < t);
                if !held && dt > 0.0 {
                    let step = (self.bitrate * INCREASE_PER_SEC).max(INCREASE_FLOOR_KBPS) * dt;
                    self.bitrate = (self.bitrate + step).min(max_kbps);
                }
            }
        }

        // Ladder position from bitrate, with hysteresis upward.
        let target = self.tier_for(self.bitrate);
        if target > self.tier {
            self.tier = target;
            self.upgrade_since = None;
        } else if target < self.tier {
            let next_up = self.tier - 1;
            let sustained =
                self.bitrate >= f64::from(self.tiers[next_up].min_kbps) * UPGRADE_MARGIN;
            if sustained && condition == Condition::Good {
                let since = *self.upgrade_since.get_or_insert(now);
                if now.duration_since(since) >= UPGRADE_DWELL {
                    self.tier = next_up;
                    self.upgrade_since = None;
                }
            } else {
                self.upgrade_since = None;
            }
        }
        // Keep the bitrate inside the current tier's useful range.
        let t = self.tiers[self.tier];
        self.bitrate = self.bitrate.min(f64::from(t.max_kbps)).max(MIN_KBPS);
        self.decision()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good(rtt: f64) -> NetworkSample {
        NetworkSample {
            rtt_ms: rtt,
            sent_kbps: 2000.0,
            encode_ms: 4.0,
            decode_ms: 3.0,
            ..Default::default()
        }
    }

    fn run(
        c: &mut AdaptiveController,
        start: Instant,
        secs: u64,
        sample: NetworkSample,
    ) -> (Instant, Decision) {
        let mut now = start;
        let mut d = c.decision();
        for _ in 0..secs * 4 {
            now += Duration::from_millis(250);
            d = c.update(now, &sample);
        }
        (now, d)
    }

    #[test]
    fn lan_ramps_up_to_top_tier() {
        let t0 = Instant::now();
        let mut c = AdaptiveController::new(QualityMode::Automatic, 1.0, t0);
        let (_, d) = run(&mut c, t0, 60, good(1.0));
        assert_eq!(d.fps, 60);
        assert_eq!(d.max_pixels, 2560 * 1440);
        assert!(d.bitrate_kbps >= 9_000, "{d:?}");
    }

    #[test]
    fn loss_cuts_bitrate_and_resolution() {
        let t0 = Instant::now();
        let mut c = AdaptiveController::new(QualityMode::Automatic, 30.0, t0);
        let (now, before) = run(&mut c, t0, 30, good(30.0));
        let lossy = NetworkSample {
            loss_ratio: 0.08,
            ..good(30.0)
        };
        let (_, after) = run(&mut c, now, 5, lossy);
        assert!(
            after.bitrate_kbps < before.bitrate_kbps / 2,
            "{before:?} -> {after:?}"
        );
        assert!(after.max_pixels < before.max_pixels || after.fps < before.fps);
        assert_eq!(c.condition(), Condition::Congested);
    }

    #[test]
    fn rtt_inflation_is_treated_as_congestion() {
        let t0 = Instant::now();
        let mut c = AdaptiveController::new(QualityMode::Automatic, 20.0, t0);
        let (now, before) = run(&mut c, t0, 10, good(20.0));
        let (_, after) = run(&mut c, now, 2, good(150.0));
        assert!(after.bitrate_kbps < before.bitrate_kbps);
    }

    #[test]
    fn recovers_after_congestion_clears() {
        let t0 = Instant::now();
        let mut c = AdaptiveController::new(QualityMode::Automatic, 20.0, t0);
        let (now, _) = run(
            &mut c,
            t0,
            5,
            NetworkSample {
                loss_ratio: 0.2,
                ..good(20.0)
            },
        );
        let low = c.decision();
        let (_, high) = run(&mut c, now, 40, good(20.0));
        assert!(
            high.bitrate_kbps > low.bitrate_kbps * 2,
            "{low:?} -> {high:?}"
        );
    }

    #[test]
    fn backlog_of_unacknowledged_frames_triggers_decrease() {
        let t0 = Instant::now();
        let mut c = AdaptiveController::new(QualityMode::Automatic, 10.0, t0);
        let (now, before) = run(&mut c, t0, 10, good(10.0));
        let backlog = NetworkSample {
            in_flight_frames: 30,
            oldest_in_flight_ms: 600.0,
            ..good(10.0)
        };
        let (_, after) = run(&mut c, now, 1, backlog);
        assert!(after.bitrate_kbps < before.bitrate_kbps);
    }

    #[test]
    fn slow_decoder_steps_down_the_ladder() {
        let t0 = Instant::now();
        let mut c = AdaptiveController::new(QualityMode::Automatic, 5.0, t0);
        let (now, before) = run(&mut c, t0, 20, good(5.0));
        let slow = NetworkSample {
            decode_ms: 60.0,
            decode_queue: 6,
            ..good(5.0)
        };
        let (_, after) = run(&mut c, now, 2, slow);
        assert!(after.fps < before.fps || after.max_pixels < before.max_pixels);
        assert_eq!(c.condition(), Condition::ReceiverLimited);
    }

    #[test]
    fn original_mode_never_reduces_resolution() {
        let t0 = Instant::now();
        let mut c = AdaptiveController::new(QualityMode::Original, 50.0, t0);
        let (_, d) = run(
            &mut c,
            t0,
            10,
            NetworkSample {
                loss_ratio: 0.3,
                ..good(50.0)
            },
        );
        assert_eq!(d.max_pixels, u64::from(u32::MAX));
        assert!(d.fps <= 15);
    }

    #[test]
    fn upgrades_require_dwell_time() {
        let t0 = Instant::now();
        let mut c = AdaptiveController::new(QualityMode::Automatic, 30.0, t0);
        let start = c.decision();
        let (_, d) = run(&mut c, t0, 1, good(30.0));
        assert_eq!(d.max_pixels, start.max_pixels, "no instant jumps");
    }

    #[test]
    fn mode_switch_keeps_bitrate_and_picks_ladder() {
        let t0 = Instant::now();
        let mut c = AdaptiveController::new(QualityMode::Automatic, 5.0, t0);
        c.set_mode(QualityMode::PreferSpeed);
        let d = c.decision();
        assert!(d.fps >= 30);
        assert!(d.max_pixels <= 1280 * 720);
    }
}
