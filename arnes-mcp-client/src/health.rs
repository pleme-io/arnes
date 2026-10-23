//! Adaptive MCP health: when to probe each server next (docs/profiles.md §1).
//!
//! Pure and clocked by the caller, so it is tested at pinned instants. A
//! healthy server is probed less and less often, a failing one soon and then
//! with backoff, and one that keeps flipping is quarantined for a while. Every
//! interval carries deterministic jitter so several arnes processes do not
//! probe in step.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The knobs, each a `mcp.health.*` config key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// First interval after a server comes up, and the floor.
    pub min: Duration,
    /// The ceiling for a healthy, idle server.
    pub max: Duration,
    /// Healthy interval growth, in percent per ok probe (200 = doubling).
    pub growth_pct: u32,
    /// The first retry after a failure.
    pub fail: Duration,
    /// The ceiling of the reconnect backoff.
    pub reconnect_max: Duration,
    /// Up to this percent of each interval is added as jitter.
    pub jitter_pct: u32,
    /// This many up/down flips inside `window` quarantines the server.
    pub flaps: u32,
    pub window: Duration,
    /// How long a quarantine lasts.
    pub quarantine: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            min: Duration::from_secs(15),
            max: Duration::from_secs(300),
            growth_pct: 200,
            fail: Duration::from_secs(2),
            reconnect_max: Duration::from_secs(120),
            jitter_pct: 10,
            flaps: 4,
            window: Duration::from_secs(300),
            quarantine: Duration::from_secs(600),
        }
    }
}

/// Where a server stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not probed yet.
    Unknown,
    Up,
    Down,
    /// Flapping: not probed until the instant.
    Quarantined { until: Instant },
}

/// What an observation changed, for the caller to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    None,
    CameUp,
    WentDown,
    /// Newly quarantined; the caller raises one notice.
    Quarantined,
    /// A quarantine ended; probing resumes.
    Released,
}

/// One server's health.
#[derive(Debug, Clone)]
pub struct Health {
    server: String,
    pub state: State,
    /// The interval the next probe was scheduled with.
    pub interval: Duration,
    pub due: Instant,
    /// Consecutive probes with the same result.
    pub streak: u32,
    attempts: u64,
    flips: VecDeque<Instant>,
}

/// Deterministic jitter in `0..=pct` percent of `d`, from the server name and
/// the attempt number (FNV-1a), so it needs no random source and is testable.
#[must_use]
pub fn jitter(server: &str, attempt: u64, d: Duration, pct: u32) -> Duration {
    if pct == 0 {
        return Duration::ZERO;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in server.bytes().chain(attempt.to_le_bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    let span = d.as_millis().saturating_mul(u128::from(pct)) / 100;
    let span = u64::try_from(span).unwrap_or(u64::MAX);
    if span == 0 { Duration::ZERO } else { Duration::from_millis(h % (span + 1)) }
}

impl Health {
    /// A server seen for the first time: probe it now.
    #[must_use]
    pub fn new(server: impl Into<String>, now: Instant) -> Self {
        Self { server: server.into(), state: State::Unknown, interval: Duration::ZERO, due: now, streak: 0, attempts: 0, flips: VecDeque::new() }
    }

    #[must_use]
    pub fn server(&self) -> &str {
        &self.server
    }

    /// Whether a probe is due at `now` (a quarantine that ended counts).
    #[must_use]
    pub fn is_due(&self, now: Instant) -> bool {
        match self.state {
            State::Quarantined { until } => now >= until,
            _ => now >= self.due,
        }
    }

    /// The instant the next probe is due.
    #[must_use]
    pub fn next_due(&self) -> Instant {
        match self.state {
            State::Quarantined { until } => until,
            _ => self.due,
        }
    }

    fn schedule(&mut self, now: Instant, interval: Duration, p: &Policy) {
        self.attempts += 1;
        self.interval = interval;
        self.due = now + interval + jitter(&self.server, self.attempts, interval, p.jitter_pct);
    }

    /// Record a probe (or connect) result at `now` and schedule the next one.
    pub fn observe(&mut self, ok: bool, now: Instant, p: &Policy) -> Change {
        let released = matches!(self.state, State::Quarantined { until } if now >= until);
        let was_up = match self.state {
            State::Up => Some(true),
            State::Down => Some(false),
            State::Unknown | State::Quarantined { .. } => None,
        };
        let flipped = was_up.is_some_and(|w| w != ok);
        if flipped || was_up.is_none() {
            self.streak = 1;
        } else {
            self.streak = self.streak.saturating_add(1);
        }
        if flipped {
            self.flips.push_back(now);
        }
        while self.flips.front().is_some_and(|t| now.duration_since(*t) > p.window) {
            self.flips.pop_front();
        }
        if p.flaps > 0 && u32::try_from(self.flips.len()).unwrap_or(u32::MAX) >= p.flaps {
            self.flips.clear();
            self.state = State::Quarantined { until: now + p.quarantine };
            self.interval = p.quarantine;
            return Change::Quarantined;
        }
        let interval = if ok {
            if flipped || was_up.is_none() {
                p.min
            } else {
                let grown = self.interval.as_millis().saturating_mul(u128::from(p.growth_pct.max(100))) / 100;
                Duration::from_millis(u64::try_from(grown).unwrap_or(u64::MAX)).clamp(p.min, p.max.max(p.min))
            }
        } else if flipped || was_up.is_none() {
            p.fail
        } else {
            (self.interval * 2).clamp(p.fail, p.reconnect_max.max(p.fail))
        };
        self.state = if ok { State::Up } else { State::Down };
        self.schedule(now, interval, p);
        match (released, was_up, ok) {
            (true, _, _) => Change::Released,
            (_, Some(false) | None, true) => Change::CameUp,
            (_, Some(true) | None, false) => Change::WentDown,
            _ => Change::None,
        }
    }

    /// A tool call on this server failed: probe it now, unless quarantined.
    pub fn suspect(&mut self, now: Instant) {
        if !matches!(self.state, State::Quarantined { .. }) {
            self.due = now;
        }
    }

    /// Another process just saw this server healthy (the fleet lease): push
    /// the routine probe out by one interval without probing.
    pub fn defer(&mut self, now: Instant, p: &Policy) {
        if self.state == State::Up {
            let interval = self.interval.max(p.min);
            self.schedule(now, interval, p);
        }
    }

    /// Replace the computed interval with `interval` (the `mcp-probe` hook).
    pub fn override_interval(&mut self, now: Instant, interval: Duration) {
        if !matches!(self.state, State::Quarantined { .. }) {
            self.interval = interval;
            self.due = now + interval;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Policy {
        Policy { jitter_pct: 0, ..Policy::default() }
    }

    #[test]
    fn a_new_server_is_due_now_and_nothing_else() {
        let t = Instant::now();
        let h = Health::new("a", t);
        assert!(h.is_due(t));
        assert_eq!(h.state, State::Unknown);
    }

    #[test]
    fn healthy_backs_off_to_the_ceiling() {
        let t = Instant::now();
        let mut h = Health::new("a", t);
        assert_eq!(h.observe(true, t, &p()), Change::CameUp);
        assert_eq!(h.interval, Duration::from_secs(15));
        let seen: Vec<u64> = (0..6).map(|_| {
            h.observe(true, t, &p());
            h.interval.as_secs()
        }).collect();
        assert_eq!(seen, [30, 60, 120, 240, 300, 300]);
        assert!(!h.is_due(t + Duration::from_secs(299)) && h.is_due(t + Duration::from_secs(300)));
    }

    #[test]
    fn failure_is_retried_fast_then_with_backoff() {
        let t = Instant::now();
        let mut h = Health::new("a", t);
        h.observe(true, t, &p());
        assert_eq!(h.observe(false, t, &p()), Change::WentDown);
        assert_eq!(h.interval, Duration::from_secs(2));
        let seen: Vec<u64> = (0..8).map(|_| {
            h.observe(false, t, &p());
            h.interval.as_secs()
        }).collect();
        assert_eq!(seen, [4, 8, 16, 32, 64, 120, 120, 120]);
        assert_eq!(h.streak, 9);
    }

    #[test]
    fn a_flapping_server_is_quarantined_once_then_released() {
        let t = Instant::now();
        let pol = Policy { flaps: 3, ..p() };
        let mut h = Health::new("a", t);
        h.observe(true, t, &pol);
        let changes: Vec<Change> = [false, true, false].iter().map(|ok| h.observe(*ok, t, &pol)).collect();
        assert_eq!(changes, [Change::WentDown, Change::CameUp, Change::Quarantined]);
        assert!(!h.is_due(t + Duration::from_secs(599)));
        h.suspect(t);
        assert!(!h.is_due(t), "a quarantined server is not probed on suspicion");
        let after = t + Duration::from_secs(600);
        assert!(h.is_due(after));
        assert_eq!(h.observe(true, after, &pol), Change::Released);
        assert_eq!(h.state, State::Up);
    }

    #[test]
    fn flips_outside_the_window_do_not_count() {
        let t = Instant::now();
        let pol = Policy { flaps: 3, window: Duration::from_secs(10), ..p() };
        let mut h = Health::new("a", t);
        h.observe(true, t, &pol);
        for (i, ok) in [false, true, false, true].iter().enumerate() {
            let c = h.observe(*ok, t + Duration::from_secs(20 * (i as u64 + 1)), &pol);
            assert_ne!(c, Change::Quarantined);
        }
    }

    #[test]
    fn a_call_error_makes_it_due_now_and_the_lease_defers() {
        let t = Instant::now();
        let mut h = Health::new("a", t);
        h.observe(true, t, &p());
        assert!(!h.is_due(t));
        h.suspect(t);
        assert!(h.is_due(t));
        h.defer(t, &p());
        assert!(!h.is_due(t + Duration::from_secs(14)));
    }

    #[test]
    fn jitter_is_bounded_deterministic_and_differs_by_server() {
        let d = Duration::from_secs(100);
        assert_eq!(jitter("a", 1, d, 0), Duration::ZERO);
        let j = jitter("a", 1, d, 10);
        assert!(j <= Duration::from_secs(10));
        assert_eq!(j, jitter("a", 1, d, 10));
        let spread: std::collections::BTreeSet<Duration> = (0..20).map(|i| jitter(&format!("s{i}"), 1, d, 10)).collect();
        assert!(spread.len() > 5);
    }
}
