//! The MCP health scheduler (docs/profiles.md §1): one task that probes each
//! connected server when its [`Health`] says so, reconnects a failed one with
//! backoff, quarantines a flapping one, and probes a server now when a call to
//! it failed. The TUI hears about it through [`Msg`]; nothing here draws.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arnes_mcp_client::health::{Change, Health, Policy, State};
use arnes_mcp_client::{Arrival, McpPool, McpRegistry, Probe};
use tokio::sync::{mpsc, watch};

/// What the scheduler tells the TUI.
pub enum Msg {
    /// A failed server reconnected: attach it.
    Arrived(Arrival),
    /// A notice for the lane: `(warn?, text)`.
    Notice(bool, String),
    /// The facts changed (the status segment and kenkou read them).
    Probed,
}

/// The fleet-wide probe lease (docs/profiles.md §1): a routine probe of a
/// healthy server is skipped when another process saw it healthy within the
/// interval. Failure-driven probes never ask.
#[async_trait::async_trait]
pub trait Lease: Send + Sync {
    /// Whether another holder saw `fingerprint` healthy within `within`.
    async fn fresh(&self, fingerprint: &str, within: Duration) -> bool;
    /// Record that this process just saw it healthy.
    async fn take(&self, fingerprint: &str);
}

#[async_trait::async_trait]
impl Lease for arnes_vigy::Lease {
    async fn fresh(&self, fingerprint: &str, within: Duration) -> bool {
        Self::fresh(self, fingerprint, within).await
    }

    async fn take(&self, fingerprint: &str) {
        Self::take(self, fingerprint).await;
    }
}

/// Everything the scheduler reads; each is a watch, so a reload or a profile
/// switch is seen on the next round.
pub struct Inputs {
    pub pool: watch::Receiver<Arc<McpPool>>,
    pub registry: watch::Receiver<McpRegistry>,
    /// `None`: probing is off. Else the schedule, the probe timeout and the
    /// connect timeout for reconnects.
    pub policy: watch::Receiver<Option<(Policy, Duration, Duration)>>,
    /// The hosted `mcp-probe` hooks.
    pub hooks: watch::Receiver<Vec<arnes_blue::Script>>,
    /// A server whose call just failed.
    pub suspect: mpsc::UnboundedReceiver<String>,
    pub lease: Option<Arc<dyn Lease>>,
    pub log_dir: Option<std::path::PathBuf>,
}

/// One round's decision for one server: what to do with it now.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    Probe,
    Reconnect,
    /// Another process just saw it healthy.
    Defer,
}

fn step(h: &Health, connected: bool, lease_fresh: bool) -> Step {
    match (connected, h.state) {
        (false, _) => Step::Reconnect,
        (true, State::Up) if lease_fresh => Step::Defer,
        (true, _) => Step::Probe,
    }
}

/// The servers the scheduler watches: connected ones, and failed ones it
/// may reconnect. A server still starting, or dormant, is left alone.
fn watched(pool: &McpPool) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = pool.servers().into_iter().map(|s| (s.to_owned(), true)).collect();
    out.extend(pool.failures().iter().map(|(s, _)| (s.clone(), false)));
    out.sort();
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// The text the TUI says for a change, if any.
fn notice(server: &str, was: State, change: Change, h: &Health, error: Option<&str>) -> Option<(bool, String)> {
    let secs = h.interval.as_secs().max(1);
    match change {
        Change::WentDown if was != State::Unknown => {
            Some((true, format!("MCP {server} down: {}; retrying in {secs}s", error.unwrap_or("no answer"))))
        }
        Change::CameUp if was == State::Down => Some((false, format!("MCP {server} is back"))),
        Change::Quarantined => Some((true, format!("MCP {server} keeps flapping; quarantined for {secs}s"))),
        Change::Released => Some((false, format!("MCP {server} released from quarantine"))),
        _ => None,
    }
}

/// The servers due at `now`, each with what to do; `leased` says whether
/// another process just saw it healthy.
fn due_now(servers: &[(String, bool)], health: &BTreeMap<String, Health>, now: Instant, leased: impl Fn(&str, &Health) -> bool) -> Vec<(String, Step)> {
    servers
        .iter()
        .filter_map(|(s, connected)| {
            let h = health.get(s).filter(|h| h.is_due(now))?;
            Some((s.clone(), step(h, *connected, leased(s, h))))
        })
        .collect()
}

/// Turn each routine probe of a healthy server that another process just
/// saw healthy into a deferral.
async fn defer_leased(lease: &dyn Lease, due: &mut [(String, Step)], health: &BTreeMap<String, Health>, fingerprint: &impl Fn(&str) -> String) {
    for (s, st) in due {
        let h = &health[s.as_str()];
        if *st == Step::Probe && h.state == State::Up && lease.fresh(&fingerprint(s), h.interval).await {
            *st = Step::Defer;
        }
    }
}

/// Ask the `mcp-probe` hooks for this server's next interval; an answer
/// replaces the computed one. Returns a refusal to say, if any.
async fn ask_hooks(hooks: &[arnes_blue::Script], h: &mut Health, server: &str, up: bool, now: Instant) -> Option<String> {
    if hooks.is_empty() {
        return None;
    }
    let arg = serde_json::json!({
        "server": server, "up": up, "streak": h.streak,
        "interval_ms": u64::try_from(h.interval.as_millis()).unwrap_or(u64::MAX),
    })
    .to_string();
    let hooks = hooks.to_vec();
    let answer = tokio::task::spawn_blocking(move || arnes_blue::policy("mcp-probe", &hooks, &arg, arnes_blue::DEFAULT_BUDGET)).await.ok().flatten()?;
    match answer {
        Ok(ms) => match ms.trim().parse::<u64>() {
            Ok(n) if n >= 100 => {
                h.override_interval(now, Duration::from_millis(n));
                None
            }
            _ => Some(format!("mcp-probe hook: {ms:?} is not a whole number of ms (at least 100); ignored")),
        },
        Err(e) => Some(format!("{e}; the computed interval is kept")),
    }
}

/// Start the scheduler. `facts` is filled with the newest probe per server.
pub fn spawn(mut inputs: Inputs, out: mpsc::UnboundedSender<Msg>, facts: arnes_vigy::Facts) {
    tokio::spawn(async move {
        let mut health: BTreeMap<String, Health> = BTreeMap::new();
        let mut latest: BTreeMap<String, Probe> = BTreeMap::new();
        loop {
            let pool = inputs.pool.borrow().clone();
            let registry = inputs.registry.borrow().clone();
            let policy = *inputs.policy.borrow();
            let hooks = inputs.hooks.borrow().clone();
            let now = Instant::now();
            let servers = watched(&pool);
            health.retain(|s, _| servers.iter().any(|(n, _)| n == s));
            latest.retain(|s, _| servers.iter().any(|(n, _)| n == s));
            let Some((p, timeout, connect)) = policy else {
                tokio::select! {
                    _ = inputs.policy.changed() => {}
                    r = inputs.suspect.recv() => if r.is_none() { return },
                }
                continue;
            };
            for (s, _) in &servers {
                health.entry(s.clone()).or_insert_with(|| Health::new(s.clone(), now));
            }
            let fingerprint = |s: &str| registry.get(s).map(arnes_mcp_client::ServerSpec::fingerprint).unwrap_or_default();
            let mut due = due_now(&servers, &health, now, |_, _| false);
            if let Some(l) = &inputs.lease {
                defer_leased(l.as_ref(), &mut due, &health, &fingerprint).await;
            }
            let rounds = due.iter().map(|(s, st)| {
                let pool = pool.clone();
                let spec = registry.get(s).cloned();
                let log_dir = inputs.log_dir.clone();
                async move {
                    match st {
                        Step::Defer => (s.clone(), None, None),
                        Step::Probe => (s.clone(), Some(pool.probe_one(s, timeout).await), None),
                        Step::Reconnect => match spec {
                            None => (s.clone(), None, None),
                            Some(spec) => {
                                let a = McpPool::connect_server(spec, connect, log_dir.as_deref()).await;
                                let probe = Probe {
                                    server: s.clone(),
                                    up: a.error().is_none(),
                                    latency_ms: None,
                                    tools: 0,
                                    error: a.error().map(str::to_owned),
                                };
                                (s.clone(), Some(probe), Some(a))
                            }
                        },
                    }
                }
            });
            let mut changed = false;
            for (server, probe, arrival) in futures::future::join_all(rounds).await {
                let now = Instant::now();
                let Some(h) = health.get_mut(&server) else { continue };
                let Some(probe) = probe else {
                    h.defer(now, &p);
                    continue;
                };
                let was = h.state;
                let change = h.observe(probe.up, now, &p);
                if probe.up
                    && let Some(l) = &inputs.lease
                {
                    l.take(&fingerprint(&server)).await;
                }
                if let Some(refused) = ask_hooks(&hooks, h, &server, probe.up, now).await {
                    let _ = out.send(Msg::Notice(true, refused));
                }
                if let Some(n) = notice(&server, was, change, h, probe.error.as_deref()) {
                    let _ = out.send(Msg::Notice(n.0, n.1));
                }
                if let Some(a) = arrival.filter(|a| a.error().is_none()) {
                    let _ = out.send(Msg::Arrived(a));
                }
                latest.insert(server, probe);
                changed = true;
            }
            if changed {
                facts.set(latest.values().cloned().collect());
                let _ = out.send(Msg::Probed);
            }
            let next = health.values().map(Health::next_due).min();
            let sleep = next.map_or(Duration::from_secs(3600), |t| t.saturating_duration_since(Instant::now()));
            tokio::select! {
                () = tokio::time::sleep(sleep) => {}
                r = inputs.suspect.recv() => match r {
                    None => return,
                    Some(server) => if let Some(h) = health.get_mut(&server) { h.suspect(Instant::now()) },
                },
                _ = inputs.pool.changed() => {}
                _ = inputs.registry.changed() => {}
                _ = inputs.policy.changed() => {}
                _ = inputs.hooks.changed() => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connected_server_is_probed_a_failed_one_reconnected_and_a_leased_healthy_one_deferred() {
        let t = Instant::now();
        let p = Policy::default();
        let mut h = Health::new("a", t);
        assert_eq!(step(&h, false, true), Step::Reconnect);
        assert_eq!(step(&h, true, true), Step::Probe, "only a server seen up may be deferred");
        h.observe(true, t, &p);
        assert_eq!(step(&h, true, false), Step::Probe);
        assert_eq!(step(&h, true, true), Step::Defer);
    }

    #[test]
    fn notices_name_real_changes_only() {
        let t = Instant::now();
        let p = Policy::default();
        let mut h = Health::new("a", t);
        let c = h.observe(true, t, &p);
        assert_eq!(notice("a", State::Unknown, c, &h, None), None, "coming up the first time is not news");
        let c = h.observe(false, t, &p);
        let (warn, text) = notice("a", State::Up, c, &h, Some("tools/list: gone")).unwrap();
        assert!(warn && text.contains("down: tools/list: gone") && text.contains("retrying in 2s"), "{text}");
        let c = h.observe(true, t, &p);
        assert_eq!(notice("a", State::Down, c, &h, None), Some((false, "MCP a is back".into())));
    }

    struct Always(bool);

    #[async_trait::async_trait]
    impl Lease for Always {
        async fn fresh(&self, _: &str, _: Duration) -> bool {
            self.0
        }
        async fn take(&self, _: &str) {}
    }

    #[tokio::test]
    async fn a_fresh_lease_defers_only_routine_probes_of_healthy_servers() {
        let t = Instant::now();
        let p = Policy::default();
        let mut health = BTreeMap::new();
        let mut up = Health::new("up", t);
        up.observe(true, t, &p);
        let mut down = Health::new("down", t);
        down.observe(true, t, &p);
        down.observe(false, t, &p);
        health.insert("up".to_owned(), up);
        health.insert("down".to_owned(), down);
        let mut due = vec![("up".to_owned(), Step::Probe), ("down".to_owned(), Step::Probe)];
        defer_leased(&Always(true), &mut due, &health, &|s: &str| s.to_owned()).await;
        assert_eq!(due, [("up".to_owned(), Step::Defer), ("down".to_owned(), Step::Probe)]);
        let mut due = vec![("up".to_owned(), Step::Probe)];
        defer_leased(&Always(false), &mut due, &health, &|s: &str| s.to_owned()).await;
        assert_eq!(due, [("up".to_owned(), Step::Probe)]);
    }

    #[test]
    fn the_watched_set_is_connected_and_failed_servers_once_each() {
        assert!(watched(&McpPool::empty()).is_empty());
    }
}
