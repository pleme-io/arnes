//! The usage ledger — one forge-core cost `Event` per API call.
//!
//! Every call's raw usage (input, output, cache read, cache write) is kept,
//! priced where the price is known, appended to a JSONL file in the fleet's
//! canonical cost-event shape (`forge_core::Event`, which
//! attribution-forge and the other *-forge tools already read), and summed
//! for the status line. `DuckDB` reads the file directly — see
//! docs/queries/usage.sql.
//!
//! **Pricing assumption, stated not hidden.** Cost is computed with
//! Anthropic's semantics — `input_tokens` excludes cached tokens, which are
//! reported in `cache_read_input_tokens`. Whether `DeepSeek`'s
//! Anthropic-compatible endpoint follows that is undocumented
//! (docs/token-efficiency.md §7); the raw fields are always logged, so a
//! cost computed under the wrong reading can be recomputed from the file.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use arnes_core::Usage;
use chrono::{Datelike, Timelike, Utc, Weekday};
use forge_core::Event;

/// USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub cache_hit: f64,
    pub cache_miss: f64,
    pub output: f64,
}

/// `DeepSeek`'s peak window (UTC): weekdays 01:00–04:00 and 06:00–10:00.
/// Everything else — including all weekend — is off-peak (half price).
#[must_use]
pub fn deepseek_peak(ts: chrono::DateTime<Utc>) -> bool {
    let weekend = matches!(ts.weekday(), Weekday::Sat | Weekday::Sun);
    let h = ts.hour();
    !weekend && ((1..4).contains(&h) || (6..10).contains(&h))
}

/// Known prices (api-docs.deepseek.com/quick_start/pricing, 2026-09-26).
/// `None` = not known here; the event is logged with cost 0 and
/// `priced = false` rather than a guessed number.
#[must_use]
pub fn price(provider: &str, model: &str, peak: bool) -> Option<Price> {
    let p = match (provider, model) {
        ("deepseek", "deepseek-flash") => Price { cache_hit: 0.006, cache_miss: 0.30, output: 1.20 },
        ("deepseek", "deepseek-v4-pro") => Price { cache_hit: 0.044, cache_miss: 1.32, output: 3.96 },
        _ => return None,
    };
    Some(if peak { p } else { Price { cache_hit: p.cache_hit / 2.0, cache_miss: p.cache_miss / 2.0, output: p.output / 2.0 } })
}

/// Cost of one call's usage at `price` (Anthropic usage semantics).
#[must_use]
pub fn cost(u: &Usage, p: Price) -> f64 {
    let hit = f64::from(u.cache_read_input_tokens.unwrap_or(0));
    // A cache write is billed as uncached input.
    let miss = f64::from(u.input_tokens) + f64::from(u.cache_creation_input_tokens.unwrap_or(0));
    let out = f64::from(u.output_tokens);
    (hit * p.cache_hit + miss * p.cache_miss + out * p.output) / 1_000_000.0
}

/// Session running totals, for the status line.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Totals {
    pub calls: u32,
    pub usage: Usage,
    pub cost_usd: f64,
    /// Whether every call so far had a known price.
    pub all_priced: bool,
    pub last: Usage,
}

impl Totals {
    /// Share of prompt tokens served from cache, 0.0–1.0.
    #[must_use]
    pub fn cache_hit_ratio(&self) -> f64 {
        let hit = f64::from(self.usage.cache_read_input_tokens.unwrap_or(0));
        let all = hit
            + f64::from(self.usage.input_tokens)
            + f64::from(self.usage.cache_creation_input_tokens.unwrap_or(0));
        if all == 0.0 { 0.0 } else { hit / all }
    }
}

/// `$XDG_STATE_HOME/arnes/usage.jsonl`, else `~/.local/state/arnes/usage.jsonl`.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    arnes_store::Layout::from_env().map(|l| l.usage_ledger())
}

/// Appends one event per call and keeps the session totals.
pub struct Ledger {
    path: Option<PathBuf>,
    session: String,
    totals: Mutex<Totals>,
    prices: Mutex<HashMap<String, Price>>,
}

impl Ledger {
    /// `path = None` keeps totals only (ledger file disabled).
    #[must_use]
    pub fn new(path: Option<PathBuf>) -> Self {
        let session = format!("{}-{}", Utc::now().format("%Y%m%dT%H%M%S"), std::process::id());
        Self {
            path,
            session,
            totals: Mutex::new(Totals { all_priced: true, ..Totals::default() }),
            prices: Mutex::new(HashMap::new()),
        }
    }

    pub fn set_prices(&self, prices: HashMap<String, Price>) {
        if let Ok(mut p) = self.prices.lock() {
            *p = prices;
        }
    }

    /// This process's session id, `<start time>-<pid>`, on every record.
    #[must_use]
    pub fn session(&self) -> &str {
        &self.session
    }

    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    #[must_use]
    pub fn totals(&self) -> Totals {
        self.totals.lock().map_or_else(|_| Totals::default(), |t| *t)
    }

    /// Record one API call. A failed write is reported once on stderr by
    /// the caller's tracing, never fatal — the conversation matters more.
    pub fn record(&self, served: &arnes_anthropic::Served, model: &str, usage: &Usage) {
        let now = Utc::now();
        let peak = deepseek_peak(now);
        let declared = self.prices.lock().ok().and_then(|p| p.get(&served.label).copied());
        let price = declared.or_else(|| price(&served.provider, model, peak));
        let cost_usd = price.map_or(0.0, |p| cost(usage, p));
        if let Ok(mut t) = self.totals.lock() {
            t.calls += 1;
            t.usage.merge(*usage);
            t.cost_usd += cost_usd;
            t.all_priced &= price.is_some();
            t.last = *usage;
        }
        let Some(path) = &self.path else { return };
        let mut dimensions = HashMap::new();
        let mut dim = |k: &str, v: String| {
            dimensions.insert(k.to_owned(), v);
        };
        dim("input_tokens", usage.input_tokens.to_string());
        dim("output_tokens", usage.output_tokens.to_string());
        dim("cache_read_input_tokens", usage.cache_read_input_tokens.unwrap_or(0).to_string());
        dim("cache_creation_input_tokens", usage.cache_creation_input_tokens.unwrap_or(0).to_string());
        dim("peak", peak.to_string());
        dim("priced", price.is_some().to_string());
        dim("tool", "arnes".to_owned());
        dim("route", served.label.clone());
        dim("fallback", served.fallback.to_string());
        dim("latency_ms", served.latency_ms.to_string());
        let event = Event {
            ts: now,
            cost_usd,
            cloud: served.provider.clone(),
            service: model.to_owned(),
            region: None,
            resource_id: Some(self.session.clone()),
            dimensions,
        };
        let line = match serde_json::to_string(&event) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "ledger: could not serialise event");
                return;
            }
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = written {
            tracing::warn!(error = %e, path = %path.display(), "ledger: append failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32, h: u32) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, 30, 0).unwrap()
    }

    #[test]
    fn peak_windows() {
        // 2026-09-28 is a Monday.
        assert!(deepseek_peak(at(2026, 9, 28, 2)));
        assert!(deepseek_peak(at(2026, 9, 28, 7)));
        assert!(!deepseek_peak(at(2026, 9, 28, 5)), "04–06 is off-peak");
        assert!(!deepseek_peak(at(2026, 9, 28, 12)));
        assert!(!deepseek_peak(at(2026, 9, 27, 2)), "Sunday is off-peak all day");
    }

    #[test]
    fn cost_uses_hit_miss_and_output_rates() {
        let u = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_input_tokens: Some(1_000_000),
            cache_creation_input_tokens: None,
        };
        let p = price("deepseek", "deepseek-flash", true).unwrap();
        assert!((cost(&u, p) - (0.006 + 0.30 + 1.20)).abs() < 1e-9);
        let off = price("deepseek", "deepseek-flash", false).unwrap();
        assert!((cost(&u, off) - (0.003 + 0.15 + 0.60)).abs() < 1e-9, "off-peak is half");
        assert_eq!(price("anthropic", "claude-opus-5-5", true), None, "unknown prices are not guessed");
    }

    fn served(label: &str, provider: &str, fallback: bool) -> arnes_anthropic::Served {
        arnes_anthropic::Served { label: label.into(), model: String::new(), fallback, provider: provider.into(), latency_ms: 7 }
    }

    #[test]
    fn a_call_is_attributed_to_the_route_that_served_it_at_that_routes_price() {
        let dir = tempfile_dir("served");
        let path = dir.join("usage.jsonl");
        let ledger = Ledger::new(Some(path.clone()));
        ledger.set_prices(HashMap::from([("opus".to_owned(), Price { cache_hit: 0.5, cache_miss: 5.0, output: 25.0 })]));
        let u = Usage { input_tokens: 1_000_000, output_tokens: 0, ..Usage::default() };
        ledger.record(&served("opus", "anthropic", true), "claude-opus-5-5", &u);
        ledger.record(&served("gpubox", "local", false), "local", &u);
        let events = forge_core::event::load_jsonl(&path).unwrap();
        assert_eq!((events[0].cloud.as_str(), events[0].dim("route"), events[0].dim("fallback"), events[0].dim("latency_ms")), ("anthropic", Some("opus"), Some("true"), Some("7")));
        assert!((events[0].cost_usd - 5.0).abs() < 1e-9, "the route's own price, not the table's");
        assert_eq!((events[1].cloud.as_str(), events[1].cost_usd), ("local", 0.0));
        let t = ledger.totals();
        assert!(!t.all_priced, "the unpriced local call is marked");
        assert_eq!(t.last, u);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn tempfile_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("arnes-ledger-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn hit_ratio() {
        let t = Totals {
            usage: Usage { input_tokens: 20, output_tokens: 5, cache_read_input_tokens: Some(80), cache_creation_input_tokens: None },
            ..Totals::default()
        };
        assert!((t.cache_hit_ratio() - 0.8).abs() < 1e-9);
        assert_eq!(Totals::default().cache_hit_ratio(), 0.0);
    }

    #[test]
    fn the_ledger_file_is_readable_by_forge_core() {
        let dir = std::env::temp_dir().join(format!("arnes-ledger-{}", std::process::id()));
        let path = dir.join("usage.jsonl");
        let ledger = Ledger::new(Some(path.clone()));
        let u = Usage { input_tokens: 100, output_tokens: 10, cache_read_input_tokens: Some(900), cache_creation_input_tokens: None };
        ledger.record(&served("deepseek", "deepseek", false), "deepseek-flash", &u);
        ledger.record(&served("deepseek", "deepseek", false), "deepseek-flash", &u);
        // The fleet's own reader is the contract: attribution-forge et al.
        let events = forge_core::event::load_jsonl(&path).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].cloud, "deepseek");
        assert_eq!(events[0].service, "deepseek-flash");
        assert_eq!(events[0].dim("cache_read_input_tokens"), Some("900"));
        assert!(events[0].cost_usd > 0.0);
        let t = ledger.totals();
        assert_eq!(t.calls, 2);
        assert_eq!(t.usage.cache_read_input_tokens, Some(1800));
        assert!(t.all_priced);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn disabled_file_still_keeps_totals() {
        let ledger = Ledger::new(None);
        ledger.record(&served("opus", "anthropic", false), "claude-opus-5-5", &Usage { input_tokens: 5, output_tokens: 1, ..Usage::default() });
        let t = ledger.totals();
        assert_eq!(t.calls, 1);
        assert!(!t.all_priced, "an unpriced call is marked, not zeroed silently");
    }
}
