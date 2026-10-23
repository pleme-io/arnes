//! `arnes-mcp-client` — MCP client over stdio, on `rmcp`.
//!
//! [`ServerSpec`] describes how to launch a server; [`McpPool`] launches
//! every server in a registry, lists their tools, and routes calls. The
//! transport is `rmcp`'s child-process client — the same pattern umbra and
//! mado already run, pinned to the rmcp line kaname and mado use (0.15) so
//! the fleet carries one copy.
//!
//! Two properties matter beyond "it calls tools":
//!
//! - **The tool list is frozen and sorted** by `(server, tool)` at connect
//!   time. Tools sit at the very front of every request, so an order that
//!   drifts between turns invalidates the provider's prefix cache from byte
//!   zero (docs/token-efficiency.md §4.2).
//! - **A failed server is reported, never silently absent.** A server that
//!   will not start is a named entry in [`McpPool::failures`]; a `required`
//!   one fails [`McpPool::connect`] outright. "No tools" and "could not ask"
//!   are different answers.

pub mod health;

use arnes_core::{Tool, ToolInputSchema, ToolResult};
use rmcp::model::{CallToolRequestParams, RawContent};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("server {server} exited before responding")]
    ServerGone { server: String },
    #[error("server {server} reported: {message}")]
    ServerError { server: String, message: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("server {server} did not answer within {secs}s")]
    Timeout { server: String, secs: u64 },
    #[error("unknown tool {0:?} (not mcp__<server>__<tool>, or no such server)")]
    UnknownTool(String),
    #[error("required server {server} failed to start: {message}")]
    RequiredFailed { server: String, message: String },
}

pub type Result<T> = std::result::Result<T, McpError>;

/// How to launch one MCP server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerSpec {
    /// Arbitrary identifier — also the prefix for tool names in the loop.
    pub name: String,
    /// Command to execute.
    pub command: String,
    /// Args passed to the command.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variable overrides for the child.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Working directory — defaults to the current dir if unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// When true, the loop should consider this server's failures fatal.
    #[serde(default)]
    pub required: bool,
    /// Not started with the pool: dormant until its first call
    /// (docs/profiles.md §1).
    #[serde(default)]
    pub lazy: bool,
}

impl ServerSpec {
    #[must_use]
    pub fn simple(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            required: false,
            lazy: false,
        }
    }

    /// What makes two declarations the same server: the command, its
    /// arguments, environment and directory (not the name).
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut env: Vec<_> = self.env.iter().collect();
        env.sort();
        serde_json::json!([self.command, self.args, env, self.cwd]).to_string()
    }
}

/// A glob set over server names; an invalid glob is refused by itself.
fn globs(patterns: &[String]) -> (globset::GlobSet, Vec<String>) {
    let mut b = globset::GlobSetBuilder::new();
    let mut refused = Vec::new();
    for p in patterns {
        match globset::Glob::new(p) {
            Ok(g) => {
                b.add(g);
            }
            Err(e) => refused.push(format!("server glob {p:?}: {e}")),
        }
    }
    (b.build().unwrap_or_else(|_| globset::GlobSet::empty()), refused)
}

/// A registry of server configs — typically hydrated from shikumi
/// (`~/.config/arnes/arnes.lisp`) or from anvil's generated MCP list.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct McpRegistry {
    pub servers: Vec<ServerSpec>,
}

impl McpRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, spec: ServerSpec) {
        self.servers.push(spec);
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ServerSpec> {
        self.servers.iter().find(|s| s.name == name)
    }

    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.servers.iter().map(|s| s.name.as_str()).collect()
    }

    /// Add every server of `other` whose name is not taken yet.
    pub fn merge(&mut self, other: Self) {
        for s in other.servers {
            if self.get(&s.name).is_none() {
                self.register(s);
            }
        }
    }

    /// Drop a server declared twice under two names (the same command,
    /// arguments and environment would start twice); the first name stays.
    /// Returns one note per dropped entry.
    pub fn dedup(&mut self) -> Vec<String> {
        let mut seen: HashMap<String, String> = HashMap::new();
        let mut notes = Vec::new();
        self.servers.retain(|s| {
            if let Some(first) = seen.get(&s.fingerprint()) {
                notes.push(format!("MCP server {} is {first} declared again; kept {first}", s.name));
                return false;
            }
            seen.insert(s.fingerprint(), s.name.clone());
            true
        });
        notes
    }

    /// Keep the servers `only` names (all when empty) and `off` does not,
    /// and mark the `lazy` ones dormant. A bad glob is refused alone.
    pub fn select(&mut self, only: &[String], off: &[String], lazy: &[String]) -> Vec<String> {
        let (only_set, mut refused) = globs(only);
        let (off_set, r) = globs(off);
        refused.extend(r);
        let (lazy_set, r) = globs(lazy);
        refused.extend(r);
        self.servers.retain(|s| (only.is_empty() || only_set.is_match(&s.name)) && !off_set.is_match(&s.name));
        for s in &mut self.servers {
            if lazy_set.is_match(&s.name) {
                s.lazy = true;
            }
        }
        refused
    }
}

/// A tool invocation ready to submit to one server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub server: String,
    pub tool: String,
    pub arguments: serde_json::Value,
}

/// The result of one tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

impl ToolOutcome {
    #[must_use]
    pub fn into_result(self) -> ToolResult {
        ToolResult {
            tool_use_id: self.tool_use_id,
            content: self.content,
            is_error: self.is_error,
        }
    }
}

/// An "anvil-native" constructor — accepts the JSON shape anvil emits.
///
/// Anvil writes to `~/.<agent>/mcp.json` with shape:
///   { "mcpServers": { "name": { "command": "...", "args": [...], "env": {...} } } }
///
/// We read that and produce a registry.
#[must_use]
pub fn from_anvil_json(input: &serde_json::Value) -> McpRegistry {
    let mut reg = McpRegistry::new();
    let Some(servers) = input.get("mcpServers").and_then(|v| v.as_object()) else {
        return reg;
    };
    for (name, def) in servers {
        let command = def
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let args = def
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(ToString::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let env = def
            .get("env")
            .and_then(|v| v.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        reg.register(ServerSpec {
            name: name.clone(),
            command,
            args,
            env,
            cwd: None,
            required: false,
            lazy: false,
        });
    }
    reg
}

/// Prefix every MCP tool carries on the wire — Claude Code's convention,
/// so prompts and permission rules written for one read the same here.
pub const TOOL_PREFIX: &str = "mcp__";

/// Deferred exposure's search meta-tool.
pub const SEARCH_TOOL: &str = "mcp_search";
/// Deferred exposure's call meta-tool.
pub const CALL_TOOL: &str = "mcp_call";

/// How MCP tools are presented to the model.
///
/// Every advertised schema rides at the front of every request. Twenty
/// servers are ~800 tools and ~350 KB per request (measured 2026-09-26) —
/// past some providers' tool-count limits outright. `Deferred` sends two
/// fixed meta-tools instead: the prefix stays small and byte-stable, and
/// schemas arrive in tool results only when the model asks (the approach
/// Claude Code takes with deferred tools; docs/token-efficiency.md §4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exposure {
    All,
    Deferred,
    /// `All` up to `max_inline` tools, `Deferred` beyond.
    Auto { max_inline: usize },
}

impl Exposure {
    /// The default: inline while small, deferred once it would bloat.
    pub const DEFAULT: Self = Self::Auto { max_inline: 40 };

    /// # Errors
    /// Names the accepted values.
    pub fn parse(s: &str) -> std::result::Result<Self, String> {
        match s {
            "all" => Ok(Self::All),
            "deferred" => Ok(Self::Deferred),
            "auto" => Ok(Self::DEFAULT),
            other => Err(format!("unknown mcp-tools {other:?}; expected one of [\"auto\", \"all\", \"deferred\"]")),
        }
    }
}

/// Providers accept `^[a-zA-Z0-9_-]{1,64}$` tool names.
const MAX_TOOL_NAME: usize = 64;

/// `mcp__<server>__<tool>`.
#[must_use]
pub fn qualified_name(server: &str, tool: &str) -> String {
    format!("{TOOL_PREFIX}{server}__{tool}")
}

/// Split `mcp__<server>__<tool>` back into its parts. The server name is
/// everything up to the first `__` after the prefix.
#[must_use]
pub fn split_name(name: &str) -> Option<(&str, &str)> {
    name.strip_prefix(TOOL_PREFIX)?.split_once("__")
}

fn valid_wire_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_TOOL_NAME
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

type Client = RunningService<RoleClient, ()>;

/// One server's liveness, as [`McpPool::probe`] measured it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Probe {
    pub server: String,
    pub up: bool,
    /// `None` when no request was made (the server never connected).
    pub latency_ms: Option<u64>,
    pub tools: usize,
    pub error: Option<String>,
}

/// What [`McpPool::reconcile`] did, by server name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reconciled {
    /// Unchanged, left as they were.
    pub kept: Vec<String>,
    /// New or changed, now connected.
    pub started: Vec<String>,
    /// Removed or changed; no longer in the pool.
    pub stopped: Vec<String>,
    /// New or changed, and could not start.
    pub failed: Vec<(String, String)>,
    /// New or changed lazy servers, left dormant until their first call.
    pub dormant: Vec<String>,
}

impl Reconciled {
    /// Whether anything changed.
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.started.is_empty() && self.stopped.is_empty() && self.failed.is_empty() && self.dormant.is_empty()
    }
}

/// A server's connection and its listed tools, or why it could not start.
type Connected = std::result::Result<(std::sync::Arc<Client>, Vec<rmcp::model::Tool>), String>;

/// One server's connect attempt, finished: attached with
/// [`McpPool::with_arrival`] as it comes in, so a pool fills while the TUI
/// runs (docs/profiles.md §1).
pub struct Arrival {
    pub spec: ServerSpec,
    result: Connected,
}

impl Arrival {
    /// Why it could not start, when it could not.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.result.as_ref().err().map(String::as_str)
    }
}

/// A woken dormant server: its connection and its listed tools, or why it
/// could not start.
type Woken = std::result::Result<(std::sync::Arc<Client>, Vec<Tool>), String>;

/// A lazy server: not started until its first call, then started once and
/// shared by every caller and every pool that keeps it.
struct Dormant {
    spec: ServerSpec,
    timeout: Duration,
    stderr_dir: Option<PathBuf>,
    woken: tokio::sync::OnceCell<Woken>,
}

/// Every configured server, connected, with its tools listed once.
pub struct McpPool {
    /// Shared, so a reconciled pool keeps an unchanged server's live
    /// connection; a server leaves when the last pool holding it is dropped.
    clients: BTreeMap<String, std::sync::Arc<Client>>,
    tools: Vec<Tool>,
    /// Each tool's MCP `readOnlyHint` — the server's own declaration.
    read_only: HashMap<String, bool>,
    failures: Vec<(String, String)>,
    skipped: Vec<String>,
    dormant: BTreeMap<String, std::sync::Arc<Dormant>>,
    call_timeout: Duration,
}

impl McpPool {
    /// A pool with no servers — tools off.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            clients: BTreeMap::new(),
            tools: Vec::new(),
            read_only: HashMap::new(),
            failures: Vec::new(),
            skipped: Vec::new(),
            dormant: BTreeMap::new(),
            call_timeout: Duration::from_secs(120),
        }
    }

    /// A copy sharing every connection.
    fn fork(&self) -> Self {
        Self {
            clients: self.clients.clone(),
            tools: self.tools.clone(),
            read_only: self.read_only.clone(),
            failures: self.failures.clone(),
            skipped: self.skipped.clone(),
            dormant: self.dormant.clone(),
            call_timeout: self.call_timeout,
        }
    }

    /// Start one server and list its tools, bounded by `timeout`. Never
    /// fails: the error is in the [`Arrival`].
    pub async fn connect_server(spec: ServerSpec, timeout: Duration, stderr_dir: Option<&std::path::Path>) -> Arrival {
        let result = match tokio::time::timeout(timeout, connect_one(&spec, stderr_dir)).await {
            Ok(r) => r.map(|(c, t)| (std::sync::Arc::new(c), t)),
            Err(_) => Err(format!("no handshake within {}s", timeout.as_secs())),
        };
        Arrival { spec, result }
    }

    /// This pool with `arrival` attached: its tools join the sorted list, or
    /// its failure is recorded (replacing an older one for the same server).
    #[must_use]
    pub fn with_arrival(&self, arrival: Arrival) -> Self {
        let mut pool = self.fork();
        pool.failures.retain(|(n, _)| *n != arrival.spec.name);
        pool.drop_server(&arrival.spec.name);
        pool.add(arrival.spec, arrival.result);
        pool.tools.sort_by(|a, b| a.name.cmp(&b.name));
        pool
    }

    /// This pool without `server` (its process ends when no pool holds it).
    #[must_use]
    pub fn without(&self, server: &str) -> Self {
        let mut pool = self.fork();
        pool.drop_server(server);
        pool.failures.retain(|(n, _)| n != server);
        pool
    }

    /// Record `spec` as dormant: listed, started on its first call.
    pub fn add_dormant(&mut self, spec: ServerSpec, timeout: Duration, stderr_dir: Option<PathBuf>) {
        self.dormant.insert(
            spec.name.clone(),
            std::sync::Arc::new(Dormant { spec, timeout, stderr_dir, woken: tokio::sync::OnceCell::new() }),
        );
    }

    /// The dormant servers' names, woken or not.
    #[must_use]
    pub fn dormant(&self) -> Vec<&str> {
        self.dormant.keys().map(String::as_str).collect()
    }

    fn drop_server(&mut self, server: &str) {
        let mine = |wire: &str| split_name(wire).is_some_and(|(srv, _)| srv == server);
        self.clients.remove(server);
        self.tools.retain(|t| !mine(&t.name));
        self.read_only.retain(|w, _| !mine(w));
        self.skipped.retain(|w| !mine(w));
    }

    /// Add one server's connect result.
    fn add(&mut self, spec: ServerSpec, result: Connected) {
        match result {
            Ok((client, tools)) => {
                for t in tools {
                    let wire = qualified_name(&spec.name, &t.name);
                    if !valid_wire_name(&wire) {
                        tracing::warn!(tool = %wire, "MCP tool name not valid on the wire; skipped");
                        self.skipped.push(wire);
                        continue;
                    }
                    if let Some(ro) = t.annotations.as_ref().and_then(|a| a.read_only_hint) {
                        self.read_only.insert(wire.clone(), ro);
                    }
                    self.tools.push(to_tool(wire, t));
                }
                self.clients.insert(spec.name.clone(), client);
            }
            Err(message) => {
                // Recorded in `failures`; the caller reports it to the
                // operator once. A warn here too showed every one twice.
                tracing::debug!(server = %spec.name, %message, "MCP server unavailable");
                self.failures.push((spec.name, message));
            }
        }
    }

    /// One server's liveness: `tools/list`, timed. A server that is not
    /// connected answers from its failure, or as dormant, without a request.
    pub async fn probe_one(&self, server: &str, timeout: Duration) -> Probe {
        let Some(client) = self.clients.get(server).cloned().or_else(|| self.woken(server).map(|(c, _)| c.clone())) else {
            let error = self
                .failures
                .iter()
                .find(|(n, _)| n == server)
                .map_or_else(|| if self.dormant.contains_key(server) { "dormant".to_owned() } else { "not configured".to_owned() }, |(_, w)| w.clone());
            return Probe { server: server.to_owned(), up: false, latency_ms: None, tools: 0, error: Some(error) };
        };
        let start = std::time::Instant::now();
        let r = tokio::time::timeout(timeout, client.list_all_tools()).await;
        let latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (up, tools, error) = match r {
            Ok(Ok(t)) => (true, t.len(), None),
            Ok(Err(e)) => (false, 0, Some(format!("tools/list: {e}"))),
            Err(_) => (false, 0, Some(format!("tools/list: no answer within {}ms", timeout.as_millis()))),
        };
        Probe { server: server.to_owned(), up, latency_ms: Some(latency_ms), tools, error }
    }

    /// A dormant server's connection, once woken.
    fn woken(&self, server: &str) -> Option<&(std::sync::Arc<Client>, Vec<Tool>)> {
        self.dormant.get(server)?.woken.get()?.as_ref().ok()
    }

    /// Start a dormant server (once; concurrent callers share the start).
    async fn wake(&self, server: &str) -> Option<std::result::Result<&(std::sync::Arc<Client>, Vec<Tool>), String>> {
        let d = self.dormant.get(server)?;
        let r = d
            .woken
            .get_or_init(|| async {
                let a = Self::connect_server(d.spec.clone(), d.timeout, d.stderr_dir.as_deref()).await;
                a.result.map(|(c, tools)| {
                    let tools = tools
                        .into_iter()
                        .map(|t| to_tool(qualified_name(server, &t.name), t))
                        .filter(|t| valid_wire_name(&t.name))
                        .collect();
                    (c, tools)
                })
            })
            .await;
        Some(r.as_ref().map_err(Clone::clone))
    }

    /// Launch every server in `registry` concurrently, handshake, and list
    /// tools. A server that fails is recorded in [`Self::failures`]; one
    /// marked `required` fails the whole connect.
    ///
    /// # Errors
    /// [`McpError::RequiredFailed`] when a `required` server cannot start.
    pub async fn connect(registry: &McpRegistry, connect_timeout: Duration) -> Result<Self> {
        Self::connect_logging(registry, connect_timeout, None).await
    }

    /// [`Self::connect`], with each server's stderr appended to
    /// `<stderr_dir>/<server>.log`. With no directory it is discarded —
    /// never inherited: a child writing to the terminal paints over the
    /// TUI (seen: a Node server's `TypeError` across arnes's frame).
    ///
    /// # Errors
    /// As [`Self::connect`].
    pub async fn connect_logging(
        registry: &McpRegistry,
        connect_timeout: Duration,
        stderr_dir: Option<std::path::PathBuf>,
    ) -> Result<Self> {
        let mut pool = Self::empty();
        let (lazy, eager): (Vec<_>, Vec<_>) = registry.servers.iter().cloned().partition(|s| s.lazy);
        for spec in lazy {
            pool.add_dormant(spec, connect_timeout, stderr_dir.clone());
        }
        pool.attach(&eager, connect_timeout, stderr_dir.as_deref(), true).await?;
        Ok(pool)
    }

    /// Start `specs` concurrently and add what they list. A failure is
    /// recorded in [`Self::failures`]; with `fail_required`, a `required`
    /// server's failure is returned instead.
    async fn attach(
        &mut self,
        specs: &[ServerSpec],
        connect_timeout: Duration,
        stderr_dir: Option<&std::path::Path>,
        fail_required: bool,
    ) -> Result<()> {
        let attempts = specs.iter().map(|spec| Self::connect_server(spec.clone(), connect_timeout, stderr_dir));
        for Arrival { spec, result } in futures::future::join_all(attempts).await {
            if let Err(message) = &result
                && spec.required
                && fail_required
            {
                return Err(McpError::RequiredFailed { server: spec.name, message: message.clone() });
            }
            self.add(spec, result);
        }
        // Frozen, sorted: the request prefix stays byte-stable across turns.
        self.tools.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(())
    }

    /// The pool `next` describes, reached from this one (built from
    /// `previous`) without touching what did not change: a server whose spec
    /// is the same keeps its connection and its tools, one that is new or
    /// changed is started, one that left or changed is dropped from the new
    /// pool (its process ends when the last pool holding it goes). A server
    /// that failed before and is unchanged is not retried. A `required`
    /// server that fails here is recorded, not fatal: the session is running.
    pub async fn reconcile(
        &self,
        previous: &McpRegistry,
        next: &McpRegistry,
        connect_timeout: Duration,
        stderr_dir: Option<&std::path::Path>,
    ) -> (Self, Reconciled) {
        let unchanged = |spec: &ServerSpec| previous.get(&spec.name) == Some(spec);
        let mut pool = Self::empty();
        pool.call_timeout = self.call_timeout;
        let mut report = Reconciled::default();
        let mut start = Vec::new();
        for spec in &next.servers {
            if spec.lazy {
                match self.dormant.get(&spec.name) {
                    Some(d) if unchanged(spec) => {
                        pool.dormant.insert(spec.name.clone(), d.clone());
                        report.kept.push(spec.name.clone());
                    }
                    _ => {
                        pool.add_dormant(spec.clone(), connect_timeout, stderr_dir.map(std::path::Path::to_path_buf));
                        report.dormant.push(spec.name.clone());
                    }
                }
                continue;
            }
            if unchanged(spec) && let Some(client) = self.clients.get(&spec.name) {
                pool.clients.insert(spec.name.clone(), client.clone());
                let mine = |wire: &str| split_name(wire).is_some_and(|(srv, _)| srv == spec.name);
                pool.tools.extend(self.tools.iter().filter(|t| mine(&t.name)).cloned());
                pool.read_only.extend(self.read_only.iter().filter(|(w, _)| mine(w)).map(|(w, r)| (w.clone(), *r)));
                pool.skipped.extend(self.skipped.iter().filter(|w| mine(w)).cloned());
                report.kept.push(spec.name.clone());
            } else if unchanged(spec) && let Some(f) = self.failures.iter().find(|(n, _)| *n == spec.name) {
                pool.failures.push(f.clone());
                report.kept.push(spec.name.clone());
            } else {
                start.push(spec.clone());
            }
        }
        report.stopped = self
            .clients
            .keys()
            .filter(|name| !pool.clients.contains_key(*name))
            .cloned()
            .collect();
        let before: Vec<String> = pool.failures.iter().map(|(n, _)| n.clone()).collect();
        // Never fails: `fail_required` is off.
        let _ = pool.attach(&start, connect_timeout, stderr_dir, false).await;
        for spec in &start {
            match pool.failures.iter().find(|(n, _)| *n == spec.name && !before.contains(n)) {
                Some(f) => report.failed.push(f.clone()),
                None => report.started.push(spec.name.clone()),
            }
        }
        (pool, report)
    }

    /// One cheap liveness check per server: `tools/list`, timed. A server
    /// that never connected answers from its connect failure without a
    /// request. Probes run concurrently.
    pub async fn probe(&self, timeout: Duration) -> Vec<Probe> {
        let woken = self.dormant.keys().filter_map(|n| self.woken(n).map(|(c, _)| (n, c)));
        let live = self.clients.iter().chain(woken).map(|(name, client)| async move {
            let start = std::time::Instant::now();
            let r = tokio::time::timeout(timeout, client.list_all_tools()).await;
            let latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
            let (up, tools, error) = match r {
                Ok(Ok(t)) => (true, t.len(), None),
                Ok(Err(e)) => (false, 0, Some(format!("tools/list: {e}"))),
                Err(_) => (false, 0, Some(format!("tools/list: no answer within {}ms", timeout.as_millis()))),
            };
            Probe { server: name.clone(), up, latency_ms: Some(latency_ms), tools, error }
        });
        let mut out: Vec<Probe> = futures::future::join_all(live).await;
        out.extend(self.failures.iter().map(|(server, why)| Probe {
            server: server.clone(),
            up: false,
            latency_ms: None,
            tools: 0,
            error: Some(why.clone()),
        }));
        out.sort_by(|a, b| a.server.cmp(&b.server));
        out
    }

    /// Per-call timeout (default 120 s).
    #[must_use]
    pub fn with_call_timeout(mut self, t: Duration) -> Self {
        self.call_timeout = t;
        self
    }

    /// Every tool, sorted by wire name.
    #[must_use]
    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    /// What to send the model each turn under `exposure` — deterministic, so
    /// the request prefix is identical turn to turn.
    #[must_use]
    pub fn exposed_tools(&self, exposure: Exposure) -> Vec<Tool> {
        let deferred = match exposure {
            Exposure::All => false,
            Exposure::Deferred => true,
            Exposure::Auto { max_inline } => self.tools.len() > max_inline,
        };
        let dormant = self.dormant.keys().cloned().collect::<Vec<_>>().join(", ");
        let dormant_note = if dormant.is_empty() {
            String::new()
        } else {
            format!(" Dormant servers, started by their first call: {dormant}; an unknown tool name on one lists its tools.")
        };
        let call_tool = Tool {
            name: CALL_TOOL.into(),
            description: format!(
                "Call an MCP tool found with {SEARCH_TOOL}. `name` is the full mcp__<server>__<tool> \
                 name; `arguments` must match that tool's input_schema.{dormant_note}"
            ),
            input_schema: ToolInputSchema(serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "arguments": {"type": "object"}
                },
                "required": ["name"]
            })),
        };
        if !deferred || self.tools.is_empty() {
            let mut out = self.tools.clone();
            if !self.dormant.is_empty() {
                out.push(call_tool);
                out.sort_by(|a, b| a.name.cmp(&b.name));
            }
            return out;
        }
        let servers = self.servers().join(", ");
        vec![
            Tool {
                name: CALL_TOOL.into(),
                description: call_tool.description,
                input_schema: ToolInputSchema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "arguments": {"type": "object"}
                    },
                    "required": ["name"]
                })),
            },
            Tool {
                name: SEARCH_TOOL.into(),
                description: format!(
                    "Search the {} available MCP tools by keywords; returns names, descriptions and \
                     input schemas. Connected servers: {servers}.",
                    self.tools.len()
                ),
                input_schema: ToolInputSchema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "keywords, e.g. \"kubernetes pod logs\""},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 25}
                    },
                    "required": ["query"]
                })),
            },
        ]
    }

    /// Rank tools against `query` with the fleet fuzzy scorer (`shibori`),
    /// best first; ties break by name so results are stable.
    #[must_use]
    pub fn search(&self, query: &str, limit: usize) -> Vec<&Tool> {
        let policy = shibori::Policy::insensitive_terms();
        let woken = self.dormant.keys().filter_map(|n| self.woken(n)).flat_map(|(_, t)| t.iter());
        let mut scored: Vec<(i32, &Tool)> = self
            .tools
            .iter()
            .chain(woken)
            .filter_map(|t| {
                let hay = format!("{} {}", t.name, t.description);
                shibori::fuzzy_score(query, &hay, policy).map(|sc| (sc, t))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
        scored.into_iter().take(limit.clamp(1, 25)).map(|(_, t)| t).collect()
    }

    /// The tool's MCP `readOnlyHint`: `Some(true)` when its server declares
    /// it side-effect free. `None` when the server said nothing — which the
    /// permission gate treats as "may write", never as "read-only".
    #[must_use]
    pub fn read_only(&self, wire_name: &str) -> Option<bool> {
        self.read_only.get(wire_name).copied()
    }

    /// Servers that could not be reached, with why.
    #[must_use]
    pub fn failures(&self) -> &[(String, String)] {
        &self.failures
    }

    /// Tools dropped because their qualified name is invalid on the wire.
    #[must_use]
    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }

    /// The connected servers, woken dormant ones included, sorted.
    #[must_use]
    pub fn servers(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.clients.keys().map(String::as_str).collect();
        out.extend(self.dormant.keys().filter(|n| self.woken(n).is_some()).map(String::as_str));
        out.sort_unstable();
        out
    }

    /// Call `mcp__<server>__<tool>` and return its text content joined by
    /// newlines. An MCP-level error result (`isError`) comes back as
    /// [`McpError::ServerError`] so the loop marks the `tool_result` an error.
    ///
    /// # Errors
    /// Unknown tool, timeout, transport failure, or a tool-reported error.
    pub async fn call(&self, wire_name: &str, input: &serde_json::Value) -> Result<String> {
        match wire_name {
            SEARCH_TOOL => return Ok(self.search_json(input)),
            CALL_TOOL => {
                let name = input.get("name").and_then(serde_json::Value::as_str).unwrap_or_default();
                let arguments = input.get("arguments").cloned().unwrap_or(serde_json::json!({}));
                return Box::pin(self.call(name, &arguments)).await;
            }
            _ => {}
        }
        let (server, tool) =
            split_name(wire_name).ok_or_else(|| McpError::UnknownTool(wire_name.into()))?;
        let client = match self.clients.get(server) {
            Some(c) => c,
            None => match self.wake(server).await {
                None => return Err(McpError::UnknownTool(wire_name.into())),
                Some(Err(message)) => return Err(McpError::ServerError { server: server.into(), message }),
                Some(Ok((c, tools))) => {
                    if !tools.iter().any(|t| t.name == wire_name) {
                        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
                        return Err(McpError::ServerError {
                            server: server.into(),
                            message: format!("no tool {tool:?}; it serves {}", names.join(", ")),
                        });
                    }
                    c
                }
            },
        };
        let arguments = match input {
            serde_json::Value::Object(m) => Some(m.clone()),
            serde_json::Value::Null => None,
            other => {
                return Err(McpError::ServerError {
                    server: server.into(),
                    message: format!("tool input must be a JSON object, got {other}"),
                });
            }
        };
        let call = client.call_tool(CallToolRequestParams {
            name: tool.to_owned().into(),
            arguments,
            meta: None,
            task: None,
        });
        let result = tokio::time::timeout(self.call_timeout, call)
            .await
            .map_err(|_| McpError::Timeout { server: server.into(), secs: self.call_timeout.as_secs() })?
            .map_err(|e| McpError::ServerError { server: server.into(), message: e.to_string() })?;
        let text = result
            .content
            .iter()
            .filter_map(|c| match &c.raw {
                RawContent::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if result.is_error.unwrap_or(false) {
            return Err(McpError::ServerError { server: server.into(), message: text });
        }
        Ok(text)
    }
}

impl McpPool {
    fn search_json(&self, input: &serde_json::Value) -> String {
        let query = input.get("query").and_then(serde_json::Value::as_str).unwrap_or_default();
        let limit = input
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(8, |n| usize::try_from(n).unwrap_or(25));
        let hits: Vec<serde_json::Value> = self
            .search(query, limit)
            .into_iter()
            .map(|t| {
                let mut d = t.description.clone();
                if d.chars().count() > 400 {
                    d = d.chars().take(400).collect::<String>() + "…";
                }
                serde_json::json!({"name": t.name, "description": d, "input_schema": t.input_schema.0})
            })
            .collect();
        serde_json::json!({"query": query, "matches": hits}).to_string()
    }

    /// A pool over fixed tools with no servers — for tests of code built
    /// on the pool (search, exposure, the permission gate).
    #[doc(hidden)]
    #[must_use]
    pub fn with_tools(tools: Vec<Tool>, read_only: &[&str]) -> Self {
        let mut p = Self::empty();
        p.read_only = read_only.iter().map(|n| ((*n).to_owned(), true)).collect();
        p.tools = tools;
        p
    }
}

fn to_tool(wire: String, t: rmcp::model::Tool) -> Tool {
    Tool {
        name: wire,
        description: t.description.map(|d| d.to_string()).unwrap_or_default(),
        input_schema: ToolInputSchema(serde_json::Value::Object((*t.input_schema).clone())),
    }
}

/// Where a server's stderr goes: its log file when one can be opened,
/// otherwise nowhere.
fn stderr_for(dir: Option<&std::path::Path>, server: &str) -> std::process::Stdio {
    dir.and_then(|d| {
        std::fs::create_dir_all(d).ok()?;
        std::fs::OpenOptions::new().create(true).append(true).open(d.join(format!("{server}.log"))).ok()
    })
    .map_or_else(std::process::Stdio::null, std::process::Stdio::from)
}

async fn connect_one(
    spec: &ServerSpec,
    stderr_dir: Option<&std::path::Path>,
) -> std::result::Result<(Client, Vec<rmcp::model::Tool>), String> {
    let mut cmd = tokio::process::Command::new(&spec.command);
    cmd.args(&spec.args).envs(&spec.env);
    if let Some(dir) = &spec.cwd {
        cmd.current_dir(dir);
    }
    let (transport, _) = TokioChildProcess::builder(cmd)
        .stderr(stderr_for(stderr_dir, &spec.name))
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", spec.command))?;
    let client = ().serve(transport).await.map_err(|e| format!("handshake: {e}"))?;
    let tools = client.list_all_tools().await.map_err(|e| format!("tools/list: {e}"))?;
    Ok((client, tools))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn registry_basic() {
        let mut r = McpRegistry::new();
        r.register(ServerSpec::simple("escriba-mcp", "escriba-mcp"));
        r.register(ServerSpec::simple("caixa-lsp", "caixa-lsp"));
        assert!(r.get("escriba-mcp").is_some());
        assert_eq!(r.names().len(), 2);
    }

    #[test]
    fn parses_anvil_json_shape() {
        let input = json!({
            "mcpServers": {
                "escriba-mcp": {
                    "command": "/nix/store/xxx/bin/escriba-mcp",
                    "args": [],
                    "env": { "RUST_LOG": "info" }
                },
                "github": {
                    "command": "/nix/store/yyy/bin/github-mcp",
                    "args": ["--token", "xxx"]
                }
            }
        });
        let reg = from_anvil_json(&input);
        assert_eq!(reg.servers.len(), 2);
        let escriba = reg.get("escriba-mcp").unwrap();
        assert!(escriba.command.ends_with("escriba-mcp"));
        assert_eq!(
            escriba.env.get("RUST_LOG").map(String::as_str),
            Some("info")
        );
    }

    #[test]
    fn tool_outcome_lifts_to_core_result() {
        let out = ToolOutcome {
            tool_use_id: "t1".into(),
            content: "done".into(),
            is_error: false,
        };
        let r = out.into_result();
        assert_eq!(r.tool_use_id, "t1");
        assert!(!r.is_error);
    }

    #[test]
    fn empty_anvil_json_gives_empty_registry() {
        let input = json!({});
        let reg = from_anvil_json(&input);
        assert!(reg.servers.is_empty());
    }

    #[test]
    fn qualified_names_round_trip() {
        let n = qualified_name("escriba-mcp", "open_file");
        assert_eq!(n, "mcp__escriba-mcp__open_file");
        assert_eq!(split_name(&n), Some(("escriba-mcp", "open_file")));
        assert_eq!(split_name("read_file"), None);
        // A tool whose own name contains `__` keeps it.
        assert_eq!(split_name("mcp__srv__a__b"), Some(("srv", "a__b")));
    }

    #[test]
    fn wire_name_validation() {
        assert!(valid_wire_name("mcp__codesearch__search_exact"));
        assert!(!valid_wire_name("mcp__srv__has space"));
        assert!(!valid_wire_name(&"x".repeat(65)));
    }

    #[tokio::test]
    async fn empty_registry_is_an_empty_pool() {
        let pool = McpPool::connect(&McpRegistry::new(), Duration::from_secs(1)).await.unwrap();
        assert!(pool.tools().is_empty() && pool.failures().is_empty());
    }

    #[tokio::test]
    async fn probe_of_nothing_is_nothing_and_a_dead_server_probes_down_without_a_request() {
        assert!(McpPool::empty().probe(Duration::from_millis(50)).await.is_empty());
        let mut reg = McpRegistry::new();
        reg.register(ServerSpec::simple("ghost", "/nonexistent/arnes-test-mcp"));
        let pool = McpPool::connect(&reg, Duration::from_secs(2)).await.unwrap();
        let p = pool.probe(Duration::from_millis(50)).await;
        assert_eq!(p.len(), 1);
        assert!(!p[0].up && p[0].latency_ms.is_none() && p[0].error.is_some());
    }

    #[tokio::test]
    async fn a_server_that_cannot_start_is_reported_not_silent() {
        let mut reg = McpRegistry::new();
        reg.register(ServerSpec::simple("ghost", "/nonexistent/arnes-test-mcp"));
        let pool = McpPool::connect(&reg, Duration::from_secs(2)).await.unwrap();
        assert_eq!(pool.failures().len(), 1);
        assert_eq!(pool.failures()[0].0, "ghost");
    }

    #[tokio::test]
    async fn a_required_server_that_cannot_start_fails_connect() {
        let mut reg = McpRegistry::new();
        let mut spec = ServerSpec::simple("must", "/nonexistent/arnes-test-mcp");
        spec.required = true;
        reg.register(spec);
        assert!(matches!(
            McpPool::connect(&reg, Duration::from_secs(2)).await,
            Err(McpError::RequiredFailed { .. })
        ));
    }

    #[tokio::test]
    async fn calling_an_unknown_tool_is_a_typed_error() {
        let pool = McpPool::empty();
        assert!(matches!(
            pool.call("mcp__nope__x", &serde_json::json!({})).await,
            Err(McpError::UnknownTool(_))
        ));
    }

    fn tool(name: &str, desc: &str) -> Tool {
        Tool {
            name: name.into(),
            description: desc.into(),
            input_schema: ToolInputSchema(serde_json::json!({"type": "object"})),
        }
    }

    fn many(n: usize) -> Vec<Tool> {
        let mut v: Vec<Tool> = (0..n).map(|i| tool(&format!("mcp__s__t{i:03}"), "filler")).collect();
        v.push(tool("mcp__kubernetes__get_pod_logs", "Read the logs of a Kubernetes pod"));
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    #[test]
    fn auto_inlines_small_sets_and_defers_large_ones() {
        let small = McpPool::with_tools(many(5), &[]);
        assert_eq!(small.exposed_tools(Exposure::DEFAULT).len(), 6);
        let big = McpPool::with_tools(many(100), &[]);
        let exposed = big.exposed_tools(Exposure::DEFAULT);
        let names: Vec<_> = exposed.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, [CALL_TOOL, SEARCH_TOOL], "two fixed meta-tools, sorted");
    }

    #[test]
    fn deferred_exposure_is_byte_stable() {
        let p = McpPool::with_tools(many(100), &[]);
        let a = serde_json::to_string(&p.exposed_tools(Exposure::Deferred)).unwrap();
        let b = serde_json::to_string(&p.exposed_tools(Exposure::Deferred)).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn search_ranks_the_relevant_tool_first() {
        let p = McpPool::with_tools(many(100), &[]);
        let hits = p.search("pod logs", 5);
        assert_eq!(hits[0].name, "mcp__kubernetes__get_pod_logs");
    }

    #[tokio::test]
    async fn mcp_search_meta_tool_returns_schemas_as_json() {
        let p = McpPool::with_tools(many(10), &[]);
        let out = p.call(SEARCH_TOOL, &serde_json::json!({"query": "kubernetes"})).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["matches"][0]["name"], "mcp__kubernetes__get_pod_logs");
        assert!(v["matches"][0]["input_schema"].is_object());
    }

    #[tokio::test]
    async fn mcp_call_routes_to_the_named_tool() {
        let p = McpPool::with_tools(many(3), &[]);
        let r = p.call(CALL_TOOL, &serde_json::json!({"name": "mcp__ghost__x", "arguments": {}})).await;
        assert!(matches!(r, Err(McpError::UnknownTool(n)) if n == "mcp__ghost__x"));
    }

    #[test]
    fn dedup_keeps_the_first_name_of_a_server_declared_twice() {
        let mut r = McpRegistry::new();
        r.register(ServerSpec::simple("a", "srv"));
        r.register(ServerSpec::simple("b", "srv"));
        r.register(ServerSpec::simple("c", "other"));
        let notes = r.dedup();
        assert_eq!(r.names(), ["a", "c"]);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("b is a"));
        let mut none = McpRegistry::new();
        assert!(none.dedup().is_empty());
    }

    #[test]
    fn merge_takes_only_new_names() {
        let mut r = McpRegistry::new();
        r.register(ServerSpec::simple("a", "one"));
        let mut other = McpRegistry::new();
        other.register(ServerSpec::simple("a", "two"));
        other.register(ServerSpec::simple("b", "three"));
        r.merge(other);
        assert_eq!(r.names(), ["a", "b"]);
        assert_eq!(r.get("a").unwrap().command, "one");
    }

    #[test]
    fn select_keeps_only_drops_off_marks_lazy_and_refuses_a_bad_glob_alone() {
        let mk = || {
            let mut r = McpRegistry::new();
            for n in ["splunk-a", "splunk-b", "github", "codesearch"] {
                r.register(ServerSpec::simple(n, n));
            }
            r
        };
        let mut r = mk();
        assert!(r.select(&[], &[], &[]).is_empty());
        assert_eq!(r.names().len(), 4, "empty only keeps all");
        let mut r = mk();
        let refused = r.select(&["splunk-*".into(), "github".into(), "[".into()], &["splunk-b".into()], &["splunk-*".into()]);
        assert_eq!(refused.len(), 1);
        assert_eq!(r.names(), ["splunk-a", "github"]);
        assert!(r.get("splunk-a").unwrap().lazy && !r.get("github").unwrap().lazy);
    }

    #[tokio::test]
    async fn a_dormant_server_is_listed_offered_mcp_call_and_not_probed_until_woken() {
        let mut p = McpPool::with_tools(many(2), &[]);
        p.add_dormant(ServerSpec::simple("sleepy", "/nonexistent/arnes-test-mcp"), Duration::from_secs(1), None);
        assert_eq!(p.dormant(), ["sleepy"]);
        let exposed = p.exposed_tools(Exposure::All);
        assert!(exposed.iter().any(|t| t.name == CALL_TOOL && t.description.contains("sleepy")));
        assert!(exposed.windows(2).all(|w| w[0].name <= w[1].name), "still sorted");
        assert!(p.probe(Duration::from_millis(50)).await.is_empty());
        assert_eq!(p.probe_one("sleepy", Duration::from_millis(50)).await.error.as_deref(), Some("dormant"));
        let r = p.call("mcp__sleepy__x", &serde_json::json!({})).await;
        assert!(matches!(r, Err(McpError::ServerError { ref server, .. }) if server == "sleepy"), "{r:?}");
        let plain = McpPool::with_tools(many(2), &[]);
        assert!(!plain.exposed_tools(Exposure::All).iter().any(|t| t.name == CALL_TOOL));
    }

    #[test]
    fn exposure_parses_and_refuses_by_name() {
        assert_eq!(Exposure::parse("deferred"), Ok(Exposure::Deferred));
        assert!(Exposure::parse("some").unwrap_err().contains("auto"));
    }
}
