//! `arnes-config` — shikumi-loaded configuration.
//!
//! `~/.config/arnes/arnes.lisp` or `.yaml` or `.nix` — users pick. All
//! three paths deserialize into the same [`ArnesConfig`] struct. The
//! tatara-lisp authoring form is first-class per the ecosystem standard:
//!
//! ```lisp
//! (defarnes
//!   :provedor           "local"             ; local (default, free) | deepseek | anthropic
//!   :modelo             "deepseek-flash"    ; omit → the provider's default
//!   :effort             "low"               ; low | high | max (implies thinking on)
//!   :api-key-file       "~/.config/deepseek/api-key"
//!   :prompt-sistema     "You are arnes — a focused Rust engineer."
//!   :tema               "nord"
//!   :mcp-servers        ((:nome "escriba-mcp" :command "escriba-mcp")
//!                        (:nome "caixa-lsp"   :command "caixa-lsp")
//!                        (:nome "github"      :command "mcp-github"
//!                         :env   (:GITHUB_TOKEN "$GITHUB_TOKEN")))
//!   :anvil-json         "~/.cursor/mcp.json"
//!   :fallbacks          ((:provedor "local" :endpoint "http://gpubox:8080"))
//!   :max-tokens         8192)
//! ```

extern crate self as arnes_config;

pub mod render;
pub mod salvage;
pub mod sections;

pub use sections::{
    AccessSection, ActivitySection, CatalogSection, ContextSection, FreeRouteSection, KeyEdits, KeysSection, McpHealthSection,
    HistorySection, McpSection, MentionsSection, NavKeysSection, NoticesSection, PersistenceSection, RegionsSection, RouteFacts, RouteSection, RoutingSection,
    SkillsSection, TimingSection, TurnSection, UiSection,
};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use arnes_anthropic::catalog::Auth as CatalogAuth;
use arnes_core::{EchoRule, Effort, ThinkingMode};
use std::path::PathBuf;
use tatara_lisp::DeriveTataraDomain;

/// The top-level config, authorable in lisp / yaml / nix.
#[derive(
    DeriveTataraDomain, Serialize, Deserialize, schemars::JsonSchema, Debug, Clone, PartialEq,
)]
// snake_case: shikumi lowers `:max-tokens` to `max_tokens`; camelCase silently dropped every multi-word key.
#[serde(rename_all = "snake_case")]
#[tatara(keyword = "defarnes")]
pub struct ArnesConfig {
    /// Model provider — `local` (default: a llama.cpp server, free),
    /// `deepseek` or `anthropic`. Read through [`ArnesConfig::provider`],
    /// which refuses an unknown name.
    #[serde(default = "default_provedor")]
    pub provedor: String,
    /// Model ID. Empty → the provider's default model.
    #[serde(default)]
    pub modelo: String,
    /// File holding the API key (a sops-nix secret path). Wins over the
    /// provider's env var.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_file: Option<PathBuf>,
    /// API endpoint override. Unset → the provider's endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// System prompt — loaded into every conversation's `:system` slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_sistema: Option<String>,
    /// Blackmatter theme variant — dark / light. Defaults to dark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tema: Option<String>,
    /// Max tokens per response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// MCP servers to spawn and expose as tools.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerDecl>,
    /// Path to an anvil-generated `mcp.json` — when set, merged with
    /// the explicit `mcp_servers` list above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anvil_json: Option<PathBuf>,
    /// Env var holding the key when no `:api-key-file` is set. Unset →
    /// the provider's (`DEEPSEEK_API_KEY` / `ANTHROPIC_API_KEY`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_env: Option<String>,
    /// `anthropic-version` header. Unset → `2023-06-01`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,

    // ── Sampling — every field unset = the provider's default ──────────
    /// 0.0–2.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperatura: Option<f32>,
    /// 0.0–1.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,

    // ── Reasoning ──────────────────────────────────────────────────────
    /// `on` | `off`. Unset → the provider default (`DeepSeek`: on/high),
    /// unless `effort` is set, which implies `on`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// `low` | `high` | `max` → `output_config.effort`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Thinking token budget. Only sent to providers that honour it
    /// (`DeepSeek` ignores it; arnes warns rather than send a no-op).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u32>,
    /// Re-send prior thinking blocks. Unset → the provider's rule
    /// (`DeepSeek`: no — it rejects them; Anthropic: yes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub echo_thinking: Option<bool>,

    // ── Transport ──────────────────────────────────────────────────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Seconds a route may take to accept a connection before it counts as
    /// unreachable and the next route gets the turn. 1..=120, unset → 10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_timeout_secs: Option<u32>,
    /// Retries on transient statuses (429/5xx/…). Unset → 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    /// First retry backoff in ms, doubling per attempt. Unset → 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_base_ms: Option<u64>,
    /// guardrail screening of Bash commands and Write/Edit content: `on`
    /// (default) or `off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardrail: Option<String>,
    /// Built-in tools (Read/Write/Edit/Glob/Grep/Bash). Unset → on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin_tools: Option<bool>,
    /// The usage ledger file (forge-core cost events, one per API call).
    /// Unset → `$XDG_STATE_HOME/arnes/usage.jsonl`; `"off"` → no file
    /// (session totals are still shown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger: Option<String>,
    /// Record each TUI session under `$XDG_STATE_HOME/arnes/sessions` so it
    /// can be resumed: `on` (default) or `off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<String>,
    /// Saved sessions kept, newest first. Unset → 200; 0 → unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions_keep: Option<u32>,
    /// Saved sessions older than this are removed. Unset → 90; 0 → never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions_max_age_days: Option<u32>,
    /// A log past this size is rotated to `.1` at startup. Unset → 8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_max_mb: Option<u32>,
    // ── Screen (docs/design.md) — every field unset = the built-in look ──
    /// Animation: `on` (default) or `off` (every surface drawn at its end
    /// state).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion: Option<String>,
    /// Most rows the notice lane opens to. Unset → 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice_rows: Option<u32>,
    /// Seconds an info / warning / error notice stays in the lane. Unset →
    /// 4 / 8 / 12.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice_info_secs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice_warn_secs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice_error_secs: Option<u32>,
    /// Rows a picker claims above the prompt. Unset → 12.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picker_rows: Option<u32>,
    /// Desktop notifications (OSC 9/777/99 + bell) when approval is needed
    /// or a long turn finishes: `auto` (default) or `off` (bell only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notifications: Option<String>,
    /// Prompt editing: `normal` (Claude Code / readline keys, default) or
    /// `vim` (escriba's vim; Insert mode keeps the Claude Code keys).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_mode: Option<String>,
    /// `default` (ask before non-read-only tools) | `plan` (read-only only)
    /// | `bypass` (never ask).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// Tool-name globs that run without asking (`mcp__codesearch__*`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_tools: Vec<String>,
    /// Tool-name globs that never run. Deny beats allow.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny_tools: Vec<String>,
    /// How MCP tools reach the model: `auto` (default — inline up to 40,
    /// deferred beyond), `all`, or `deferred` (two meta-tools).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_tools: Option<String>,
    /// Extra HTTP headers on every request (proxies, gateways, tracing).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_headers: Vec<HeaderDecl>,
    /// Routes tried in order when the provider above cannot serve a turn
    /// (unreachable, out of credit, rate limited, failing), and started on
    /// when its key cannot be read at all. Authored as plain keyword lists,
    /// `((:provedor "local" :timeout-secs 600))`, which is what shikumi (the
    /// load path) lowers; `tests::loads_the_form_the_hm_module_renders` pins
    /// it through that path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks: Vec<RouteDecl>,

    /// Slash commands implemented in blue:
    /// `((:name "wc" :description "…" :blue "~/.config/arnes/wc.b" :fn "run"))`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<CommandDecl>,
    /// Model-callable tools implemented in blue, gated like every other tool.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDecl>,
    /// Hooks implemented in blue, on `before-submit`, `after-turn` or
    /// `before-tool`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<HookDecl>,
    /// Always-on reconcilers implemented in blue, ticked by the shared vigy
    /// runtime for host `arnes`:
    /// `((:name "mcp-health" :blue "~/.config/arnes/mcp-health.b" :fn "tick" :every-ms 5000))`.
    /// None declared → no vigy runtime and no MCP probing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vigies: Vec<VigyDecl>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endpoints: Vec<EndpointDecl>,
    /// Hosted bidama entries (commands, segments) turned off by name, e.g. `("help")`
    /// (`docs/kernel.md`). Off is not deleted: see `kernel_fallback`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modules_off: Vec<String>,
    /// Serve a feature from its Rust twin in the kernel when its bidama
    /// entry is off. Unset → no: the module is the implementation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_fallback: Option<bool>,
    /// Wall-clock budget for one blue command call from the prompt, in ms.
    /// Over it, the call's effects are dropped and a notice names it.
    /// Unset → 250.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_budget_ms: Option<u64>,
    /// Bidamas to host beside arnes's own (`meirei`, `joutai`), by name, each
    /// resolved on arnes's bidama path and asked for its exports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bidamas: Vec<String>,

    /// Appended to the system prompt while the primary route answers: what
    /// the model is and where it runs (a fallback carries its own). Kept out
    /// of `:prompt-sistema` so switching routes never tells the next model
    /// it is the last one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_note: Option<String>,

    // ── Sections (docs/configuration.md). Each is optional as a whole and
    // field by field; a nested key wins over its flat twin above. ────────
    #[serde(default, skip_serializing_if = "sections::is_default")]
    pub ui: UiSection,
    #[serde(default, skip_serializing_if = "sections::is_default")]
    pub keys: KeysSection,
    #[serde(default, skip_serializing_if = "sections::is_default")]
    pub activity: ActivitySection,
    #[serde(default, skip_serializing_if = "sections::is_default")]
    pub mcp: McpSection,
    #[serde(default, skip_serializing_if = "sections::is_default")]
    pub persistence: PersistenceSection,
    #[serde(default, skip_serializing_if = "sections::is_default")]
    pub free_route: FreeRouteSection,
    #[serde(default, skip_serializing_if = "sections::is_default")]
    pub route: RouteSection,
    #[serde(default, skip_serializing_if = "sections::is_default")]
    pub routing: RoutingSection,
    #[serde(default, skip_serializing_if = "sections::is_default")]
    pub catalog: CatalogSection,
    #[serde(default, skip_serializing_if = "sections::is_default")]
    pub mentions: MentionsSection,
    #[serde(default, skip_serializing_if = "sections::is_default")]
    pub history: HistorySection,
    #[serde(default, skip_serializing_if = "sections::is_default")]
    pub turn: TurnSection,
    #[serde(default, skip_serializing_if = "sections::is_default")]
    pub skills: SkillsSection,
    #[serde(default, skip_serializing_if = "sections::is_default")]
    pub context: ContextSection,
    #[serde(default, skip_serializing_if = "sections::is_default")]
    pub access: AccessSection,

    /// The profile used when the command line, this directory's last choice
    /// and the `choose-profile` hook name none (docs/profiles.md §2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Named config overlays, switched live with `/profile` or `--profile`:
    /// `((:name "ops" :inherits ("base") :config (:permission-mode "plan")))`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<ProfileDecl>,

    /// Free-form map for agent-specific knobs (escriba socket path, etc).
    #[serde(default)]
    pub extras: HashMap<String, String>,
}

/// One profile: a named overlay onto the config, applied over its parents
/// (docs/profiles.md §2). `:config` holds any top-level keys and sections,
/// written as in `(defarnes …)`; each is refused alone when it does not fit.
#[derive(
    DeriveTataraDomain, Serialize, Deserialize, schemars::JsonSchema, Debug, Clone, PartialEq,
)]
#[serde(rename_all = "snake_case")]
#[tatara(keyword = "defarnes-profile")]
pub struct ProfileDecl {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Parents applied first, left to right.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inherits: Vec<String>,
    /// The overlay.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub config: serde_json::Value,
}

/// One extra HTTP header — `(:name "x-trace" :value "on")`.
#[derive(
    DeriveTataraDomain, Serialize, Deserialize, schemars::JsonSchema, Debug, Clone, PartialEq,
)]
#[tatara(keyword = "defheader")]
pub struct HeaderDecl {
    pub name: String,
    pub value: String,
}

/// One fallback route — `(:provedor "local" :endpoint "http://gpubox:8080")`.
/// The same provider facts as the top level; everything unset takes the
/// provider's default, and the sampling/transport knobs are shared with the
/// primary except the echo rule, which is always the route's own.
#[derive(
    DeriveTataraDomain, Serialize, Deserialize, schemars::JsonSchema, Debug, Clone, PartialEq, Default,
)]
#[serde(rename_all = "snake_case")]
#[tatara(keyword = "defroute")]
pub struct RouteDecl {
    pub provedor: String,
    #[serde(default)]
    pub modelo: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_env: Option<String>,
    /// Whole-request timeout for this route. Unset → the primary's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Appended to the system prompt when THIS route answers: what the model
    /// is and where it runs. A model knows nothing of its host, and asked
    /// about it a local one invents a plausible one (gpubox's said "Ubuntu
    /// container, GPT-4-Turbo").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_note: Option<String>,
    /// Whether this route is sent the tool list. Unset → yes. Off for a
    /// model whose context cannot hold it (Apple's on-device model: 4096
    /// tokens, against hundreds of tool descriptions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    /// The route's name in notices and the status line. Unset → the
    /// provider's name; two `local` routes need telling apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "sections::is_default")]
    pub facts: RouteFacts,
}

/// A slash command implemented in blue — `(defarnes-command :name "wc" :blue
/// "wc.b" :fn "run")`. The function receives the text typed after `/name` and
/// returns a string (shown as a hint) or `nil`. The keyword is prefixed
/// because `defcommand`, `deftool` and `defhook` are already claimed in the
/// fleet's keyword ledger (escriba, kura, frost).
#[derive(
    DeriveTataraDomain, Serialize, Deserialize, schemars::JsonSchema, Debug, Clone, PartialEq,
)]
#[serde(rename_all = "snake_case")]
#[tatara(keyword = "defarnes-command")]
pub struct CommandDecl {
    pub name: String,
    /// The `/` picker's one-line description.
    #[serde(default)]
    pub description: String,
    /// The `.b` file (`~` expands).
    pub blue: PathBuf,
    /// The function in it to call.
    pub r#fn: String,
}

/// A vigy implemented in blue — `(defarnes-vigy :name "mcp-health" :blue
/// "mcp-health.b" :fn "tick" :every-ms 5000)`. The function receives the MCP
/// probe facts as a JSON string and returns the tick's one-line result.
#[derive(
    DeriveTataraDomain, Serialize, Deserialize, schemars::JsonSchema, Debug, Clone, PartialEq,
)]
#[serde(rename_all = "snake_case")]
#[tatara(keyword = "defarnes-vigy")]
pub struct VigyDecl {
    pub name: String,
    /// The `.b` file (`~` expands).
    pub blue: PathBuf,
    /// The function in it to call.
    pub r#fn: String,
    /// Tick interval in milliseconds. Unset → 5000; under 100 is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_ms: Option<u64>,
}

#[derive(
    DeriveTataraDomain, Serialize, Deserialize, schemars::JsonSchema, Debug, Clone, PartialEq, Default,
)]
#[serde(rename_all = "snake_case")]
#[tatara(keyword = "defarnes-endpoint")]
pub struct EndpointDecl {
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_kib: Option<u32>,
}

impl EndpointDecl {
    /// # Errors
    /// An unreadable key, an unknown auth, a timeout or size out of bounds,
    /// or a key sent in the clear to a host off this machine.
    pub fn resolve(&self) -> Result<arnes_anthropic::reach::Endpoint, String> {
        let key = match (&self.key_file, &self.key_env) {
            (None, None) => None,
            (file, var) => read_key(Provider::Messages, file.as_ref(), var.as_deref())?,
        };
        let loopback = ["http://127.0.0.1", "http://localhost", "http://[::1]"].iter().any(|h| self.url.starts_with(h));
        if key.is_some() && self.url.starts_with("http://") && !loopback {
            return Err(format!("{} sends a key in the clear to {}", self.name, self.url));
        }
        let (timeout, bad_timeout) = sections::bounded("timeout_ms", self.timeout_ms, 100, 10_000, 700);
        let (max_kib, bad_size) = sections::bounded("max_kib", self.max_kib, 1, 4096, 256);
        if let Some(bad) = bad_timeout.or(bad_size) {
            return Err(bad);
        }
        Ok(arnes_anthropic::reach::Endpoint {
            name: self.name.clone(),
            url: self.url.clone(),
            key: key.map(std::sync::Arc::new),
            auth: auth_of(Provider::Messages, self.auth.as_deref())?,
            timeout: std::time::Duration::from_millis(u64::from(timeout)),
            max_bytes: usize::try_from(max_kib).unwrap_or(256) * 1024,
        })
    }
}

impl CommandDecl {
    #[must_use]
    pub fn script(&self) -> arnes_blue::Script {
        arnes_blue::Script::new(self.blue.clone(), self.r#fn.clone())
    }
}

/// A model-callable tool implemented in blue — `(defarnes-tool :name "shout"
/// :description "…" :blue "shout.b" :fn "shout" :schema "{…}" :read-only #t)`.
/// The function receives the tool input as a JSON string.
#[derive(
    DeriveTataraDomain, Serialize, Deserialize, schemars::JsonSchema, Debug, Clone, PartialEq,
)]
#[serde(rename_all = "snake_case")]
#[tatara(keyword = "defarnes-tool")]
pub struct ToolDecl {
    pub name: String,
    pub description: String,
    pub blue: PathBuf,
    pub r#fn: String,
    /// The input JSON Schema, as JSON text. Unset: an object with no
    /// declared properties.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Whether the tool changes nothing. Unset: it may, so it is asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
}

impl ToolDecl {
    /// # Errors
    /// The schema is not JSON, or not a JSON object.
    pub fn spec(&self) -> Result<arnes_blue::ToolSpec, String> {
        let schema = match &self.schema {
            None => None,
            Some(text) => match serde_json::from_str::<serde_json::Value>(text) {
                Ok(v) if v.is_object() => Some(v),
                Ok(_) => return Err(format!("tool {}: :schema must be a JSON object", self.name)),
                Err(e) => return Err(format!("tool {}: :schema is not JSON: {e}", self.name)),
            },
        };
        Ok(arnes_blue::ToolSpec {
            name: self.name.clone(),
            description: self.description.clone(),
            schema,
            read_only: self.read_only.unwrap_or(false),
            script: arnes_blue::Script::new(self.blue.clone(), self.r#fn.clone()),
        })
    }
}

/// A hook implemented in blue — `(defarnes-hook :on "before-tool" :blue
/// "hooks.b" :fn "guard")`. `:on` is `before-submit`, `after-turn` or
/// `before-tool`.
#[derive(
    DeriveTataraDomain, Serialize, Deserialize, schemars::JsonSchema, Debug, Clone, PartialEq,
)]
#[serde(rename_all = "snake_case")]
#[tatara(keyword = "defarnes-hook")]
pub struct HookDecl {
    pub on: String,
    pub blue: PathBuf,
    pub r#fn: String,
}

/// A fallback, resolved: its provider parsed and its key read.
#[derive(Debug)]
pub struct Route {
    pub provider: Provider,
    pub model: String,
    pub endpoint: String,
    pub key: Option<cofre_secret::Secret>,
    pub timeout: Option<std::time::Duration>,
    pub system_note: Option<String>,
    pub tools: bool,
    pub label: String,
    pub auth: CatalogAuth,
    pub facts: RouteFacts,
}

/// What a config change touches ([`ArnesConfig::reload_plan`]). Each flag
/// is one independent part of the session to swap.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ReloadPlan {
    /// The server list or the anvil file changed: reconcile the pool.
    pub mcp_servers: bool,
    /// Tool calls get a new timeout.
    pub mcp_call_timeout: bool,
    /// The tool list sent to the model changes (exposure, built-ins, blue).
    pub tools: bool,
    /// The permission gate is rebuilt (built-ins, guardrail, blue tools,
    /// before-tool hooks). Session "always" answers carry over.
    pub gate: bool,
    /// The primary route moved (another provider or endpoint).
    pub route: bool,
    /// Retention changed: sweep again now.
    pub sweep: bool,
    /// The declared vigies changed: redeclare them on the running runtime.
    pub vigies: bool,
    /// What cannot apply in this session, each with its reason.
    pub restart: Vec<&'static str>,
}

/// The bounded knobs outside the screen, resolved ([`ArnesConfig::knobs`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Knobs {
    pub mcp_connect: std::time::Duration,
    pub mcp_call: std::time::Duration,
    pub catalog_max_age: std::time::Duration,
    pub mention_max_files: usize,
    pub mention_max_bytes: u64,
    pub max_rounds: u32,
    pub notify_after: std::time::Duration,
    pub frame_rate: u32,
    pub idle_poll: std::time::Duration,
}

/// The prompt history, typed (`history.*`, docs/configuration.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct HistoryKnobs {
    /// Prompts are written to and read from disk.
    pub record: bool,
    pub scope: arnes_store::history::Scope,
    /// Newest entries kept on disk; 0 = all.
    pub keep: usize,
    pub search: HistorySearchStyle,
    /// Order the picker by recency instead of frecency.
    pub recency: bool,
    /// The newest prompt stays first in the picker.
    pub pin_recent: bool,
}

impl Default for HistoryKnobs {
    fn default() -> Self {
        ArnesConfig::default().history_knobs().0
    }
}

/// What `Ctrl-R` opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HistorySearchStyle {
    /// The ranked fuzzy picker over the prompt history.
    #[default]
    Picker,
    /// Reverse-i-search in the prompt box, newest match first.
    Incremental,
}

/// A key that lives in a section and, from before the sections, flat at the
/// top level: the nested value when set, else the flat one. One line per key,
/// so the precedence rule is written once.
macro_rules! nested_or_flat {
    ($($(#[$m:meta])* $name:ident -> $t:ty = $sec:ident . $($field:ident).+ | $flat:ident;)*) => {
        impl ArnesConfig {
            $(
                $(#[$m])*
                #[must_use]
                pub fn $name(&self) -> Option<$t> {
                    self.$sec.$($field).+.clone().or_else(|| self.$flat.clone())
                }
            )*
        }
    };
}

nested_or_flat! {
    /// `ui.theme`, else `:tema`.
    theme -> String = ui.theme | tema;
    /// `ui.motion`, else `:motion`.
    motion_mode -> String = ui.motion | motion;
    /// `ui.notifications`, else `:notifications`.
    notifications_mode -> String = ui.notifications | notifications;
    /// `ui.editor-mode`, else `:editor-mode`.
    editor_mode_name -> String = ui.editor_mode | editor_mode;
    /// `ui.regions.notice-rows`, else `:notice-rows`.
    notice_rows_setting -> u32 = ui.regions.notice_rows | notice_rows;
    /// `ui.regions.picker-rows`, else `:picker-rows`.
    picker_rows_setting -> u32 = ui.regions.picker_rows | picker_rows;
    /// `ui.notices.info-secs`, else `:notice-info-secs`.
    notice_info_setting -> u32 = ui.notices.info_secs | notice_info_secs;
    /// `ui.notices.warn-secs`, else `:notice-warn-secs`.
    notice_warn_setting -> u32 = ui.notices.warn_secs | notice_warn_secs;
    /// `ui.notices.error-secs`, else `:notice-error-secs`.
    notice_error_setting -> u32 = ui.notices.error_secs | notice_error_secs;
    /// `mcp.exposure`, else `:mcp-tools`.
    mcp_tools_mode -> String = mcp.exposure | mcp_tools;
    /// `persistence.sessions`, else `:sessions`.
    sessions_mode -> String = persistence.sessions | sessions;
    /// `persistence.sessions-keep`, else `:sessions-keep`.
    sessions_keep -> u32 = persistence.sessions_keep | sessions_keep;
    /// `persistence.sessions-max-age-days`, else `:sessions-max-age-days`.
    sessions_max_age_days -> u32 = persistence.sessions_max_age_days | sessions_max_age_days;
    /// `persistence.log-max-mb`, else `:log-max-mb`.
    log_max_mb -> u32 = persistence.log_max_mb | log_max_mb;
    /// `persistence.ledger`, else `:ledger`.
    ledger_setting -> String = persistence.ledger | ledger;
}

/// Read a key: the file if one is named, else the env var (`var` is the
/// route's override or its provider's). Absent is `Ok(None)` only for a
/// provider that takes no key.
fn read_key(
    provider: Provider,
    file: Option<&PathBuf>,
    var: Option<&str>,
) -> Result<Option<cofre_secret::Secret>, String> {
    if let Some(path) = file {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("reading api-key-file {}: {e}", path.display()))?;
        return cofre_secret::Secret::new(raw.trim())
            .map(Some)
            .map_err(|_| format!("api-key-file {} is empty", path.display()));
    }
    let var = var.unwrap_or(provider.key_env());
    match cofre_secret::Secret::from_env(var) {
        Ok(k) => Ok(Some(k)),
        Err(_) if !provider.requires_key() => Ok(None),
        Err(cofre_secret::SecretError::Empty) => Err(format!("{var} is set but empty")),
        Err(_) => Err(format!("{var} is unset and no :api-key-file is configured")),
    }
}

impl RouteDecl {
    /// # Errors
    /// An unknown provider, or a key the provider requires that cannot be read.
    pub fn resolve(&self) -> Result<Route, String> {
        let provider = Provider::parse(&self.provedor)?;
        let endpoint = match (&self.endpoint, provider) {
            (Some(e), _) => e.clone(),
            (None, Provider::Messages) => return Err("a messages route names its :endpoint".into()),
            (None, p) => p.endpoint().into(),
        };
        let model = match (self.modelo.is_empty(), provider) {
            (false, _) => self.modelo.clone(),
            (true, Provider::Messages) => return Err("a messages route names its :modelo".into()),
            (true, p) => p.default_model().into(),
        };
        Ok(Route {
            provider,
            model,
            endpoint,
            key: read_key(provider, self.api_key_file.as_ref(), self.key_env.as_deref())?,
            timeout: self.timeout_secs.map(std::time::Duration::from_secs),
            system_note: self.system_note.clone(),
            tools: self.tools.unwrap_or(true),
            label: self.label.clone().unwrap_or_else(|| provider.name().to_owned()),
            auth: auth_of(provider, self.facts.auth.as_deref())?,
            facts: self.facts.clone(),
        })
    }
}

fn auth_of(provider: Provider, named: Option<&str>) -> Result<CatalogAuth, String> {
    match named {
        None => Ok(provider.default_auth()),
        Some("x-api-key") => Ok(CatalogAuth::XApiKey),
        Some("bearer") => Ok(CatalogAuth::Bearer),
        Some(other) => Err(format!("unknown auth {other:?}; expected \"x-api-key\" or \"bearer\"")),
    }
}

impl Default for ArnesConfig {
    fn default() -> Self {
        Self {
            provedor: default_provedor(),
            modelo: String::new(),
            api_key_file: None,
            endpoint: None,
            prompt_sistema: None,
            tema: Some("dark".into()),
            max_tokens: Some(arnes_core::DEFAULT_MAX_TOKENS),
            mcp_servers: Vec::new(),
            anvil_json: None,
            key_env: None,
            api_version: None,
            temperatura: None,
            top_p: None,
            top_k: None,
            stop_sequences: Vec::new(),
            thinking: None,
            effort: None,
            thinking_budget: None,
            echo_thinking: None,
            timeout_secs: None,
            connect_timeout_secs: None,
            max_retries: None,
            retry_base_ms: None,
            extra_headers: Vec::new(),
            fallbacks: Vec::new(),
            commands: Vec::new(),
            tools: Vec::new(),
            hooks: Vec::new(),
            vigies: Vec::new(),
            endpoints: Vec::new(),
            modules_off: Vec::new(),
            kernel_fallback: None,
            module_budget_ms: None,
            bidamas: Vec::new(),
            mcp_tools: None,
            permission_mode: None,
            editor_mode: None,
            notifications: None,
            ledger: None,
            sessions: None,
            sessions_keep: None,
            sessions_max_age_days: None,
            log_max_mb: None,
            motion: None,
            notice_rows: None,
            notice_info_secs: None,
            notice_warn_secs: None,
            notice_error_secs: None,
            picker_rows: None,
            builtin_tools: None,
            guardrail: None,
            allow_tools: Vec::new(),
            deny_tools: Vec::new(),
            system_note: None,
            ui: UiSection::default(),
            keys: KeysSection::default(),
            activity: ActivitySection::default(),
            mcp: McpSection::default(),
            persistence: PersistenceSection::default(),
            free_route: FreeRouteSection::default(),
            route: RouteSection::default(),
            routing: RoutingSection::default(),
            catalog: CatalogSection::default(),
            mentions: MentionsSection::default(),
            history: HistorySection::default(),
            turn: TurnSection::default(),
            skills: SkillsSection::default(),
            context: ContextSection::default(),
            access: AccessSection::default(),
            profile: None,
            profiles: Vec::new(),
            extras: HashMap::new(),
        }
    }
}

/// The default provider: the free local route (operator, 2026-09-28). A paid
/// provider is used once it is configured with a key.
fn default_provedor() -> String {
    Provider::Local.name().into()
}

/// The typed view of `:provedor`. All speak the Anthropic Messages wire
/// format — `DeepSeek` through its Anthropic-compatible endpoint, a local
/// llama.cpp server through its `/v1/messages` route — so a provider is a
/// handful of facts: endpoint, default model, key env var, echo rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    DeepSeek,
    Anthropic,
    /// A llama.cpp `llama-server` (0.5.0+): the fleet's own model. It takes
    /// no key, serves whatever it was started with under an alias, and
    /// normally lives at an address only its `:endpoint` can name.
    Local,
    Messages,
}

impl Provider {
    pub const ALL: [Self; 4] = [Self::DeepSeek, Self::Anthropic, Self::Local, Self::Messages];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::DeepSeek => "deepseek",
            Self::Anthropic => "anthropic",
            Self::Local => "local",
            Self::Messages => "messages",
        }
    }

    #[must_use]
    pub const fn default_auth(self) -> CatalogAuth {
        match self {
            Self::Messages => CatalogAuth::Bearer,
            Self::DeepSeek | Self::Anthropic | Self::Local => CatalogAuth::XApiKey,
        }
    }

    #[must_use]
    pub const fn endpoint(self) -> &'static str {
        match self {
            Self::DeepSeek => "https://api.deepseek.com/anthropic",
            Self::Anthropic => "https://api.anthropic.com",
            // llama-server's own default bind.
            Self::Local => "http://127.0.0.1:8080",
            Self::Messages => "",
        }
    }

    #[must_use]
    pub const fn default_model(self) -> &'static str {
        match self {
            // Live /models (2026-09-26): deepseek-chat/-reasoner are retired
            // and an unknown name is silently routed to flash with thinking on.
            Self::DeepSeek => "deepseek-flash",
            Self::Anthropic => "claude-opus-5-5",
            // The fleet's alias (nix: pleme.llm.server.alias).
            Self::Local => "local",
            Self::Messages => "",
        }
    }

    /// When prior thinking blocks go back in history. `DeepSeek`: with
    /// tools, omitting them is a 400; without, they are ignored. Local:
    /// llama-server signs nothing (`"signature": ""`), so echoing buys
    /// nothing and risks a template that rejects the block.
    #[must_use]
    pub const fn echo_rule(self) -> EchoRule {
        match self {
            Self::DeepSeek => EchoRule::WhenTools,
            Self::Anthropic => EchoRule::Always,
            Self::Local | Self::Messages => EchoRule::Never,
        }
    }

    /// Whether `thinking.budget_tokens` has any effect.
    #[must_use]
    pub const fn honours_thinking_budget(self) -> bool {
        match self {
            Self::DeepSeek | Self::Local | Self::Messages => false,
            Self::Anthropic => true,
        }
    }

    /// The provider's model catalogue endpoint and its auth header. `None`
    /// where there is no catalogue to check against: a local server serves
    /// exactly the model it was started with.
    #[must_use]
    pub const fn models_url(self) -> Option<(&'static str, CatalogAuth)> {
        match self {
            Self::DeepSeek => Some(("https://api.deepseek.com/models", CatalogAuth::Bearer)),
            Self::Anthropic => Some(("https://api.anthropic.com/v1/models", CatalogAuth::XApiKey)),
            Self::Local | Self::Messages => None,
        }
    }

    #[must_use]
    pub const fn key_env(self) -> &'static str {
        match self {
            Self::DeepSeek => "DEEPSEEK_API_KEY",
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::Local => "LOCAL_LLM_API_KEY",
            Self::Messages => "MESSAGES_API_KEY",
        }
    }

    /// Whether a request without a key is refused. A local server started
    /// without `--api-key` takes none; one started with it reads the key
    /// from `:api-key-file` / `:key-env` like any other.
    #[must_use]
    pub const fn requires_key(self) -> bool {
        !matches!(self, Self::Local)
    }

    /// # Errors
    /// Names every accepted provider when `s` is not one of them.
    pub fn parse(s: &str) -> Result<Self, String> {
        Self::ALL.into_iter().find(|p| p.name() == s).ok_or_else(|| {
            let legal: Vec<_> = Self::ALL.iter().map(|p| p.name()).collect();
            format!("unknown provedor {s:?}; expected one of {legal:?}")
        })
    }
}

/// One MCP server to launch — `defmcp-server` in Lisp, same shape as
/// `arnes_mcp_client::ServerSpec` minus the typed paths.
#[derive(
    DeriveTataraDomain, Serialize, Deserialize, schemars::JsonSchema, Debug, Clone, PartialEq,
)]
// snake_case: shikumi lowers `:max-tokens` to `max_tokens`; camelCase silently dropped every multi-word key.
#[serde(rename_all = "snake_case")]
#[tatara(keyword = "defmcp-server")]
pub struct McpServerDecl {
    pub nome: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub required: bool,
}

/// Merge `layer` into `base`: objects key by key (recursively), anything
/// else replaced.
fn merge_json(base: &mut serde_json::Value, layer: serde_json::Value) {
    match (base, layer) {
        (serde_json::Value::Object(b), serde_json::Value::Object(l)) => {
            for (k, v) in l {
                merge_json(b.entry(k).or_insert(serde_json::Value::Null), v);
            }
        }
        (b, l) => *b = l,
    }
}

/// `ARNES_*` env vars override config keys (`ARNES_MODELO`).
pub const ENV_PREFIX: &str = "ARNES_";
/// `ARNES_TIER=bare|discovered|default` picks the tier a file is layered on.
pub const TIER_ENV: &str = "ARNES_TIER";

/// A config as loaded: the value, what was refused, and the documents it was
/// made from.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: ArnesConfig,
    /// One line per refused key, list entry or unknown key.
    pub refused: Vec<String>,
    /// The file as written (keys lowered), before any refusal.
    pub file: serde_json::Value,
    /// The `ARNES_*` overrides.
    pub env: serde_json::Value,
}

impl shikumi::TieredConfig for ArnesConfig {
    /// Zero opinion: the keyless local provider and nothing else chosen.
    fn bare() -> Self {
        Self { tema: None, max_tokens: None, ..Self::default() }
    }

    fn prescribed_default() -> Self {
        Self::default()
    }
}

impl ArnesConfig {
    /// # Errors
    /// When `:provedor` names no known provider.
    pub fn provider(&self) -> Result<Provider, String> {
        Provider::parse(&self.provedor)
    }

    /// The model to request: explicit `:modelo`, else the provider default.
    #[must_use]
    pub fn model(&self) -> String {
        if !self.modelo.is_empty() {
            return self.modelo.clone();
        }
        self.provider()
            .map_or_else(|_| String::new(), |p| p.default_model().to_string())
    }

    /// The endpoint to call: explicit `:endpoint`, else the provider's.
    ///
    /// # Errors
    /// When `:provedor` names no known provider.
    pub fn resolved_endpoint(&self) -> Result<String, String> {
        match &self.endpoint {
            Some(e) => Ok(e.clone()),
            None => self.provider().map(|p| p.endpoint().to_string()),
        }
    }

    /// The API key: `:api-key-file` if set, else the provider's env var.
    /// `None` only for a provider that takes no key.
    ///
    /// # Errors
    /// Names the file or env var that was tried.
    pub fn api_key(&self) -> Result<Option<cofre_secret::Secret>, String> {
        self.key_file_allowed(self.api_key_file.as_ref())?;
        read_key(self.provider()?, self.api_key_file.as_ref(), self.key_env.as_deref())
    }

    /// The fallbacks, resolved in order. A route that cannot be resolved is
    /// refused by itself, named with its reason, and the others proceed: a
    /// missing key on one fallback must not take the rest down with it.
    #[must_use]
    pub fn fallback_routes(&self) -> (Vec<Route>, Vec<String>) {
        let mut routes = Vec::new();
        let mut refused = Vec::new();
        for (i, decl) in self.fallbacks.iter().enumerate() {
            match self.key_file_allowed(decl.api_key_file.as_ref()).and_then(|()| decl.resolve()) {
                Ok(r) => routes.push(r),
                Err(e) => refused.push(format!("fallback {} ({}): {e}", i + 1, decl.provedor)),
            }
        }
        (routes, refused)
    }

    /// This config with fallback `i` as its provider and the rest still
    /// behind it — for when the configured provider cannot be used at all
    /// (its key is unreadable), so the session starts on what can serve.
    #[must_use]
    pub fn promote(&self, i: usize) -> Self {
        let mut next = self.clone();
        let decl = next.fallbacks.remove(i);
        next.provedor = decl.provedor;
        next.modelo = decl.modelo;
        next.endpoint = decl.endpoint;
        next.api_key_file = decl.api_key_file;
        next.key_env = decl.key_env;
        // The route's note travels with the route: it is the primary's now,
        // and the next switch takes it away again.
        next.system_note = decl.system_note;
        if decl.timeout_secs.is_some() {
            next.timeout_secs = decl.timeout_secs;
        }
        next.route = RouteSection { label: decl.label, tools: decl.tools, facts: decl.facts };
        next
    }

    /// The free route to start on when the configured provider cannot be
    /// used and no fallback can: the local llama.cpp server, then (macOS,
    /// unless `free-route.on-device` is off) Apple's on-device model, with
    /// every fallback that still resolves kept behind them. `None` when
    /// `free-route.enabled` is off. A paid provider is used only once its key
    /// is given, so it is always opted into.
    #[must_use]
    pub fn free_route(&self) -> Option<Self> {
        let f = &self.free_route;
        if f.enabled == Some(false) {
            return None;
        }
        let mut next = self.clone();
        next.provedor = Provider::Local.name().into();
        next.modelo.clear();
        next.endpoint.clone_from(&f.endpoint);
        next.api_key_file = None;
        next.key_env = None;
        next.system_note = None;
        next.route = RouteSection::default();
        next.fallbacks.retain(|r| r.resolve().is_ok());
        let on_device = f.on_device.unwrap_or(cfg!(target_os = "macos"));
        if on_device && !next.fallbacks.iter().any(|r| r.label.as_deref() == Some("on-device")) {
            next.fallbacks.push(RouteDecl {
                provedor: Provider::Local.name().into(),
                modelo: f.on_device_model.clone().unwrap_or_else(|| "apple-on-device".into()),
                endpoint: Some(f.on_device_endpoint.clone().unwrap_or_else(|| "http://127.0.0.1:8434".into())),
                api_key_file: None,
                key_env: None,
                timeout_secs: None,
                system_note: None,
                tools: Some(false),
                label: Some("on-device".into()),
                facts: RouteFacts::default(),
            });
        }
        Some(next)
    }

    /// This config with `other`'s route: provider, model, endpoint, key,
    /// route note and fallbacks. A reload that leaves the file's route alone
    /// keeps the route `/model` switched to.
    #[must_use]
    pub fn with_route_of(&self, other: &Self) -> Self {
        let mut next = self.clone();
        next.provedor.clone_from(&other.provedor);
        next.modelo.clone_from(&other.modelo);
        next.endpoint.clone_from(&other.endpoint);
        next.api_key_file.clone_from(&other.api_key_file);
        next.key_env.clone_from(&other.key_env);
        next.system_note.clone_from(&other.system_note);
        next.fallbacks.clone_from(&other.fallbacks);
        next.route.clone_from(&other.route);
        next
    }

    /// Whether `next` routes differently from `self` (the file moved the route).
    #[must_use]
    pub fn route_differs(&self, next: &Self) -> bool {
        self.provedor != next.provedor
            || self.modelo != next.modelo
            || self.endpoint != next.endpoint
            || self.api_key_file != next.api_key_file
            || self.key_env != next.key_env
            || self.system_note != next.system_note
            || self.fallbacks != next.fallbacks
            || self.route != next.route
    }

    /// What a change from `self` to `next` touches, so a live session applies
    /// each part in place and rebuilds nothing else.
    #[must_use]
    pub fn reload_plan(&self, next: &Self) -> ReloadPlan {
        let before_tool = |c: &Self| c.hooks.iter().filter(|h| h.on == "before-tool").cloned().collect::<Vec<_>>();
        let gate = self.builtin_tools != next.builtin_tools
            || self.guardrail != next.guardrail
            || self.tools != next.tools
            || before_tool(self) != before_tool(next);
        let mut restart = Vec::new();
        if self.ledger_path() != next.ledger_path() {
            restart.push("persistence.ledger: this session keeps writing the usage file it opened, so its totals stay whole");
        }
        ReloadPlan {
            mcp_servers: self.mcp_servers != next.mcp_servers || self.anvil_json != next.anvil_json,
            mcp_call_timeout: self.mcp.call_timeout_secs != next.mcp.call_timeout_secs,
            tools: gate || self.mcp_exposure() != next.mcp_exposure(),
            gate,
            route: self.provedor != next.provedor || self.endpoint != next.endpoint || self.route != next.route,
            sweep: self.persistence() != next.persistence(),
            vigies: self.vigies != next.vigies,
            restart,
        }
    }

    /// Whether moving the conversation from `from` to this config's primary
    /// route must drop the thinking blocks in it: they are signed by the
    /// provider that wrote them, and this route echoes history back.
    #[must_use]
    pub fn drops_foreign_thinking(&self, from: &Self) -> bool {
        let moved = self.provedor != from.provedor || self.endpoint != from.endpoint;
        let echoes = match self.echo_thinking {
            Some(b) => b,
            None => self.provider().map_or(true, |p| p.echo_rule() != EchoRule::Never),
        };
        moved && echoes
    }

    /// The primary as a route declaration — what [`Self::switch_to`] puts
    /// back in the fallback list so the switch can be undone.
    #[must_use]
    pub fn primary_route(&self) -> RouteDecl {
        RouteDecl {
            provedor: self.provedor.clone(),
            modelo: self.modelo.clone(),
            endpoint: self.endpoint.clone(),
            api_key_file: self.api_key_file.clone(),
            key_env: self.key_env.clone(),
            timeout_secs: None,
            system_note: self.system_note.clone(),
            tools: self.route.tools,
            label: self.route.label.clone(),
            facts: self.route.facts.clone(),
        }
    }

    /// The declared endpoints, resolved; one that cannot be is refused by
    /// name and the others proceed.
    #[must_use]
    pub fn endpoints(&self) -> (Vec<arnes_anthropic::reach::Endpoint>, Vec<String>) {
        let mut ok = Vec::new();
        let mut refused = Vec::new();
        for e in &self.endpoints {
            match self.key_file_allowed(e.key_file.as_ref()).and_then(|()| e.resolve()) {
                Ok(r) => ok.push(r),
                Err(why) => refused.push(format!("endpoint {}: {why}", e.name)),
            }
        }
        (ok, refused)
    }

    /// The primary, resolved the way a fallback is.
    ///
    /// # Errors
    /// As [`RouteDecl::resolve`], or a key file outside `access.key-files`.
    pub fn primary(&self) -> Result<Route, String> {
        self.key_file_allowed(self.api_key_file.as_ref())?;
        self.primary_route().resolve()
    }

    /// `/model`: fallback `i` becomes the provider and the old provider
    /// takes its place at the head of the fallbacks, so nothing is lost and
    /// switching back is one more pick.
    #[must_use]
    pub fn switch_to(&self, i: usize) -> Self {
        let mut next = self.promote(i);
        next.fallbacks.insert(0, self.primary_route());
        next
    }

    /// The request params for a fallback: the primary's knobs, with the
    /// route's own echo rule and, when it sets one, its own timeout.
    ///
    /// # Errors
    /// As [`Self::request_params`].
    pub fn route_params(&self, route: &Route) -> Result<arnes_anthropic::RequestParams, String> {
        let mut p = self.request_params()?;
        if self.echo_thinking.is_none() {
            p.echo = route.provider.echo_rule();
        }
        if route.timeout.is_some() {
            p.timeout = route.timeout;
        }
        Ok(p)
    }

    /// Load via shikumi — auto-detects format (lisp / yaml / nix / toml).
    /// Falls back to `ArnesConfig::default()` if no config file is found.
    /// Where shikumi finds the config (`$ARNES_CONFIG`, then XDG), if any —
    /// the file hot reload watches.
    #[must_use]
    pub fn discover_path() -> Option<PathBuf> {
        shikumi::ConfigDiscovery::new("arnes")
            .env_override("ARNES_CONFIG")
            .formats(&[
                shikumi::Format::Lisp,
                shikumi::Format::Yaml,
                shikumi::Format::Nix,
                shikumi::Format::Toml,
            ])
            .discover()
            .ok()
    }

    #[must_use]
    pub fn load() -> Self {
        let Some(path) = Self::discover_path() else {
            return Self::base_tier();
        };
        Self::load_file(&path).unwrap_or_else(|e| {
            // Loud, never silent: only a file that does not parse at all
            // lands here; a value that does not fit is refused alone.
            tracing::warn!("ignoring {} ({e}); using defaults", path.display());
            Self::base_tier()
        })
    }

    /// The tier a file is layered on: `$ARNES_TIER` (`bare`, `discovered`,
    /// `default`), else the prescribed default.
    #[must_use]
    pub fn base_tier() -> Self {
        use shikumi::TieredConfig;
        match shikumi::ConfigTier::from_env(TIER_ENV) {
            shikumi::ConfigTier::Custom(_) => Self::prescribed_default(),
            tier => Self::resolve_tier(tier),
        }
    }

    /// The one load path: the base tier, then `ARNES_*`, then the file, each
    /// key applied alone (see [`salvage`]). Refusals are warned (each one a
    /// notice in the TUI) and the rest of the file applies. Tests read
    /// through this, not `compile_from_sexp`: the two Lisp readers disagree
    /// on nested forms, and this is the one the product runs.
    ///
    /// # Errors
    /// The file cannot be read or parsed at all.
    pub fn load_file(path: &std::path::Path) -> Result<Self, String> {
        let loaded = Self::load_report(path)?;
        for r in &loaded.refused {
            tracing::warn!("config {r}");
        }
        Ok(loaded.config)
    }

    /// [`Self::load_file`], returning what was refused instead of warning it,
    /// and the raw file and env documents (for `arnes config show`).
    ///
    /// # Errors
    /// The file cannot be read or parsed at all.
    pub fn load_report(path: &std::path::Path) -> Result<Loaded, String> {
        let base = Self::base_tier();
        let env: serde_json::Value = shikumi::ProviderChain::new()
            .with_env(ENV_PREFIX)
            .extract()
            .unwrap_or(serde_json::Value::Null);
        // `ARNES_CONFIG` and `ARNES_TIER` name the file and tier; they are not keys.
        let env = match env {
            serde_json::Value::Object(mut m) => {
                m.remove("config");
                m.remove("tier");
                serde_json::Value::Object(m)
            }
            other => other,
        };
        let file: serde_json::Value = shikumi::ProviderChain::new()
            .with_file(path)
            .extract()
            .map_err(|e| e.to_string())?;
        let (with_env, env_refused) = salvage::salvage(&base, &env);
        let (config, file_refused) = salvage::salvage(&with_env, &file);
        let schema = Self::schema_value();
        let mut refused: Vec<String> = env_refused.0.into_iter().map(|r| format!("{ENV_PREFIX}{r}")).collect();
        refused.extend(file_refused.0);
        refused.extend(salvage::unknown_keys(&schema, &file));
        Ok(Loaded { config, refused, file, env })
    }

    /// The JSON schema of the whole config (`arnes config schema`).
    #[must_use]
    pub fn schema_value() -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(Self)).unwrap_or(serde_json::Value::Null)
    }

    /// Resolve `:thinking` / `:effort` / `:thinking-budget` into one mode.
    ///
    /// # Errors
    /// An unknown `thinking` or `effort` value, named with its legal set.
    pub fn thinking_mode(&self) -> Result<ThinkingMode, String> {
        let effort = self.effort.as_deref().map(Effort::parse).transpose()?;
        let budget = match self.thinking_budget {
            Some(b) if !self.provider()?.honours_thinking_budget() => {
                tracing::warn!(
                    "thinking-budget {b} ignored — {} does not honour it; use :effort",
                    self.provedor
                );
                None
            }
            b => b,
        };
        match self.thinking.as_deref() {
            Some("off") => Ok(ThinkingMode::Off),
            Some("on") => Ok(ThinkingMode::On { effort, budget }),
            // A configured budget asks for thinking even where it is dropped.
            None if effort.is_some() || self.thinking_budget.is_some() => {
                Ok(ThinkingMode::On { effort, budget })
            }
            None => Ok(ThinkingMode::ProviderDefault),
            Some(other) => Err(format!("unknown thinking {other:?}; expected one of [\"on\", \"off\"]")),
        }
    }

    /// The typed request knobs, validated. Out-of-range sampling is
    /// refused here, at startup, naming the field and its legal range —
    /// never discovered as a provider 400 mid-conversation.
    ///
    /// # Errors
    /// Names the first field outside its range, or an unknown provider.
    pub fn request_params(&self) -> Result<arnes_anthropic::RequestParams, String> {
        let range = |name: &str, v: Option<f32>, hi: f32| match v {
            Some(x) if !(0.0..=hi).contains(&x) => Err(format!("{name} = {x} is outside 0.0..={hi}")),
            _ => Ok(v),
        };
        Ok(arnes_anthropic::RequestParams {
            temperature: range("temperatura", self.temperatura, 2.0)?,
            top_p: range("top-p", self.top_p, 1.0)?,
            top_k: self.top_k,
            stop_sequences: self.stop_sequences.clone(),
            thinking: self.thinking_mode()?,
            echo: match self.echo_thinking {
                Some(true) => EchoRule::Always,
                Some(false) => EchoRule::Never,
                None => self.provider()?.echo_rule(),
            },
            timeout: self.timeout_secs.map(std::time::Duration::from_secs),
            connect_timeout: match self.connect_timeout_secs {
                None => None,
                Some(s @ 1..=120) => Some(std::time::Duration::from_secs(u64::from(s))),
                Some(s) => return Err(format!("connect-timeout-secs = {s} is outside 1..=120")),
            },
            max_retries: self.max_retries.unwrap_or(3),
            retry_base: std::time::Duration::from_millis(self.retry_base_ms.unwrap_or(500)),
            extra_headers: self
                .extra_headers
                .iter()
                .map(|h| (h.name.clone(), h.value.clone()))
                .collect(),
        })
    }

    /// # Errors
    /// An unknown exposure value, named with its legal set.
    pub fn mcp_exposure(&self) -> Result<arnes_mcp_client::Exposure, String> {
        let (max_inline, _) = sections::bounded("mcp.inline-max", self.mcp.inline_max, 1, 2000, 40);
        match self.mcp_tools_mode().as_deref() {
            None | Some("auto") => Ok(arnes_mcp_client::Exposure::Auto { max_inline: max_inline as usize }),
            Some(other) => arnes_mcp_client::Exposure::parse(other),
        }
    }

    /// Every bounded knob outside `ui`/`keys` read against its bounds, one
    /// refusal per value out of range (each default kept).
    #[must_use]
    pub fn knobs(&self) -> (Knobs, Vec<String>) {
        use sections::bounded;
        let mut refused = Vec::new();
        let mut take = |(v, r): (u32, Option<String>)| {
            refused.extend(r);
            v
        };
        let k = Knobs {
            mcp_connect: std::time::Duration::from_secs(u64::from(take(bounded("mcp.connect-timeout-secs", self.mcp.connect_timeout_secs, 1, 300, 15)))),
            mcp_call: std::time::Duration::from_secs(u64::from(take(bounded("mcp.call-timeout-secs", self.mcp.call_timeout_secs, 1, 3600, 120)))),
            catalog_max_age: std::time::Duration::from_secs(u64::from(take(bounded("catalog.max-age-hours", self.catalog.max_age_hours, 0, 720, 24))) * 3600),
            mention_max_files: take(bounded("mentions.max-files", self.mentions.max_files, 1, 1_000_000, 20_000)) as usize,
            mention_max_bytes: u64::from(take(bounded("mentions.max-kib", self.mentions.max_kib, 1, 16_384, 256))) * 1024,
            max_rounds: take(bounded("turn.max-rounds", self.turn.max_rounds, 1, 256, 16)),
            notify_after: std::time::Duration::from_secs(u64::from(take(bounded("activity.notify-after-secs", self.activity.notify_after_secs, 0, 3600, 10)))),
            frame_rate: take(bounded("ui.timing.frame-rate", self.ui.timing.frame_rate, 1, 240, 60)),
            idle_poll: std::time::Duration::from_millis(u64::from(take(bounded("ui.timing.idle-poll-ms", self.ui.timing.idle_poll_ms, 10, 1000, 50)))),
        };
        (k, refused)
    }

    /// The prompt history's knobs, each read against its legal set alone.
    #[must_use]
    pub fn history_knobs(&self) -> (HistoryKnobs, Vec<String>) {
        use sections::{bounded, one_of};
        let h = &self.history;
        let mut refused = Vec::new();
        let (record, r) = one_of("history.record", h.record.as_deref(), &["on", "off"], "on");
        refused.extend(r);
        let (scope, r) = one_of("history.scope", h.scope.as_deref(), &arnes_store::history::Scope::NAMES, "project-then-global");
        refused.extend(r);
        let (keep, r) = bounded("history.keep", h.keep, 0, 1_000_000, 10_000);
        refused.extend(r);
        let (search, r) = one_of("history.search", h.search.as_deref(), &["picker", "incremental"], "picker");
        refused.extend(r);
        let (ranking, r) = one_of("history.ranking", h.ranking.as_deref(), &["frecency", "recency"], "frecency");
        refused.extend(r);
        let k = HistoryKnobs {
            record: record == "on",
            scope: arnes_store::history::Scope::parse(scope).unwrap_or_default(),
            keep: keep as usize,
            search: if search == "incremental" { HistorySearchStyle::Incremental } else { HistorySearchStyle::Picker },
            recency: ranking == "recency",
            pin_recent: h.pin_recent.unwrap_or(true),
        };
        (k, refused)
    }

    /// The permission policy, validated.
    ///
    /// # Errors
    /// An unknown `:permission-mode`, named with its legal set.
    pub fn permission_policy(&self) -> Result<arnes_loop::permission::Policy, String> {
        use arnes_loop::permission::{PermissionMode, Policy};
        Ok(Policy {
            mode: self.permission_mode.as_deref().map_or(Ok(PermissionMode::Default), PermissionMode::parse)?,
            allow: self.allow_tools.clone(),
            deny: self.deny_tools.clone(),
        })
    }

    /// The blue commands whose names are free, and a refusal for each one
    /// that shadows a name in `reserved` (the built-ins) or an earlier
    /// declaration. A refused entry is skipped by itself; the rest load.
    #[must_use]
    pub fn blue_commands(&self, reserved: &[&str]) -> (Vec<CommandDecl>, Vec<String>) {
        let mut ok: Vec<CommandDecl> = Vec::new();
        let mut refused = Vec::new();
        for c in &self.commands {
            if c.name.is_empty() || c.name.contains(char::is_whitespace) {
                refused.push(format!("command {:?}: a name is one word", c.name));
            } else if reserved.contains(&c.name.as_str()) {
                refused.push(format!("command /{}: shadows the built-in", c.name));
            } else if ok.iter().any(|o| o.name == c.name) {
                refused.push(format!("command /{}: declared twice; the first is kept", c.name));
            } else {
                ok.push(c.clone());
            }
        }
        (ok, refused)
    }

    /// The declared vigies, each refused alone: a name that is not one word,
    /// or one declared twice (the first is kept). The interval's floor is
    /// vigy's own, checked where the vigy is built.
    #[must_use]
    pub fn blue_vigies(&self) -> (Vec<arnes_vigy::Decl>, Vec<String>) {
        let mut ok: Vec<arnes_vigy::Decl> = Vec::new();
        let mut refused = Vec::new();
        for v in &self.vigies {
            if v.name.is_empty() || v.name.contains(char::is_whitespace) {
                refused.push(format!("vigy {:?}: a name is one word", v.name));
            } else if ok.iter().any(|o| o.name == v.name) {
                refused.push(format!("vigy {}: declared twice; the first is kept", v.name));
            } else {
                ok.push(arnes_vigy::Decl {
                    name: v.name.clone(),
                    script: arnes_blue::Script::new(v.blue.clone(), v.r#fn.clone()),
                    every: std::time::Duration::from_millis(v.every_ms.unwrap_or(5000)),
                });
            }
        }
        (ok, refused)
    }

    /// The blue tools that resolve, and a refusal for each one that does not
    /// (bad schema, a name another tool already has). `reserved` are names
    /// the built-in tools own.
    #[must_use]
    pub fn blue_tools(&self, reserved: &[&str]) -> (arnes_blue::Tools, Vec<String>) {
        let mut specs: Vec<arnes_blue::ToolSpec> = Vec::new();
        let mut refused = Vec::new();
        for t in &self.tools {
            if reserved.contains(&t.name.as_str()) || specs.iter().any(|s| s.name == t.name) {
                refused.push(format!("tool {}: the name is already taken", t.name));
                continue;
            }
            match t.spec() {
                Ok(s) => specs.push(s),
                Err(e) => refused.push(e),
            }
        }
        (arnes_blue::Tools::new(specs), refused)
    }

    /// The blue hooks, and a refusal for each entry whose `:on` names no
    /// event (that entry alone is skipped).
    #[must_use]
    pub fn blue_hooks(&self) -> (arnes_blue::Hooks, Vec<String>) {
        let mut ok = Vec::new();
        let mut refused = Vec::new();
        for h in &self.hooks {
            match arnes_blue::HookEvent::parse(&h.on) {
                Ok(e) => ok.push((e, arnes_blue::Script::new(h.blue.clone(), h.r#fn.clone()))),
                Err(e) => refused.push(format!("hook {}: {e}", h.r#fn)),
            }
        }
        (arnes_blue::Hooks::new(ok), refused)
    }

    /// Where the usage ledger is written, or `None` when it is off.
    #[must_use]
    pub fn ledger_path(&self) -> Option<PathBuf> {
        match self.ledger_setting().as_deref() {
            Some("off") => None,
            Some(p) => Some(PathBuf::from(p)),
            None => arnes_loop::ledger::default_path(),
        }
    }

    /// What arnes keeps on disk, each key over its default. `sessions` other
    /// than `on`/`off` is refused alone (warned) and the default kept.
    #[must_use]
    pub fn persistence(&self) -> arnes_store::Policy {
        let mut p = arnes_store::Policy::default();
        match self.sessions_mode().as_deref() {
            None | Some("on") => {}
            Some("off") => p.sessions = false,
            Some(other) => tracing::warn!("sessions: {other:?} is not on or off; recording sessions"),
        }
        if let Some(n) = self.sessions_keep() {
            p.session_retention.keep_newest = (n > 0).then_some(n as usize);
        }
        if let Some(d) = self.sessions_max_age_days() {
            p.session_retention.max_age = (d > 0).then(|| std::time::Duration::from_secs(u64::from(d) * 86_400));
        }
        if let Some(mb) = self.log_max_mb() {
            p.log_max_bytes = u64::from(mb.max(1)) * 1024 * 1024;
        }
        p
    }

    /// Register the `(def…)` keywords. A collision means another domain
    /// already claims one of them; it is reported, not swallowed — the
    /// returned `Result`s used to be dropped, so a clash was invisible.
    pub fn register_domains() {
        let results = [
            tatara_lisp::domain::register::<Self>(),
            tatara_lisp::domain::register::<McpServerDecl>(),
            tatara_lisp::domain::register::<HeaderDecl>(),
            tatara_lisp::domain::register::<RouteDecl>(),
            tatara_lisp::domain::register::<CommandDecl>(),
            tatara_lisp::domain::register::<ToolDecl>(),
            tatara_lisp::domain::register::<HookDecl>(),
            tatara_lisp::domain::register::<VigyDecl>(),
        ];
        for e in results.into_iter().filter_map(Result::err) {
            tracing::warn!("tatara-lisp keyword collision: {e:?}");
        }
    }

    /// Every MCP server arnes would run (docs/profiles.md §1), with what was
    /// dropped or refused on the way: `:mcp-servers`, then `:anvil-json`,
    /// then each `mcp.scopes` file, then `./.mcp.json` when `mcp.project` is
    /// on, first name wins; a server declared twice under two names is kept
    /// once; `mcp.only` and `mcp.off` select and `mcp.lazy` marks dormant.
    #[must_use]
    pub fn mcp_registry_report(&self) -> (arnes_mcp_client::McpRegistry, Vec<String>) {
        let mut reg = self.to_declared_registry();
        let mut notes = Vec::new();
        let read = |path: &std::path::Path, notes: &mut Vec<String>| -> Option<arnes_mcp_client::McpRegistry> {
            let text = std::fs::read_to_string(path).map_err(|e| notes.push(format!("MCP file {}: {e}", path.display()))).ok()?;
            let v = serde_json::from_str::<serde_json::Value>(&text).map_err(|e| notes.push(format!("MCP file {}: {e}", path.display()))).ok()?;
            Some(arnes_mcp_client::from_anvil_json(&v))
        };
        if !self.mcp.scopes.is_empty() {
            let dir = self.mcp.scope_dir.as_deref().map(|d| arnes_blue::expand_home(std::path::Path::new(d))).or_else(|| {
                std::env::var_os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
                    .map(|c| c.join("arnes/mcp"))
            });
            for scope in &self.mcp.scopes {
                if let Some(r) = dir.as_ref().and_then(|d| read(&d.join(format!("{scope}.json")), &mut notes)) {
                    reg.merge(r);
                }
            }
        }
        if self.mcp.project == Some(true) {
            let project = std::path::Path::new(".mcp.json");
            if project.exists()
                && let Some(r) = read(project, &mut notes)
            {
                reg.merge(r);
            }
        }
        notes.extend(reg.dedup());
        notes.extend(reg.select(&self.mcp.only, &self.mcp.off, &self.mcp.lazy));
        (reg, notes)
    }

    /// [`Self::mcp_registry_report`] without the notes.
    #[must_use]
    pub fn to_mcp_registry(&self) -> arnes_mcp_client::McpRegistry {
        self.mcp_registry_report().0
    }

    /// The declared profiles' names and descriptions, for the `/profile`
    /// picker.
    #[must_use]
    pub fn profile_choices(&self) -> Vec<(String, String)> {
        self.profiles.iter().map(|p| (p.name.clone(), p.description.clone().unwrap_or_default())).collect()
    }

    /// The overlays `name` applies, parents first: each `:inherits` entry
    /// before the profile itself, a parent reached twice applied once. An
    /// unknown name or a cycle is refused by name and the rest of the chain
    /// applies.
    fn profile_chain(&self, name: &str, seen: &mut Vec<String>, out: &mut Vec<serde_json::Value>, refused: &mut Vec<String>) {
        if seen.iter().any(|s| s == name) {
            refused.push(format!("profile {name}: inherited in a cycle; applied once"));
            return;
        }
        let Some(p) = self.profiles.iter().find(|p| p.name == name) else {
            refused.push(format!("profile {name}: no such profile"));
            return;
        };
        seen.push(name.to_owned());
        for parent in &p.inherits {
            self.profile_chain(parent, seen, out, refused);
        }
        out.push(p.config.clone());
    }

    /// This config with profile `name` applied (docs/profiles.md §2): the
    /// chain's overlays merged, parents first, then applied key by key with
    /// [`salvage::salvage`], so a key that does not fit is refused alone. A
    /// profile cannot redefine `profiles` or `profile`; those keys are
    /// refused. `None` is this config unchanged.
    ///
    /// # Errors
    /// `name` is not a declared profile.
    pub fn with_profile(&self, name: Option<&str>) -> Result<(Self, Vec<String>), String> {
        let Some(name) = name else { return Ok((self.clone(), Vec::new())) };
        if !self.profiles.iter().any(|p| p.name == name) {
            let known: Vec<&str> = self.profiles.iter().map(|p| p.name.as_str()).collect();
            return Err(format!("no profile {name:?}; declared: {}", if known.is_empty() { "none".to_owned() } else { known.join(", ") }));
        }
        let (mut chain, mut refused) = (Vec::new(), Vec::new());
        self.profile_chain(name, &mut Vec::new(), &mut chain, &mut refused);
        // Deep: a profile's `:mcp (:only …)` leaves the file's other `mcp`
        // keys alone, where salvage alone would replace the whole section.
        let mut overlay = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        for mut layer in chain {
            if let serde_json::Value::Object(m) = &mut layer {
                for key in ["profiles", "profile"] {
                    if m.remove(key).is_some() {
                        refused.push(format!("profile {name}: {key} cannot be set by a profile"));
                    }
                }
            }
            merge_json(&mut overlay, layer);
        }
        if let serde_json::Value::Object(m) = &mut overlay {
            for key in ["profiles", "profile"] {
                m.remove(key);
            }
        }
        let (mut out, bad) = salvage::salvage(self, &overlay);
        refused.extend(bad.0.into_iter().map(|r| format!("profile {name}: {r}")));
        out.profiles.clone_from(&self.profiles);
        out.profile = Some(name.to_owned());
        Ok((out, refused))
    }

    /// Whether `access.key_files` lets a route read `file`.
    fn key_file_allowed(&self, file: Option<&PathBuf>) -> Result<(), String> {
        match file {
            Some(f) if !self.access.key_files.is_empty() => {
                let f = arnes_blue::expand_home(f);
                let ok = self.access.key_files.iter().any(|k| arnes_blue::expand_home(std::path::Path::new(k)) == f);
                if ok { Ok(()) } else { Err(format!("access: key file {} is not in access.key-files", f.display())) }
            }
            _ => Ok(()),
        }
    }

    /// Whether `access.read_roots` lets a hosted bidama or an `@path` read
    /// `path`: under one of the roots, or anywhere when none is set.
    #[must_use]
    pub fn read_allowed(&self, path: &std::path::Path) -> bool {
        arnes_blue::within_roots(&self.read_roots(), path)
    }

    /// `access.read_roots` as paths.
    #[must_use]
    pub fn read_roots(&self) -> Vec<PathBuf> {
        self.access.read_roots.iter().map(PathBuf::from).collect()
    }

    /// Whether the TUI starts before its MCP servers (`mcp.startup`).
    #[must_use]
    pub fn mcp_background(&self) -> (bool, Option<String>) {
        let (v, r) = sections::one_of("mcp.startup", self.mcp.startup.as_deref(), &["background", "block"], "background");
        (v == "background", r)
    }

    /// The probe schedule (`mcp.health`): `None` when probing is off, the
    /// probe timeout, and whether routine probes are shared through the
    /// fleet lease. Each bad value is refused alone.
    #[must_use]
    pub fn mcp_health(&self) -> (Option<arnes_mcp_client::health::Policy>, std::time::Duration, bool, Vec<String>) {
        use sections::{bounded, one_of};
        use std::time::Duration;
        let h = &self.mcp.health;
        let mut refused = Vec::new();
        let mut take = |(v, r): (u32, Option<String>)| {
            refused.extend(r);
            v
        };
        let ms = |v: u32| Duration::from_millis(u64::from(v));
        let secs = |v: u32| Duration::from_secs(u64::from(v));
        let policy = arnes_mcp_client::health::Policy {
            min: ms(take(bounded("mcp.health.probe-min-ms", h.probe_min_ms, 1000, 3_600_000, 15_000))),
            max: ms(take(bounded("mcp.health.probe-max-ms", h.probe_max_ms, 1000, 86_400_000, 300_000))),
            growth_pct: take(bounded("mcp.health.growth-pct", h.growth_pct, 100, 1000, 200)),
            fail: ms(take(bounded("mcp.health.fail-ms", h.fail_ms, 100, 600_000, 2000))),
            reconnect_max: ms(take(bounded("mcp.health.reconnect-max-ms", h.reconnect_max_ms, 1000, 3_600_000, 120_000))),
            jitter_pct: take(bounded("mcp.health.jitter-pct", h.jitter_pct, 0, 100, 10)),
            flaps: take(bounded("mcp.health.quarantine-flaps", h.quarantine_flaps, 0, 100, 4)),
            window: secs(take(bounded("mcp.health.quarantine-window-secs", h.quarantine_window_secs, 1, 86_400, 300))),
            quarantine: secs(take(bounded("mcp.health.quarantine-secs", h.quarantine_secs, 1, 86_400, 600))),
        };
        let timeout = ms(take(bounded("mcp.health.timeout-ms", h.timeout_ms, 100, 60_000, 2000)));
        let (enabled, r) = one_of("mcp.health.enabled", h.enabled.as_deref(), &["on", "off"], "on");
        refused.extend(r);
        let (lease, r) = one_of("mcp.health.lease", h.lease.as_deref(), &["on", "off"], "on");
        refused.extend(r);
        ((enabled == "on").then_some(policy), timeout, lease == "on", refused)
    }

    /// `:mcp-servers` and `:anvil-json` alone, as they were before discovery.
    fn to_declared_registry(&self) -> arnes_mcp_client::McpRegistry {
        let mut reg = arnes_mcp_client::McpRegistry::new();
        for decl in &self.mcp_servers {
            reg.register(arnes_mcp_client::ServerSpec {
                name: decl.nome.clone(),
                command: decl.command.clone(),
                args: decl.args.clone(),
                env: decl.env.clone(),
                cwd: decl.cwd.clone(),
                required: decl.required,
                lazy: false,
            });
        }
        // Merge with an anvil-generated json if configured.
        if let Some(anvil_path) = &self.anvil_json
            && let Ok(text) = std::fs::read_to_string(anvil_path)
                && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                    let from_anvil = arnes_mcp_client::from_anvil_json(&parsed);
                    for s in from_anvil.servers {
                        if reg.get(&s.name).is_none() {
                            reg.register(s);
                        }
                    }
                }
        reg
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tatara_lisp::domain::TataraDomain;

    #[test]
    fn default_is_the_free_local_route() {
        let c = ArnesConfig::default();
        assert_eq!(c.provider(), Ok(Provider::Local));
        assert_eq!(c.model(), "local");
        assert_eq!(c.resolved_endpoint().unwrap(), "http://127.0.0.1:8080");
        assert!(c.api_key().unwrap().is_none(), "the default needs no key");
        let d = deepseek();
        assert_eq!(d.model(), "deepseek-flash");
        assert_eq!(d.resolved_endpoint().unwrap(), "https://api.deepseek.com/anthropic");
    }

    fn deepseek() -> ArnesConfig {
        ArnesConfig { provedor: "deepseek".into(), ..ArnesConfig::default() }
    }

    #[test]
    fn every_provider_round_trips_and_is_complete() {
        for p in Provider::ALL {
            assert_eq!(Provider::parse(p.name()), Ok(p));
            if p == Provider::Messages {
                assert!(p.endpoint().is_empty() && p.default_model().is_empty() && p.models_url().is_none() && p.requires_key());
                continue;
            }
            // Only a keyless server on the operator's own network may be plain HTTP.
            assert!(p.endpoint().starts_with("https://") || !p.requires_key(), "{p:?}");
            assert!(!p.default_model().is_empty());
            assert!(p.key_env().ends_with("_API_KEY"));
            // Every keyed provider has a catalogue; only a local server lacks one.
            assert_eq!(p.models_url().is_some(), p.requires_key(), "{p:?}");
        }
        assert!(Provider::parse("openai").is_err());
    }

    #[test]
    fn a_local_server_needs_no_key_and_a_keyed_provider_does() {
        let local = ArnesConfig { provedor: "local".into(), key_env: Some("ARNES_TEST_NO_SUCH_KEY".into()), ..ArnesConfig::default() };
        assert!(local.api_key().unwrap().is_none());
        assert_eq!(local.model(), "local");
        assert_eq!(local.request_params().unwrap().echo, EchoRule::Never);
        let keyed = ArnesConfig { key_env: Some("ARNES_TEST_NO_SUCH_KEY".into()), ..deepseek() };
        assert!(keyed.api_key().unwrap_err().contains("ARNES_TEST_NO_SUCH_KEY"));
    }

    /// `src` through [`ArnesConfig::load_file`], the path the product runs.
    fn load_lisp(src: &str) -> ArnesConfig {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "arnes-cfg-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("arnes.lisp");
        std::fs::write(&path, src).unwrap();
        let c = ArnesConfig::load_file(&path);
        std::fs::remove_dir_all(&dir).unwrap();
        c.unwrap()
    }

    #[test]
    fn loads_the_form_the_hm_module_renders() {
        // Byte-shaped like module/default.nix's output: kv puts every key on
        // its own line, inside a fallback too.
        let c = load_lisp(
            r#";; ~/.config/arnes/arnes.lisp — generated by home-manager.
(defarnes
  :provedor "deepseek"
  :tema "dark"
  :max-tokens 8192
  :api-key-file "/run/secret"
  :fallbacks ((:provedor "local"
  :endpoint "http://192.0.2.10:8080"
  :timeout-secs 600)))
"#,
        );
        assert_eq!(c.api_key_file, Some(PathBuf::from("/run/secret")));
        assert_eq!(c.max_tokens, Some(8192));
        assert_eq!(c.fallbacks.len(), 1);
        assert_eq!(c.fallbacks[0].endpoint.as_deref(), Some("http://192.0.2.10:8080"));
        assert_eq!(c.fallbacks[0].timeout_secs, Some(600), "a nested multi-word key must lower");
    }

    #[test]
    fn two_local_routes_are_told_apart_and_one_can_go_without_tools() {
        let c = load_lisp(
            r#"
(defarnes
  :fallbacks ((:provedor "local" :label "gpubox" :endpoint "http://192.0.2.10:8080")
              (:provedor "local" :label "on-device" :endpoint "http://127.0.0.1:8434" :modelo "apple-on-device" :tools #f)
              (:provedor "local")))
"#,
        );
        let (routes, refused) = c.fallback_routes();
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!((routes[0].label.as_str(), routes[0].tools), ("gpubox", true));
        assert_eq!((routes[1].label.as_str(), routes[1].tools), ("on-device", false));
        assert_eq!(routes[1].model, "apple-on-device");
        assert_eq!(routes[2].label, "local", "unset label → the provider's name");
    }

    #[test]
    fn parses_fallbacks_and_refuses_only_the_bad_route() {
        let c = load_lisp(
            r#"
(defarnes
  :provedor "deepseek"
  :fallbacks ((:provedor "local" :endpoint "http://192.0.2.10:8080" :timeout-secs 600)
              (:provedor "openai")
              (:provedor "anthropic" :key-env "ARNES_TEST_NO_SUCH_KEY")))
"#,
        );
        assert_eq!(c.fallbacks.len(), 3);
        let (routes, refused) = c.fallback_routes();
        assert_eq!(routes.len(), 1, "the local route survives its broken siblings");
        assert_eq!(routes[0].provider, Provider::Local);
        assert_eq!(routes[0].endpoint, "http://192.0.2.10:8080");
        assert_eq!(routes[0].model, "local");
        assert!(routes[0].key.is_none());
        assert_eq!(routes[0].timeout, Some(std::time::Duration::from_secs(600)));
        assert_eq!(refused.len(), 2);
        assert!(refused[0].contains("fallback 2") && refused[0].contains("openai"), "{refused:?}");
        assert!(refused[1].contains("ARNES_TEST_NO_SUCH_KEY"), "{refused:?}");
        let p = c.route_params(&routes[0]).unwrap();
        assert_eq!(p.echo, EchoRule::Never, "a route keeps its own echo rule");
        assert_eq!(p.timeout, Some(std::time::Duration::from_secs(600)));
    }

    #[test]
    fn promote_starts_on_the_fallback_and_keeps_the_rest() {
        let c = ArnesConfig {
            fallbacks: vec![
                RouteDecl { provedor: "local".into(), modelo: String::new(), endpoint: Some("http://h:8080".into()), api_key_file: None, key_env: None, timeout_secs: None, system_note: Some("You are gpt-oss-20b on gpubox.".into()), tools: None, label: None, facts: RouteFacts::default() },
                RouteDecl { provedor: "anthropic".into(), modelo: String::new(), endpoint: None, api_key_file: None, key_env: None, timeout_secs: None, system_note: None, tools: None, label: None, facts: RouteFacts::default() },
            ],
            ..ArnesConfig::default()
        };
        let p = c.promote(0);
        assert_eq!(p.provider(), Ok(Provider::Local));
        assert_eq!(p.resolved_endpoint().unwrap(), "http://h:8080");
        assert_eq!(p.model(), "local");
        assert_eq!(p.fallbacks.len(), 1);
        assert_eq!(p.fallbacks[0].provedor, "anthropic");
        assert_eq!(p.system_note.as_deref(), Some("You are gpt-oss-20b on gpubox."), "a promoted route's note travels with it");
        assert_eq!(p.prompt_sistema, None, "and never joins the prompt, so the next switch drops it");
        let back = p.switch_to(0);
        assert_eq!(back.system_note, None, "switching on leaves the note behind");
        assert_eq!(back.fallbacks[0].system_note.as_deref(), Some("You are gpt-oss-20b on gpubox."));
    }

    #[test]
    fn switch_to_keeps_the_old_primary_as_the_first_fallback() {
        let c = ArnesConfig {
            provedor: "deepseek".into(),
            fallbacks: vec![
                RouteDecl { provedor: "local".into(), modelo: String::new(), endpoint: Some("http://h:8080".into()), api_key_file: None, key_env: None, timeout_secs: None, system_note: None, tools: None, label: Some("gpubox".into()), facts: RouteFacts::default() },
                RouteDecl { provedor: "anthropic".into(), modelo: String::new(), endpoint: None, api_key_file: None, key_env: None, timeout_secs: None, system_note: None, tools: None, label: None, facts: RouteFacts::default() },
            ],
            ..ArnesConfig::default()
        };
        let s = c.switch_to(1);
        assert_eq!(s.provider(), Ok(Provider::Anthropic));
        let order: Vec<&str> = s.fallbacks.iter().map(|f| f.provedor.as_str()).collect();
        assert_eq!(order, ["deepseek", "local"]);
    }

    #[test]
    fn api_key_file_wins_and_is_trimmed() {
        let dir = std::env::temp_dir().join(format!("arnes-key-{}", std::process::id()));
        std::fs::write(&dir, "sk-test\n").unwrap();
        let c = ArnesConfig { api_key_file: Some(dir.clone()), ..ArnesConfig::default() };
        assert_eq!(c.api_key().unwrap().unwrap().expose(), "sk-test");
        std::fs::remove_file(dir).unwrap();
    }

    #[test]
    fn parses_the_form_the_hm_module_renders() {
        let src = r#"
(defarnes
  :provedor "deepseek"
  :modelo "deepseek-reasoner"
  :api-key-file "/run/secret"
  :tema "dark"
  :max-tokens 8192)
"#;
        let forms = tatara_lisp::read(src).unwrap();
        let c = ArnesConfig::compile_from_sexp(&forms[0]).unwrap();
        assert_eq!(c.model(), "deepseek-reasoner");
        assert_eq!(c.api_key_file, Some(PathBuf::from("/run/secret")));
    }

    #[test]
    fn parses_defarnes_form() {
        let src = r#"
(defarnes
  :modelo "claude-sonnet-4-6"
  :endpoint "https://api.anthropic.com"
  :tema "nord"
  :max-tokens 4096)
"#;
        let forms = tatara_lisp::read(src).unwrap();
        let c = ArnesConfig::compile_from_sexp(&forms[0]).unwrap();
        assert_eq!(c.modelo, "claude-sonnet-4-6");
        assert_eq!(c.tema.as_deref(), Some("nord"));
        assert_eq!(c.max_tokens, Some(4096));
    }

    #[test]
    fn parses_defmcp_server() {
        let src = r#"
(defmcp-server
  :nome    "escriba-mcp"
  :command "/nix/store/xxx/bin/escriba-mcp"
  :required #t)
"#;
        let forms = tatara_lisp::read(src).unwrap();
        let s = McpServerDecl::compile_from_sexp(&forms[0]).unwrap();
        assert_eq!(s.nome, "escriba-mcp");
        assert!(s.required);
    }

    #[test]
    fn loads_blue_commands_and_tools_through_the_product_path() {
        let c = load_lisp(
            r#"(defarnes
  :commands ((:name "wc" :description "count words" :blue "~/.config/arnes/wc.b" :fn "run"))
  :tools ((:name "shout" :description "upper-case" :blue "shout.b" :fn "shout"
           :schema "{\"type\":\"object\",\"properties\":{\"text\":{\"type\":\"string\"}}}"
           :read-only #t)))
"#,
        );
        assert_eq!(c.commands.len(), 1);
        assert_eq!(c.commands[0].r#fn, "run");
        assert_eq!(c.commands[0].script().path, PathBuf::from("~/.config/arnes/wc.b"));
        let (tools, refused) = c.blue_tools(&[]);
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(tools.read_only("shout"), Some(true));
        assert_eq!(tools.tools()[0].input_schema.0["properties"]["text"]["type"], "string");
    }

    #[test]
    fn history_keys_load_and_a_bad_one_is_refused_alone() {
        let d = ArnesConfig::default().history_knobs();
        assert_eq!(d.1, Vec::<String>::new());
        assert_eq!(
            d.0,
            HistoryKnobs { record: true, scope: arnes_store::history::Scope::ProjectThenGlobal, keep: 10_000, search: HistorySearchStyle::Picker, recency: false, pin_recent: true }
        );
        let c = load_lisp(
            r#"(defarnes
  :history (:record "off" :scope "project" :keep 50 :search "incremental" :ranking "sideways" :pin-recent #f))
"#,
        );
        let (k, refused) = c.history_knobs();
        assert_eq!((k.record, k.scope, k.keep, k.search, k.pin_recent), (false, arnes_store::history::Scope::Project, 50, HistorySearchStyle::Incremental, false));
        assert!(!k.recency, "the bad ranking falls back to frecency");
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(refused[0].starts_with("history.ranking"), "{refused:?}");
    }

    #[test]
    fn vigies_and_persistence_keys_load_through_the_product_path() {
        let c = load_lisp(
            r#"(defarnes
  :sessions "off"
  :sessions-keep 0
  :log-max-mb 2
  :vigies ((:name "mcp-health" :blue "~/.config/arnes/mcp-health.b" :fn "tick" :every-ms 2000)
           (:name "two words" :blue "x.b" :fn "tick")
           (:name "mcp-health" :blue "y.b" :fn "tick")
           (:name "plain" :blue "p.b" :fn "tick")))
"#,
        );
        let (ok, refused) = c.blue_vigies();
        assert_eq!(ok.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["mcp-health", "plain"]);
        assert_eq!(ok[0].every, std::time::Duration::from_millis(2000));
        assert_eq!(ok[1].every, std::time::Duration::from_millis(5000));
        assert_eq!(ok[0].script.path, PathBuf::from("~/.config/arnes/mcp-health.b"));
        assert_eq!(refused.len(), 2, "{refused:?}");
        let p = c.persistence();
        assert!(!p.sessions);
        assert_eq!(p.session_retention.keep_newest, None);
        assert_eq!(p.log_max_bytes, 2 * 1024 * 1024);
        assert_eq!(ArnesConfig::default().persistence(), arnes_store::Policy::default());
        assert!(ArnesConfig::default().blue_vigies().0.is_empty());
    }

    #[test]
    fn hooks_load_and_an_unknown_event_is_refused_alone() {
        let c = load_lisp(
            r#"(defarnes
  :hooks ((:on "before-tool" :blue "h.b" :fn "guard")
          (:on "on-a-whim" :blue "h.b" :fn "odd")
          (:on "after-turn" :blue "h.b" :fn "log")))
"#,
        );
        let (hooks, refused) = c.blue_hooks();
        assert!(hooks.any(arnes_blue::HookEvent::BeforeTool) && hooks.any(arnes_blue::HookEvent::AfterTurn));
        assert!(!hooks.any(arnes_blue::HookEvent::BeforeSubmit));
        assert_eq!(refused.len(), 1);
        assert!(refused[0].contains("on-a-whim") && refused[0].contains("before-submit"), "{refused:?}");
    }

    #[test]
    fn a_bad_command_or_tool_is_refused_alone() {
        let decl = |name: &str| CommandDecl {
            name: name.into(),
            description: String::new(),
            blue: "x.b".into(),
            r#fn: "run".into(),
        };
        let c = ArnesConfig {
            commands: vec![decl("clear"), decl("wc"), decl("wc"), decl("two words"), decl("ok")],
            tools: vec![
                ToolDecl { name: "Read".into(), description: String::new(), blue: "t.b".into(), r#fn: "f".into(), schema: None, read_only: None },
                ToolDecl { name: "bad".into(), description: String::new(), blue: "t.b".into(), r#fn: "f".into(), schema: Some("[1]".into()), read_only: None },
                ToolDecl { name: "good".into(), description: String::new(), blue: "t.b".into(), r#fn: "f".into(), schema: None, read_only: None },
            ],
            ..ArnesConfig::default()
        };
        let (ok, refused) = c.blue_commands(&["clear", "exit"]);
        assert_eq!(ok.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["wc", "ok"]);
        assert_eq!(refused.len(), 3, "{refused:?}");
        let (tools, refused) = c.blue_tools(&["Read"]);
        assert!(tools.owns("good") && !tools.owns("bad") && !tools.owns("Read"));
        assert_eq!(tools.read_only("good"), Some(false), "unset read-only is asked");
        assert_eq!(refused.len(), 2, "{refused:?}");
    }

    #[test]
    fn register_populates_registry() {
        ArnesConfig::register_domains();
        let kws = tatara_lisp::domain::registered_keywords();
        assert!(kws.contains(&"defarnes"));
        assert!(kws.contains(&"defmcp-server"));
        assert!(kws.contains(&"defroute"));
    }

    #[test]
    fn config_to_mcp_registry() {
        let mut c = ArnesConfig::default();
        c.mcp_servers.push(McpServerDecl {
            nome: "escriba-mcp".into(),
            command: "/bin/escriba-mcp".into(),
            args: vec![],
            env: HashMap::new(),
            cwd: None,
            required: true,
        });
        let reg = c.to_mcp_registry();
        assert_eq!(reg.names(), vec!["escriba-mcp"]);
    }

    #[test]
    fn request_params_follow_provider_rules() {
        let d = deepseek().request_params().unwrap();
        assert_eq!(d.echo, EchoRule::WhenTools, "DeepSeek: echo exactly when tools are sent");
        assert_eq!(d.thinking, ThinkingMode::ProviderDefault);
        assert_eq!(d.max_retries, 3);
        assert_eq!(d.temperature, None);
        let a = ArnesConfig { provedor: "anthropic".into(), ..ArnesConfig::default() };
        assert_eq!(a.request_params().unwrap().echo, EchoRule::Always);
    }

    #[test]
    fn out_of_range_sampling_is_refused_by_name() {
        let c = ArnesConfig { temperatura: Some(2.5), ..ArnesConfig::default() };
        assert!(c.request_params().unwrap_err().contains("temperatura"));
        let c = ArnesConfig { top_p: Some(1.5), ..ArnesConfig::default() };
        assert!(c.request_params().unwrap_err().contains("top-p"));
    }

    #[test]
    fn key_env_override_is_honoured() {
        let c = ArnesConfig { key_env: Some("ARNES_TEST_KEY_ENV_UNSET".into()), ..deepseek() };
        assert!(c.api_key().unwrap_err().contains("ARNES_TEST_KEY_ENV_UNSET"));
    }

    #[test]
    fn parses_every_knob_from_lisp() {
        let src = r#"
(defarnes
  :provedor "deepseek"
  :modelo "deepseek-reasoner"
  :temperatura 0.3
  :top-p 0.95
  :top-k 40
  :stop-sequences ("END" "STOP")
  :thinking-budget 4096
  :echo-thinking #f
  :timeout-secs 120
  :max-retries 5
  :retry-base-ms 250
  :api-version "2023-06-01"
  :key-env "MY_KEY"
  :extra-headers ((:name "x-trace" :value "on")))
"#;
        let forms = tatara_lisp::read(src).unwrap();
        let c = ArnesConfig::compile_from_sexp(&forms[0]).unwrap();
        let p = c.request_params().unwrap();
        assert_eq!(p.top_k, Some(40));
        assert_eq!(p.stop_sequences, vec!["END", "STOP"]);
        // DeepSeek ignores budgets, so the parsed 4096 is dropped with a warning.
        assert_eq!(p.thinking, ThinkingMode::On { effort: None, budget: None });
        assert_eq!(p.max_retries, 5);
        assert_eq!(p.extra_headers, vec![("x-trace".into(), "on".into())]);
    }

    #[test]
    fn thinking_mode_resolution_table() {
        let with = |thinking: Option<&str>, effort: Option<&str>| ArnesConfig {
            thinking: thinking.map(Into::into),
            effort: effort.map(Into::into),
            ..ArnesConfig::default()
        };
        assert_eq!(with(None, None).thinking_mode(), Ok(ThinkingMode::ProviderDefault));
        assert_eq!(with(Some("off"), None).thinking_mode(), Ok(ThinkingMode::Off));
        assert_eq!(
            with(None, Some("low")).thinking_mode(),
            Ok(ThinkingMode::On { effort: Some(Effort::Low), budget: None })
        );
        assert!(with(Some("maybe"), None).thinking_mode().unwrap_err().contains("\"on\""));
        assert!(with(None, Some("medium")).thinking_mode().unwrap_err().contains("low"));
    }

    #[test]
    fn anthropic_keeps_its_budget() {
        let c = ArnesConfig {
            provedor: "anthropic".into(),
            thinking_budget: Some(1024),
            ..ArnesConfig::default()
        };
        assert_eq!(c.thinking_mode(), Ok(ThinkingMode::On { effort: None, budget: Some(1024) }));
    }

    #[test]
    fn modules_off_and_kernel_fallback_parse_and_default_empty() {
        let c = load_lisp("(defarnes :modules-off (\"help\" \"clear\") :kernel-fallback #t)");
        assert_eq!(c.modules_off, vec!["help".to_owned(), "clear".to_owned()]);
        assert_eq!(c.kernel_fallback, Some(true));
        let d = load_lisp("(defarnes)");
        assert!(d.modules_off.is_empty() && d.kernel_fallback.is_none());
        assert_eq!(load_lisp("(defarnes :module-budget-ms 40)").module_budget_ms, Some(40));
        assert_eq!(load_lisp("(defarnes :bidamas (\"mine\"))").bidamas, vec!["mine".to_owned()]);
    }

    /// `src` written as `name` and read through [`ArnesConfig::load_report`].
    fn load_as(name: &str, src: &str) -> Loaded {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("arnes-sec-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, src).unwrap();
        let l = ArnesConfig::load_report(&path);
        std::fs::remove_dir_all(&dir).unwrap();
        l.unwrap()
    }

    /// Every section set, for the round trips.
    fn everything() -> ArnesConfig {
        ArnesConfig {
            provedor: "deepseek".into(),
            modelo: "deepseek-flash".into(),
            system_note: Some("You are on ${HOME}'s \"box\".".into()),
            fallbacks: vec![RouteDecl { provedor: "local".into(), modelo: String::new(), endpoint: Some("http://h:8080".into()), api_key_file: None, key_env: None, timeout_secs: Some(600), system_note: None, tools: Some(false), label: Some("gpubox".into()), facts: RouteFacts::default() }],
            ui: UiSection {
                theme: Some("vellum".into()),
                motion: Some("off".into()),
                notifications: Some("off".into()),
                editor_mode: Some("vim".into()),
                regions: RegionsSection { notice_rows: Some(2), picker_rows: Some(20) },
                notices: NoticesSection { info_secs: Some(3), warn_secs: Some(9), error_secs: Some(30) },
                timing: TimingSection { frame_rate: Some(30), idle_poll_ms: Some(100), reveal_row_ms: Some(0) },
            },
            keys: KeysSection {
                prompt: KeyEdits { bind: vec![("ctrl+q".into(), "app:exit".into())], unbind: vec!["ctrl+d".into()] },
                nav: NavKeysSection {
                    menu: KeyEdits { bind: vec![("x".into(), "nav:close".into())], unbind: vec!["q".into()] },
                    filter: KeyEdits { bind: vec![], unbind: vec!["tab".into()] },
                },
            },
            activity: ActivitySection { shimmer_ms: Some(900), hold_ms: Some(600), notify_after_secs: Some(30) },
            mcp: McpSection { exposure: Some("deferred".into()), inline_max: Some(10), connect_timeout_secs: Some(5), call_timeout_secs: Some(60), ..McpSection::default() },
            persistence: PersistenceSection { sessions: Some("off".into()), sessions_keep: Some(10), sessions_max_age_days: Some(7), log_max_mb: Some(2), ledger: Some("off".into()) },
            free_route: FreeRouteSection { enabled: Some(false), endpoint: Some("http://gpubox:8080".into()), on_device: Some(false), on_device_endpoint: None, on_device_model: None },
            catalog: CatalogSection { max_age_hours: Some(1) },
            mentions: MentionsSection { max_files: Some(100), max_kib: Some(64) },
            turn: TurnSection { max_rounds: Some(8) },
            extras: HashMap::from([("escriba_socket".into(), "/tmp/e.sock".into())]),
            ..ArnesConfig::default()
        }
    }

    #[test]
    fn every_section_round_trips_through_lisp_and_yaml() {
        let c = everything();
        let v = serde_json::to_value(&c).unwrap();
        for (name, format) in [("arnes.lisp", render::Format::Lisp), ("arnes.yaml", render::Format::Yaml)] {
            let text = render::render(&v, format);
            let l = load_as(name, &text);
            assert!(l.refused.is_empty(), "{name}: {:?}\n{text}", l.refused);
            assert_eq!(l.config, c, "{name} round trip:\n{text}");
        }
    }

    #[test]
    fn every_section_round_trips_through_nix() {
        // shikumi evaluates a .nix file with `nix eval`; a machine without nix
        // cannot run this half, and says so rather than passing silently.
        if std::process::Command::new("nix").arg("--version").output().is_err() {
            eprintln!("nix is not on PATH; the nix round trip is not run here");
            return;
        }
        let c = everything();
        let text = render::render(&serde_json::to_value(&c).unwrap(), render::Format::Nix);
        let l = load_as("arnes.nix", &text);
        assert!(l.refused.is_empty(), "{:?}\n{text}", l.refused);
        assert_eq!(l.config, c, "nix round trip:\n{text}");
    }

    #[test]
    fn a_partial_file_keeps_every_other_default() {
        let l = load_as("arnes.lisp", r"(defarnes :ui (:regions (:picker-rows 20)))");
        assert!(l.refused.is_empty(), "{:?}", l.refused);
        let want = ArnesConfig { ui: UiSection { regions: RegionsSection { picker_rows: Some(20), notice_rows: None }, ..UiSection::default() }, ..ArnesConfig::default() };
        assert_eq!(l.config, want);
        assert_eq!(l.config.picker_rows_setting(), Some(20));
        let l = load_as("arnes.yaml", "keys:\n  nav:\n    menu:\n      unbind: [q]\n");
        assert_eq!(l.config.keys.nav.menu.unbind, ["q"]);
        assert_eq!(l.config.provedor, "local");
    }

    #[test]
    fn a_bad_entry_is_refused_alone_and_the_file_still_loads() {
        let l = load_as(
            "arnes.lisp",
            r#"(defarnes
  :modelo "kept"
  :max-tokens "lots"
  :ui (:motion "off" :regions (:notice-rows "many" :picker-rows 8))
  :keys (:nav (:menu (:bind (("x" "nav:close") ("y") ("z" "nav:top")))))
  :fallbacks ((:provedor "local" :label "gpubox") (:label "no provider") (:provedor "anthropic"))
  :tem "vellum")
"#,
        );
        let c = &l.config;
        assert_eq!(c.modelo, "kept");
        assert_eq!(c.max_tokens, ArnesConfig::default().max_tokens, "the bad value's default kept");
        assert_eq!(c.motion_mode().as_deref(), Some("off"), "its sibling applied");
        assert_eq!((c.ui.regions.notice_rows, c.ui.regions.picker_rows), (None, Some(8)));
        assert_eq!(c.keys.nav.menu.bind.len(), 2, "the malformed pair alone is skipped");
        assert_eq!(c.fallbacks.iter().map(|f| f.provedor.as_str()).collect::<Vec<_>>(), ["local", "anthropic"]);
        let r = &l.refused;
        assert_eq!(r.len(), 5, "{r:?}");
        for needle in ["max_tokens", "ui.regions.notice_rows", "keys.nav.menu.bind[1]", "fallbacks[1]", "tem: unknown key"] {
            assert!(r.iter().any(|x| x.contains(needle)), "{needle} not refused: {r:?}");
        }
        assert!(r.iter().any(|x| x.contains("did you mean tema")), "{r:?}");
    }

    #[test]
    fn a_nested_key_wins_over_its_flat_twin_and_the_flat_one_still_works() {
        let flat = ArnesConfig { motion: Some("off".into()), tema: Some("vellum".into()), notice_rows: Some(1), ..ArnesConfig::default() };
        assert_eq!((flat.motion_mode().as_deref(), flat.theme().as_deref(), flat.notice_rows_setting()), (Some("off"), Some("vellum"), Some(1)));
        let both = ArnesConfig { ui: UiSection { motion: Some("on".into()), ..UiSection::default() }, ..flat };
        assert_eq!(both.motion_mode().as_deref(), Some("on"));
    }

    #[test]
    fn knobs_are_bounded_and_refused_one_by_one() {
        let c = ArnesConfig {
            mcp: McpSection { connect_timeout_secs: Some(0), call_timeout_secs: Some(30), ..McpSection::default() },
            turn: TurnSection { max_rounds: Some(1000) },
            ..ArnesConfig::default()
        };
        let (k, refused) = c.knobs();
        assert_eq!(k.mcp_connect, std::time::Duration::from_secs(15));
        assert_eq!(k.mcp_call, std::time::Duration::from_secs(30));
        assert_eq!(k.max_rounds, 16);
        assert_eq!(refused.len(), 2, "{refused:?}");
        let (d, none) = ArnesConfig::default().knobs();
        assert!(none.is_empty());
        assert_eq!((d.frame_rate, d.mention_max_bytes, d.catalog_max_age.as_secs()), (60, 256 * 1024, 86_400));
    }

    #[test]
    fn inline_max_shapes_auto_exposure() {
        let c = ArnesConfig { mcp: McpSection { inline_max: Some(5), ..McpSection::default() }, ..ArnesConfig::default() };
        assert_eq!(c.mcp_exposure(), Ok(arnes_mcp_client::Exposure::Auto { max_inline: 5 }));
        let c = ArnesConfig { mcp_tools: Some("all".into()), ..ArnesConfig::default() };
        assert_eq!(c.mcp_exposure(), Ok(arnes_mcp_client::Exposure::All));
        assert_eq!(ArnesConfig::default().mcp_exposure(), Ok(arnes_mcp_client::Exposure::DEFAULT));
    }

    #[test]
    fn tiers_follow_shikumi() {
        use shikumi::TieredConfig;
        assert_eq!(ArnesConfig::prescribed_default(), ArnesConfig::default());
        let b = ArnesConfig::bare();
        assert_eq!((b.tema.as_deref(), b.max_tokens, b.provedor.as_str()), (None, None, "local"));
        assert_ne!(b, ArnesConfig::prescribed_default());
    }

    #[test]
    fn the_schema_names_every_section() {
        let s = ArnesConfig::schema_value();
        for k in ["ui", "keys", "activity", "mcp", "persistence", "free_route", "route", "routing", "catalog", "mentions", "history", "turn", "system_note"] {
            assert!(s["properties"].get(k).is_some(), "{k} missing from the schema");
        }
    }

    #[test]
    fn the_free_route_is_local_then_on_device_and_can_be_turned_off() {
        let c = ArnesConfig {
            provedor: "deepseek".into(),
            free_route: FreeRouteSection { endpoint: Some("http://gpubox:8080".into()), on_device: Some(true), ..FreeRouteSection::default() },
            fallbacks: vec![RouteDecl { provedor: "openai".into(), modelo: String::new(), endpoint: None, api_key_file: None, key_env: None, timeout_secs: None, system_note: None, tools: None, label: None, facts: RouteFacts::default() }],
            ..ArnesConfig::default()
        };
        let f = c.free_route().unwrap();
        assert_eq!((f.provider(), f.resolved_endpoint().unwrap().as_str()), (Ok(Provider::Local), "http://gpubox:8080"));
        assert_eq!(f.fallbacks.len(), 1, "the unresolvable route dropped, on-device added");
        assert_eq!((f.fallbacks[0].label.as_deref(), f.fallbacks[0].tools), (Some("on-device"), Some(false)));
        let off = ArnesConfig { free_route: FreeRouteSection { enabled: Some(false), ..FreeRouteSection::default() }, ..c };
        assert!(off.free_route().is_none());
    }

    #[test]
    fn a_reload_plan_names_only_what_changed() {
        let a = ArnesConfig::default();
        let same = a.reload_plan(&a.clone());
        assert!(!same.mcp_servers && !same.tools && !same.gate && !same.route && !same.sweep && same.restart.is_empty());
        let b = ArnesConfig {
            mcp_servers: vec![McpServerDecl { nome: "x".into(), command: "x".into(), args: vec![], env: HashMap::new(), cwd: None, required: false }],
            ui: UiSection { motion: Some("off".into()), ..UiSection::default() },
            ..a.clone()
        };
        let p = a.reload_plan(&b);
        assert!(p.mcp_servers && !p.gate && !p.route, "{p:?}");
        let c = ArnesConfig { builtin_tools: Some(false), provedor: "deepseek".into(), ledger: Some("off".into()), sessions_keep: Some(3), ..a.clone() };
        let p = a.reload_plan(&c);
        assert!(p.gate && p.tools && p.route && p.sweep, "{p:?}");
        assert_eq!(p.restart.len(), 1);
    }

    #[test]
    fn thinking_is_dropped_only_when_moving_to_a_route_that_echoes() {
        let local = ArnesConfig::default();
        let anthropic = ArnesConfig { provedor: "anthropic".into(), ..ArnesConfig::default() };
        let deepseek = deepseek();
        assert!(anthropic.drops_foreign_thinking(&local), "anthropic echoes: local's unsigned blocks would be refused");
        assert!(deepseek.drops_foreign_thinking(&anthropic));
        assert!(!local.drops_foreign_thinking(&anthropic), "local never echoes, nothing to drop");
        assert!(!anthropic.drops_foreign_thinking(&anthropic.clone()), "same route, keep them");
    }

    #[test]
    fn loads_the_sections_the_hm_module_renders() {
        // Byte-shaped like module/default.nix's renderSection output.
        let l = load_as(
            "arnes.lisp",
            r#";; ~/.config/arnes/arnes.lisp — generated by home-manager.
(defarnes
  :provedor "local"
  :tema "dark"
  :max-tokens 8192
  :system-note "You are local."
  :modules-off ("help")
  :free-route (:on-device #f)
  :keys (:nav (:menu (:unbind ("q"))) :prompt (:bind (("ctrl+q" "app:exit"))))
  :mcp (:connect-timeout-secs 5)
  :ui (:motion "off" :regions (:picker-rows 20))
  :tools ((:blue "s.b" :description "up" :fn "shout" :name "shout" :read-only #t))
  :vigies ((:blue "~/h.b" :every-ms 2000 :fn "tick" :name "mcp-health")))
"#,
        );
        assert!(l.refused.is_empty(), "{:?}", l.refused);
        let c = l.config;
        assert_eq!(c.modules_off, ["help"]);
        assert_eq!((c.tools[0].name.as_str(), c.tools[0].read_only), ("shout", Some(true)));
        assert_eq!(c.vigies[0].every_ms, Some(2000));
        assert_eq!(c.system_note.as_deref(), Some("You are local."));
        assert_eq!(c.free_route.on_device, Some(false));
        assert_eq!(c.keys.nav.menu.unbind, ["q"]);
        assert_eq!(c.keys.prompt.bind, [("ctrl+q".to_owned(), "app:exit".to_owned())]);
        assert_eq!(c.mcp.connect_timeout_secs, Some(5));
        assert_eq!((c.motion_mode().as_deref(), c.picker_rows_setting()), (Some("off"), Some(20)));
    }

    #[test]
    fn every_section_leaf_has_a_typed_option() {
        // Every key a section declares is a typed home-manager option, so
        // nothing needs extraConfig. Read from the schema, so a new field
        // without an option is a red test.
        fn object<'a>(s: &'a serde_json::Value, defs: &'a serde_json::Value) -> Option<&'a serde_json::Map<String, serde_json::Value>> {
            let r = s.get("$ref").or_else(|| s.get("allOf").and_then(|a| a.get(0)).and_then(|a| a.get("$ref")))?;
            defs.get(r.as_str()?.rsplit('/').next()?)?.get("properties")?.as_object()
        }
        fn walk(props: &serde_json::Map<String, serde_json::Value>, defs: &serde_json::Value, path: &str, out: &mut Vec<(String, String, bool)>) {
            for (k, v) in props {
                match object(v, defs) {
                    Some(inner) => {
                        out.push((format!("{path}.{k}"), k.clone(), false));
                        walk(inner, defs, &format!("{path}.{k}"), out);
                    }
                    None => out.push((format!("{path}.{k}"), k.clone(), true)),
                }
            }
        }
        let module = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../module/default.nix")).unwrap();
        let schema = ArnesConfig::schema_value();
        let defs = &schema["definitions"];
        let camel = |snake: &str| {
            let mut out = String::new();
            let mut up = false;
            for ch in snake.chars() {
                if ch == '_' {
                    up = true;
                } else if up {
                    out.extend(ch.to_uppercase());
                    up = false;
                } else {
                    out.push(ch);
                }
            }
            out
        };
        let mut keys = Vec::new();
        for section in ["ui", "keys", "activity", "mcp", "persistence", "free_route", "route", "routing", "catalog", "mentions", "history", "turn", "skills", "context", "access"] {
            let props = object(&schema["properties"][section], defs).unwrap_or_else(|| panic!("{section} is a section"));
            keys.push((section.to_owned(), section.to_owned(), false));
            walk(props, defs, section, &mut keys);
        }
        for (path, key, is_leaf) in keys {
            let name = camel(&key);
            let found = if is_leaf {
                module.contains(&format!("{name} = leaf"))
            } else {
                module.contains(&format!("{name} = {{"))
                    || ["keyEdits", "routeFacts"].iter().any(|spec| module.contains(&format!("{name} = {spec};")))
            };
            assert!(found, "{path} has no typed option `{name}` in module/default.nix");
        }
    }
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    fn server(nome: &str, command: &str) -> McpServerDecl {
        McpServerDecl { nome: nome.into(), command: command.into(), args: vec![], env: HashMap::new(), cwd: None, required: false }
    }

    #[test]
    fn scopes_merge_after_the_declared_servers_deduplicated_and_selected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pleme.json"),
            r#"{"mcpServers":{"codesearch":{"command":"cs"},"dup":{"command":"gh"},"github":{"command":"other"}}}"#,
        )
        .unwrap();
        let cfg = ArnesConfig {
            mcp_servers: vec![server("github", "gh"), server("splunk", "sp")],
            mcp: McpSection {
                scopes: vec!["pleme".into(), "missing".into()],
                scope_dir: Some(dir.path().display().to_string()),
                off: vec!["splunk".into()],
                lazy: vec!["code*".into()],
                ..McpSection::default()
            },
            ..ArnesConfig::default()
        };
        let (reg, notes) = cfg.mcp_registry_report();
        assert_eq!(reg.names(), ["github", "codesearch"]);
        assert_eq!(reg.get("github").unwrap().command, "gh", "the declared one wins its name");
        assert!(reg.get("codesearch").unwrap().lazy);
        assert!(notes.iter().any(|n| n.contains("dup is github")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("missing.json")), "a missing scope file is named: {notes:?}");
        let plain = ArnesConfig::default().mcp_registry_report();
        assert!(plain.0.servers.is_empty() && plain.1.is_empty());
    }

    #[test]
    fn health_knobs_are_bounded_one_by_one_and_off_is_none() {
        let (p, timeout, lease, refused) = ArnesConfig::default().mcp_health();
        assert_eq!(p, Some(arnes_mcp_client::health::Policy::default()));
        assert_eq!((timeout, lease, refused.len()), (std::time::Duration::from_secs(2), true, 0));
        let mut cfg = ArnesConfig::default();
        cfg.mcp.health.probe_max_ms = Some(60_000);
        cfg.mcp.health.fail_ms = Some(1);
        cfg.mcp.health.lease = Some("off".into());
        let (p, _, lease, refused) = cfg.mcp_health();
        let p = p.unwrap();
        assert_eq!(p.max, std::time::Duration::from_secs(60));
        assert_eq!(p.fail, std::time::Duration::from_secs(2), "the bad value keeps its default");
        assert!(!lease);
        assert_eq!(refused.len(), 1);
        cfg.mcp.health.enabled = Some("off".into());
        assert!(cfg.mcp_health().0.is_none());
        cfg.mcp.startup = Some("block".into());
        assert_eq!(cfg.mcp_background(), (false, None));
        assert!(ArnesConfig::default().mcp_background().0);
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    fn load(src: &str) -> Loaded {
        ArnesConfig::register_domains();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("arnes.lisp");
        std::fs::write(&path, src).unwrap();
        ArnesConfig::load_report(&path).unwrap()
    }

    const FILE: &str = r#"(defarnes
      :permission-mode "default"
      :mcp (:exposure "auto")
      :profiles ((:name "base" :description "pleme-io"
                  :config (:mcp (:scopes ("pleme")) :skills (:only ("arnes"))))
                 (:name "ops" :inherits ("base") :description "read-mostly"
                  :config (:permission-mode "plan" :deny-tools ("Bash")
                           :mcp (:only ("codesearch")) :ui (:theme "vellum")
                           :turn (:max-rounds 24) :max-tokens "lots"
                           :profiles ()))
                 (:name "loop" :inherits ("loop" "ghost") :config (:tema "nord"))))"#;

    #[test]
    fn a_profile_overlays_its_parents_then_itself_and_refuses_bad_keys_alone() {
        let loaded = load(FILE);
        assert!(loaded.refused.is_empty(), "{:?}", loaded.refused);
        let cfg = loaded.config;
        assert_eq!(cfg.profile_choices().len(), 3);
        let (same, none) = cfg.with_profile(None).unwrap();
        assert_eq!((same, none.len()), (cfg.clone(), 0));
        let (a, refused) = cfg.with_profile(Some("ops")).unwrap();
        assert_eq!(a.permission_mode.as_deref(), Some("plan"));
        assert_eq!(a.deny_tools, ["Bash"]);
        assert_eq!(a.mcp.scopes, ["pleme"], "inherited from base");
        assert_eq!(a.mcp.only, ["codesearch"]);
        assert_eq!(a.mcp.exposure.as_deref(), Some("auto"), "the file's keys the chain does not touch are kept");
        assert_eq!(a.skills.only, ["arnes"]);
        assert_eq!(a.ui.theme.as_deref(), Some("vellum"));
        assert_eq!(a.profile.as_deref(), Some("ops"));
        assert_eq!(a.profiles, cfg.profiles, "a profile cannot redefine the profiles");
        assert!(refused.iter().any(|r| r.contains("profiles cannot be set")), "{refused:?}");
        assert_eq!(a.turn.max_rounds, Some(24));
        assert!(refused.iter().any(|r| r.starts_with("profile ops: max_tokens")), "a bad key is refused alone: {refused:?}");
        assert_eq!(a.max_tokens, cfg.max_tokens, "and keeps the file's value");
        let (_, refused) = cfg.with_profile(Some("loop")).unwrap();
        assert!(refused.iter().any(|r| r.contains("cycle")) && refused.iter().any(|r| r.contains("ghost")), "{refused:?}");
        assert!(cfg.with_profile(Some("nope")).unwrap_err().contains("ops"));
    }

    /// What `module/default.nix` renders for a profile set (evaluated with
    /// `lib.evalModules`, 2026-09-28): it loads with nothing refused.
    const RENDERED: &str = r#";; ~/.config/arnes/arnes.lisp — generated by home-manager.
(defarnes
  :provedor "local"
  :tema "dark"
  :max-tokens 8192
  :profile "base"
  :profiles ((:name "ops"
  :description "read-mostly"
  :inherits ("base") :config (:context (:prompt "Terragrunt, never pangea.") :deny-tools ("Bash") :mcp (:only ("codesearch")) :permission-mode "plan" :provedor "deepseek"))
             (:name "base"
  :description "pleme-io" :config (:mcp (:scopes ("pleme")))))
  :access (:read-roots ("~/code"))
  :mcp (:health (:probe-max-ms 60000))
  :skills (:only ("arnes")))
"#;

    #[test]
    fn loads_the_profiles_the_hm_module_renders() {
        let loaded = load(RENDERED);
        assert!(loaded.refused.is_empty(), "{:?}", loaded.refused);
        let cfg = loaded.config;
        assert_eq!(cfg.profile.as_deref(), Some("base"));
        assert_eq!(cfg.mcp.health.probe_max_ms, Some(60_000));
        let (a, refused) = cfg.with_profile(Some("ops")).unwrap();
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!((a.provedor.as_str(), a.permission_mode.as_deref()), ("deepseek", Some("plan")));
        assert_eq!((a.mcp.scopes.clone(), a.mcp.only.clone()), (vec!["pleme".to_owned()], vec!["codesearch".to_owned()]));
        assert_eq!(a.context.prompt.as_deref(), Some("Terragrunt, never pangea."));
        assert_eq!(a.access.read_roots, ["~/code"], "the file's access is kept under the profile");
    }

    #[test]
    fn the_example_in_docs_profiles_loads_and_applies_with_nothing_refused() {
        let doc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/profiles.md")).unwrap();
        let example = doc.split("## Example profile").nth(1).and_then(|s| s.split("```lisp\n").nth(1)).and_then(|s| s.split("```").next()).unwrap();
        let loaded = load(example);
        assert!(loaded.refused.is_empty(), "{:?}", loaded.refused);
        let (a, refused) = loaded.config.with_profile(Some("ops")).unwrap();
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(a.mcp.lazy, ["splunk-*"], "inherited from pleme");
        assert_eq!(a.mcp.scopes, ["ops"], "a list is replaced, not appended");
        assert_eq!(a.access.key_files, ["~/.config/deepseek/api-key"]);
    }

    #[test]
    fn a_switched_route_keeps_its_tools_label_and_facts_and_switching_back_restores_the_primary() {
        let cfg = ArnesConfig {
            fallbacks: vec![RouteDecl {
                provedor: "local".into(),
                endpoint: Some("http://127.0.0.1:8434".into()),
                tools: Some(false),
                label: Some("on-device".into()),
                facts: RouteFacts { tier: Some(0), context_window: Some(4096), ..RouteFacts::default() },
                ..RouteDecl::default()
            }],
            ..ArnesConfig::default()
        };
        assert!(cfg.primary().unwrap().tools);
        let there = cfg.switch_to(0);
        let p = there.primary().unwrap();
        assert_eq!((p.label.as_str(), p.tools, p.facts.tier, p.facts.context_window), ("on-device", false, Some(0), Some(4096)));
        let back = there.switch_to(0);
        let q = back.primary().unwrap();
        assert_eq!((q.label.as_str(), q.tools, q.facts.tier), ("local", true, None));
    }

    #[test]
    fn a_messages_route_names_its_endpoint_and_model_and_defaults_to_bearer() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("k");
        std::fs::write(&key, "sk-or").unwrap();
        let decl = |endpoint: Option<&str>, modelo: &str, auth: Option<&str>| RouteDecl {
            provedor: "messages".into(),
            modelo: modelo.into(),
            endpoint: endpoint.map(Into::into),
            api_key_file: Some(key.clone()),
            facts: RouteFacts { auth: auth.map(Into::into), ..RouteFacts::default() },
            ..RouteDecl::default()
        };
        let ok = decl(Some("https://openrouter.ai/api"), "example/judge-router", None).resolve().unwrap();
        assert_eq!((ok.auth, ok.model.as_str(), ok.endpoint.as_str()), (CatalogAuth::Bearer, "example/judge-router", "https://openrouter.ai/api"));
        assert!(decl(None, "m", None).resolve().unwrap_err().contains(":endpoint"));
        assert!(decl(Some("https://x"), "", None).resolve().unwrap_err().contains(":modelo"));
        assert_eq!(decl(Some("https://x"), "m", Some("x-api-key")).resolve().unwrap().auth, CatalogAuth::XApiKey);
        assert!(decl(Some("https://x"), "m", Some("basic")).resolve().unwrap_err().contains("unknown auth"));
        let cfg = ArnesConfig { fallbacks: vec![decl(None, "m", None), decl(Some("https://x"), "m", None)], ..ArnesConfig::default() };
        let (routes, refused) = cfg.fallback_routes();
        assert_eq!((routes.len(), refused.len()), (1, 1), "the bad route is refused alone");
        assert_eq!(RouteDecl { provedor: "anthropic".into(), api_key_file: Some(key), ..RouteDecl::default() }.resolve().unwrap().auth, CatalogAuth::XApiKey);
    }

    #[test]
    fn access_bounds_key_files_and_read_roots() {
        let dir = tempfile::tempdir().unwrap();
        let ok = dir.path().join("ok-key");
        let other = dir.path().join("other-key");
        std::fs::write(&ok, "sk").unwrap();
        std::fs::write(&other, "sk").unwrap();
        let mut cfg = ArnesConfig { provedor: "deepseek".into(), api_key_file: Some(other.clone()), ..ArnesConfig::default() };
        assert!(cfg.api_key().is_ok(), "no access.key-files: any");
        cfg.access.key_files = vec![ok.display().to_string()];
        assert!(cfg.api_key().unwrap_err().contains("access.key-files"));
        cfg.api_key_file = Some(ok.clone());
        assert!(cfg.api_key().is_ok());
        cfg.fallbacks = vec![ArnesConfig { provedor: "deepseek".into(), api_key_file: Some(other), ..ArnesConfig::default() }.primary_route()];
        let (routes, refused) = cfg.fallback_routes();
        assert!(routes.is_empty() && refused[0].contains("access"), "{refused:?}");
        assert!(cfg.read_allowed(&ok), "no read roots: anywhere");
        cfg.access.read_roots = vec![dir.path().display().to_string()];
        assert!(cfg.read_allowed(&ok));
        assert!(!cfg.read_allowed(std::path::Path::new("/etc/hosts")));
    }
}
