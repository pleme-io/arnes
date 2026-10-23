//! arnes as a vigy host (`docs/persistence.md`, the vigy data path).
//!
//! - **Fetch once.** arnes probes its MCP servers and keeps the facts in a
//!   [`Facts`] cache; nothing else probes them.
//! - **Tick once.** The runtime is opened on the shared store for host
//!   `arnes`, so another host on the same store (mado, the CLI) lists these
//!   vigies but never schedules them.
//! - **Store once.** Each tick's result is a `noop` action whose payload is
//!   the blue function's text, persisted in the shared store where any host
//!   reads it.
//!
//! A vigy's body is blue. The program the store holds is generated from its
//! declaration ([`program`]), one call into the `arnes-blue` intrinsic, so
//! nobody writes tatara-lisp for an arnes vigy.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use arnes_blue::Script;
use arnes_mcp_client::Probe;
use tatara_lisp_eval::{Arity, EvalError, Value};
use vigy_eval::VigyHost;
use vigy_runtime::{HOST_LABEL, RuntimeHandle};
use vigy_types::{ResultStatus, TickInterval, Vigy, VigyId, VigyRun};

/// The host name arnes schedules for.
pub const HOST: &str = "arnes";

/// The shared vigy store: `$VIGY_DB`, else `~/.local/share/vigy/vigy.db`
/// (the rule the `vigy` CLI and mado follow).
#[must_use]
pub fn store_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("VIGY_DB") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/vigy/vigy.db"))
}

/// The last MCP probe, shared between the task that fills it and the vigy
/// intrinsics that read it (synchronously, under `spawn_blocking`).
#[derive(Clone, Default)]
pub struct Facts(Arc<RwLock<Vec<Probe>>>);

impl Facts {
    pub fn set(&self, probes: Vec<Probe>) {
        if let Ok(mut g) = self.0.write() {
            *g = probes;
        }
    }

    #[must_use]
    pub fn get(&self) -> Vec<Probe> {
        self.0.read().map(|g| g.clone()).unwrap_or_default()
    }

    /// The facts as the JSON array a blue body receives.
    #[must_use]
    pub fn json(&self) -> String {
        serde_json::to_string(&self.get()).unwrap_or_else(|_| "[]".into())
    }
}

/// One declared vigy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decl {
    pub name: String,
    pub script: Script,
    pub every: Duration,
}

fn lisp_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The tatara-lisp program the store holds for `script`: call the blue
/// function with the MCP facts and record its text as the tick's result
/// (`noop`), then record the facts it saw (`pull`), so any host reads the
/// health from the store instead of probing.
#[must_use]
pub fn program(script: &Script) -> String {
    format!(
        "(vigy-emit \"noop\" (arnes-blue {} {}))\n(vigy-emit \"pull\" (arnes-mcp-facts))",
        lisp_str(&arnes_blue::expand_home(&script.path).display().to_string()),
        lisp_str(&script.function)
    )
}

/// The `arnes` extension: `(arnes-blue path fn)` runs a blue function with
/// the facts JSON as its one argument and returns its text; `(arnes-mcp-facts)`
/// returns the facts JSON itself.
#[must_use]
pub fn extension(facts: Facts) -> vigy_eval::ExtensionHandle {
    vigy_eval::closure_extension(move |interp: &mut tatara_lisp_eval::Interpreter<VigyHost>| {
        let f = facts.clone();
        interp.register_fn("arnes-mcp-facts", Arity::Exact(0), move |_: &[Value], _: &mut VigyHost, _| {
            Ok(Value::Str(f.json().into()))
        });
        let f = facts.clone();
        interp.register_fn("arnes-blue", Arity::Exact(2), move |args: &[Value], _: &mut VigyHost, span| {
            let text = |i: usize| match args.get(i) {
                Some(Value::Str(s)) => Ok(s.to_string()),
                other => Err(EvalError::native_fn("arnes-blue", format!("argument {} must be a string, got {other:?}", i + 1), span)),
            };
            let script = Script::new(text(0)?, text(1)?);
            let facts = f.json();
            let out = arnes_blue::invoke("vigy", &script, &[&facts], "")
                .map_err(|e| EvalError::native_fn("arnes-blue", e.to_string(), span))?;
            Ok(Value::Str(out.text().to_owned().into()))
        });
    })
}

/// A declared vigy and its last result.
#[derive(Debug, Clone)]
pub struct Status {
    pub name: String,
    pub every: Duration,
    pub last: Option<VigyRun>,
}

impl Status {
    /// The last run's text: the blue function's return, or its error.
    #[must_use]
    pub fn text(&self) -> String {
        match &self.last {
            None => "not run yet".into(),
            Some(r) if matches!(r.result, ResultStatus::Failed) => {
                format!("failed: {}", r.error.as_deref().unwrap_or("unknown error"))
            }
            Some(r) => summary(r).unwrap_or_default(),
        }
    }
}

/// The text a successful tick recorded.
#[must_use]
pub fn summary(run: &VigyRun) -> Option<String> {
    run.actions
        .iter()
        .filter(|a| a.kind == vigy_types::ReconcileKind::Noop)
        .find_map(|a| a.payload.as_ref()?.as_str().map(str::to_owned))
}

/// The probe facts a tick recorded, if it recorded any.
#[must_use]
pub fn recorded_facts(run: &VigyRun) -> Option<Vec<Probe>> {
    run.actions
        .iter()
        .filter(|a| a.kind == vigy_types::ReconcileKind::Pull)
        .find_map(|a| serde_json::from_str(a.payload.as_ref()?.as_str()?).ok())
}

/// Facts read back from the store, and how old they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    pub probes: Vec<Probe>,
    pub age: Duration,
}

impl Health {
    /// One server's line: `up 12ms, 3 tools, 4s ago` or `down: why, 4s ago`.
    #[must_use]
    pub fn line(&self, server: &str) -> Option<String> {
        let p = self.probes.iter().find(|p| p.server == server)?;
        let ago = format!("{}s ago", self.age.as_secs());
        Some(if p.up {
            format!("up {}ms, {} tools, {ago}", p.latency_ms.unwrap_or(0), p.tools)
        } else {
            format!("down: {}, {ago}", p.error.as_deref().unwrap_or("no answer"))
        })
    }
}

/// The newest recorded facts among `runs`, aged against `now`.
#[must_use]
pub fn newest_health(runs: &[VigyRun], now: time::OffsetDateTime) -> Option<Health> {
    let run = runs.iter().filter(|r| recorded_facts(r).is_some()).max_by_key(|r| r.started_at)?;
    let age = Duration::try_from(now - run.started_at).unwrap_or_default();
    Some(Health { probes: recorded_facts(run)?, age })
}

/// The `/vigy` panel: each vigy and its last result, then each server's
/// recorded health.
#[must_use]
pub fn panel_lines(status: &[Status], health: Option<&Health>) -> Vec<String> {
    let mut out: Vec<String> = status.iter().map(|s| format!("{}  every {}s  {}", s.name, s.every.as_secs(), s.text())).collect();
    if let Some(h) = health {
        out.push(String::new());
        out.extend(h.probes.iter().filter_map(|p| h.line(&p.server).map(|l| format!("{:<20} {l}", p.server))));
    }
    out
}

/// arnes's embedded runtime and the vigies it declared.
#[derive(Clone)]
pub struct Ticking {
    rt: RuntimeHandle,
    declared: Vec<(Decl, VigyId)>,
}

impl Ticking {
    /// Open the store at `path` for host `arnes` and register `decls`. A
    /// vigy this host registered before and no longer declares is disabled
    /// (kept, with its history), so an edited declaration never leaves its
    /// old self ticking. A declaration vigy refuses (an interval under its
    /// minimum) is skipped and named in the second value.
    ///
    /// # Errors
    /// The store cannot be opened.
    pub async fn open(path: &Path, decls: Vec<Decl>, facts: Facts) -> Result<(Self, Vec<String>), String> {
        let mut exts = vigy_eval::standard_extensions();
        exts.push(extension(facts));
        let rt = RuntimeHandle::open_with_extensions_for(path, exts, HOST).await.map_err(|e| e.to_string())?;
        Self::register(rt, decls).await
    }

    /// The same runtime with `decls` as the declared set (config hot
    /// reload): a new or edited vigy is registered, one no longer declared is
    /// disabled, and the others keep ticking on their schedule.
    ///
    /// # Errors
    /// The store refuses a registration.
    pub async fn redeclare(&self, decls: Vec<Decl>) -> Result<(Self, Vec<String>), String> {
        Self::register(self.rt.clone(), decls).await
    }

    async fn register(rt: RuntimeHandle, decls: Vec<Decl>) -> Result<(Self, Vec<String>), String> {
        let mut declared = Vec::new();
        let mut refused = Vec::new();
        for d in decls {
            let every = u64::try_from(d.every.as_millis()).unwrap_or(u64::MAX);
            let built = TickInterval::from_millis(every)
                .map_err(|e| e.to_string())
                .and_then(|i| Vigy::new(format!("arnes/{}", d.name), program(&d.script), i).map_err(|e| e.to_string()))
                .and_then(|mut v| {
                    v.labels.insert(HOST_LABEL, HOST).map_err(|e| e.to_string())?;
                    Ok(v)
                });
            match built {
                Ok(v) => {
                    let v = rt.register_or_update(v).await.map_err(|e| e.to_string())?;
                    declared.push((d, v.id));
                }
                Err(e) => refused.push(format!("vigy {}: {e}", d.name)),
            }
        }
        let keep: Vec<&VigyId> = declared.iter().map(|(_, id)| id).collect();
        let mine = rt.list(Some(&format!("{HOST_LABEL}={HOST}"))).await.map_err(|e| e.to_string())?;
        for v in mine.into_iter().filter(|v| v.enabled && !keep.contains(&&v.id)) {
            let _ = rt.disable(&v.id).await;
        }
        Ok((Self { rt, declared }, refused))
    }

    /// Every declared vigy with its last result, in declaration order.
    pub async fn status(&self) -> Vec<Status> {
        let mut out = Vec::with_capacity(self.declared.len());
        for (d, id) in &self.declared {
            let last = self.rt.recent_runs(id, 1).await.ok().and_then(|mut r| r.pop());
            out.push(Status { name: d.name.clone(), every: d.every, last });
        }
        out
    }

    /// The newest probe facts any declared vigy recorded in the store.
    pub async fn health(&self) -> Option<Health> {
        let mut runs = Vec::new();
        for (_, id) in &self.declared {
            runs.extend(self.rt.recent_runs(id, 1).await.unwrap_or_default());
        }
        newest_health(&runs, time::OffsetDateTime::now_utc())
    }

    /// Every finished tick, for redrawing on each one.
    #[must_use]
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<VigyRun> {
        self.rt.subscribe()
    }

    /// The shortest declared interval: how often the facts are worth refreshing.
    #[must_use]
    pub fn cadence(&self) -> Option<Duration> {
        self.declared.iter().map(|(d, _)| d.every).min()
    }

    /// Tick one declared vigy now.
    ///
    /// # Errors
    /// No such vigy, or the store failed.
    pub async fn tick(&self, name: &str) -> Result<VigyRun, String> {
        let (_, id) = self.declared.iter().find(|(d, _)| d.name == name).ok_or_else(|| format!("no vigy named {name}"))?;
        self.rt.tick_now(id).await.map_err(|e| e.to_string())
    }

    #[cfg(test)]
    async fn in_memory(decls: Vec<Decl>, facts: Facts) -> (Self, Vec<String>) {
        let rt = RuntimeHandle::open_in_memory_for(HOST).await.unwrap();
        let _ = facts;
        Self::register(rt, decls).await.unwrap()
    }
}

/// One line for a status bar: `mcp 5/6`, from the facts.
#[must_use]
pub fn segment(probes: &[Probe]) -> Option<String> {
    (!probes.is_empty()).then(|| format!("mcp {}/{}", probes.iter().filter(|p| p.up).count(), probes.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(server: &str, up: bool) -> Probe {
        Probe { server: server.into(), up, latency_ms: up.then_some(12), tools: usize::from(up) * 3, error: (!up).then(|| "gone".into()) }
    }

    fn blue(dir: &tempfile::TempDir, src: &str) -> Script {
        let p = dir.path().join("health.b");
        std::fs::write(&p, src).unwrap();
        Script::new(p, "tick")
    }

    #[test]
    fn program_escapes_its_strings() {
        let s = Script::new("/a \"b\"\\c.b", "tick");
        assert_eq!(
            program(&s),
            "(vigy-emit \"noop\" (arnes-blue \"/a \\\"b\\\"\\\\c.b\" \"tick\"))\n(vigy-emit \"pull\" (arnes-mcp-facts))"
        );
    }

    #[test]
    fn segment_counts_up_servers() {
        assert_eq!(segment(&[]), None);
        assert_eq!(segment(&[probe("a", true), probe("b", false)]).as_deref(), Some("mcp 1/2"));
    }

    #[tokio::test]
    async fn a_blue_vigy_sees_the_facts_and_its_text_is_the_result() {
        let dir = tempfile::tempdir().unwrap();
        let src = "def tick(facts)\n  ps = json_parse(facts)\n  \"#{length(ps)} servers\"\nend\n";
        let facts = Facts::default();
        facts.set(vec![probe("a", true), probe("b", false)]);
        let mut exts = vigy_eval::standard_extensions();
        exts.push(extension(facts.clone()));
        let rt = RuntimeHandle::open_in_memory_for(HOST).await.unwrap();
        let v = Vigy::new("t", program(&blue(&dir, src)), TickInterval::from_millis(1000).unwrap()).unwrap();
        let id = v.id.clone();
        rt.register_or_update(v).await.unwrap();
        let run = rt.tick_now_with(&id, &vigy_eval::LispReconciler::with_extensions(rt.get(&id).await.unwrap().program, exts)).await.unwrap();
        assert!(matches!(run.result, ResultStatus::Ok), "{:?}", run.error);
        assert_eq!(summary(&run).as_deref(), Some("2 servers"));
        assert_eq!(recorded_facts(&run), Some(facts.get()), "the tick stores what it saw");
        let later = run.started_at + time::Duration::seconds(4);
        let h = newest_health(std::slice::from_ref(&run), later).unwrap();
        assert_eq!(h.line("a").as_deref(), Some("up 12ms, 3 tools, 4s ago"));
        assert_eq!(h.line("b").as_deref(), Some("down: gone, 4s ago"));
        assert_eq!(h.line("zzz"), None);
        assert_eq!(newest_health(&[], later), None);
        let st = [Status { name: "t".into(), every: Duration::from_secs(5), last: Some(run.clone()) }];
        let lines = panel_lines(&st, Some(&h));
        assert_eq!(lines[0], "t  every 5s  2 servers");
        assert!(lines[3].starts_with("b ") && lines[3].ends_with("down: gone, 4s ago"), "{lines:?}");
    }

    #[test]
    fn the_shipped_mcp_health_vigy() {
        let script = Script::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../examples/vigy/mcp-health.b"), "tick");
        let run = |probes: Vec<Probe>| {
            let f = Facts::default();
            f.set(probes);
            arnes_blue::invoke("vigy", &script, &[&f.json()], "").unwrap().text().to_owned()
        };
        assert_eq!(run(vec![]), "no MCP servers");
        assert_eq!(run(vec![probe("a", true), probe("b", false)]), "1/2 up: a 12ms, b down");
    }

    #[tokio::test]
    async fn a_failing_blue_body_is_a_failed_run_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut exts = vigy_eval::standard_extensions();
        exts.push(extension(Facts::default()));
        let rt = RuntimeHandle::open_in_memory_for(HOST).await.unwrap();
        let v = Vigy::new("t", program(&blue(&dir, "def tick(f)\n  raise(:boom, \"boom\")\nend\n")), TickInterval::default()).unwrap();
        let id = v.id.clone();
        rt.register_or_update(v).await.unwrap();
        let run = rt.tick_now_with(&id, &vigy_eval::LispReconciler::with_extensions(rt.get(&id).await.unwrap().program, exts)).await.unwrap();
        let st = Status { name: "t".into(), every: Duration::from_secs(1), last: Some(run) };
        assert!(st.text().starts_with("failed:") && st.text().contains("health.b#tick") && st.text().contains("call failed"), "{}", st.text());
    }

    #[tokio::test]
    async fn registration_labels_refuses_alone_and_retires_the_undeclared() {
        let dir = tempfile::tempdir().unwrap();
        let s = blue(&dir, "def tick(f)\n  \"ok\"\nend\n");
        let d = |name: &str, ms: u64| Decl { name: name.into(), script: s.clone(), every: Duration::from_millis(ms) };
        let (t, refused) = Ticking::in_memory(vec![d("health", 5000), d("fast", 10)], Facts::default()).await;
        assert_eq!(refused.len(), 1);
        assert!(refused[0].contains("fast"));
        assert_eq!(t.cadence(), Some(Duration::from_millis(5000)));
        let all = t.rt.list(None).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].labels.get(HOST_LABEL), Some(HOST));
        assert_eq!(t.status().await[0].text(), "not run yet");

        let (t2, _) = Ticking::register(t.rt.clone(), vec![d("other", 5000)]).await.unwrap();
        let listed = t2.rt.list(None).await.unwrap();
        let health = listed.iter().find(|v| v.name == "arnes/health").unwrap();
        assert!(!health.enabled, "no longer declared, so disabled but kept");
        assert!(t2.tick("health").await.is_err());
    }
}

/// The fleet-wide MCP probe lease (docs/profiles.md §1), in the shared
/// `vigy.db`: one kv entry per server fingerprint, `{pid, at_ms}`, written
/// when a process sees the server healthy. Another process that finds a
/// fresh entry skips its routine probe of that server.
pub struct Lease {
    store: vigy_store::Store,
    id: VigyId,
    pid: u32,
}

impl Lease {
    /// The key space every arnes process shares.
    const NAMESPACE: &'static str = "arnes/mcp-lease";

    /// # Errors
    /// The store cannot be opened.
    pub async fn open(path: &Path) -> Result<Self, String> {
        let store = vigy_store::Store::open(path).await.map_err(|e| e.to_string())?;
        Ok(Self::with(store, std::process::id()))
    }

    fn with(store: vigy_store::Store, pid: u32) -> Self {
        Self { store, id: VigyId::from_name_and_program(Self::NAMESPACE, ""), pid }
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }

    /// Whether another process saw `fingerprint` healthy within `within`.
    pub async fn fresh(&self, fingerprint: &str, within: Duration) -> bool {
        let Ok(kv) = self.store.load_kv(&self.id).await else { return false };
        let Some(entry) = kv.get(fingerprint) else { return false };
        let pid = entry.get("pid").and_then(serde_json::Value::as_u64);
        let at = entry.get("at_ms").and_then(serde_json::Value::as_u64).unwrap_or(0);
        let age = Self::now_ms().saturating_sub(at);
        pid.is_some_and(|p| p != u64::from(self.pid)) && u128::from(age) < within.as_millis()
    }

    /// Record that this process just saw `fingerprint` healthy.
    pub async fn take(&self, fingerprint: &str) {
        let entry = serde_json::json!({"pid": self.pid, "at_ms": Self::now_ms()});
        let dirty = std::iter::once((fingerprint.to_owned(), entry)).collect();
        if let Err(e) = self.store.save_kv(&self.id, &dirty, &std::collections::BTreeSet::new()).await {
            tracing::debug!(error = %e, "mcp lease not written");
        }
    }
}

#[cfg(test)]
mod lease_tests {
    use super::*;

    #[tokio::test]
    async fn another_process_lease_is_fresh_within_the_interval_and_ones_own_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vigy.db");
        let me = Lease::with(vigy_store::Store::open(&path).await.unwrap(), 1);
        let other = Lease::with(vigy_store::Store::open(&path).await.unwrap(), 2);
        let long = Duration::from_secs(60);
        assert!(!me.fresh("fp", long).await, "no entry, no lease");
        other.take("fp").await;
        assert!(me.fresh("fp", long).await, "another process just saw it healthy");
        assert!(!other.fresh("fp", long).await, "a process never defers to itself");
        assert!(!me.fresh("fp", Duration::ZERO).await, "outside the interval it is stale");
        assert!(!me.fresh("other-fp", long).await, "per fingerprint");
    }
}
