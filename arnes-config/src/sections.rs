//! The nested sections of [`crate::ArnesConfig`] (docs/configuration.md).
//!
//! Every section is declared through [`section!`]: one derive set, every
//! field defaulted and omitted from output when unset, so a partial file
//! loads and `arnes config show` prints only what was chosen. A field here
//! is an open value (`Option<u32>`, `Option<String>`); its bounds are checked
//! where it is read, and a bad value is refused alone with its default kept.
//!
//! Where a section field has an older flat twin (`:motion` beside
//! `:ui (:motion …)`), the nested one wins and the flat one keeps working:
//! behaviour is configured off, never deleted.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

/// Whether a value equals its default: the `skip_serializing_if` of every
/// section field.
pub(crate) fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// A config section: the derive set, `snake_case` keys (shikumi lowers
/// `:notice-rows` to `notice_rows`), and every field defaulted and skipped
/// when unset.
macro_rules! section {
    ($(#[$m:meta])* $name:ident = $kw:literal { $($(#[$fm:meta])* $f:ident : $t:ty),* $(,)? }) => {
        $(#[$m])*
        #[derive(
            DeriveTataraDomain, Serialize, Deserialize, schemars::JsonSchema, Debug, Clone, PartialEq, Default,
        )]
        #[serde(rename_all = "snake_case")]
        #[tatara(keyword = $kw)]
        pub struct $name {
            $(
                $(#[$fm])*
                #[serde(default, skip_serializing_if = "crate::sections::is_default")]
                pub $f: $t,
            )*
        }
    };
}

section! {
    /// How arnes looks and moves.
    UiSection = "defarnes-ui" {
        /// Fleet theme: `pleme_dark` (also `dark`), `vellum`, `polar_veil`,
        /// `bare`, or a preset name (`nord`). Flat twin `:tema`.
        theme: Option<String>,
        /// `on` (default) or `off`: every surface drawn at its end state.
        motion: Option<String>,
        /// `auto` (default) or `off` (bell only).
        notifications: Option<String>,
        /// `normal` (default) or `vim`.
        editor_mode: Option<String>,
        regions: RegionsSection,
        notices: NoticesSection,
        timing: TimingSection,
    }
}

section! {
    /// Rows the reserved regions may claim (docs/design.md, the screen law).
    RegionsSection = "defarnes-regions" {
        /// Most rows the notice lane opens to. 0..=10, default 3.
        notice_rows: Option<u32>,
        /// Rows a picker claims above the prompt. 3..=40, default 12.
        picker_rows: Option<u32>,
    }
}

section! {
    /// How long a notice stays in the lane, per severity. 1..=600 seconds.
    NoticesSection = "defarnes-notices" {
        /// Default 4.
        info_secs: Option<u32>,
        /// Default 8.
        warn_secs: Option<u32>,
        /// Default 12.
        error_secs: Option<u32>,
    }
}

section! {
    /// Frame pacing and the reveal of a finished reply.
    TimingSection = "defarnes-timing" {
        /// Most frames a second while something moves. 1..=240, default 60.
        frame_rate: Option<u32>,
        /// Input poll interval when nothing moves, ms. 10..=1000, default 50.
        idle_poll_ms: Option<u32>,
        /// A finished reply unrolls this many ms per row (clamped by the
        /// fleet's fast/slow tokens). 0..=200, default 12.
        reveal_row_ms: Option<u32>,
    }
}

section! {
    /// The activity line and the 150 ms acknowledgements around it.
    ActivitySection = "defarnes-activity" {
        /// One sweep of the thinking shimmer, ms. 200..=10000, default 1800.
        shimmer_ms: Option<u32>,
        /// How long a tool tick or a scroll position stays lit, ms.
        /// 100..=10000, default 1200 (a note stays twice that).
        hold_ms: Option<u32>,
        /// Turns shorter than this finish without a desktop notification,
        /// seconds. 0..=3600, default 10.
        notify_after_secs: Option<u32>,
    }
}

section! {
    /// One set of edits over a keymap: unbind first, then bind.
    ///
    /// `(:bind (("ctrl+q" "app:exit") ("x" "nav:close")) :unbind ("q"))`.
    /// Each entry is refused alone (unknown action, unparsable chord) and
    /// the rest apply.
    KeyEdits = "defarnes-keys-edit" {
        /// `(chord action)` pairs. A chord is awase's spelling
        /// (`ctrl+x ctrl+e`); an action is an id (`chat:submit`, `nav:down`).
        bind: Vec<(String, String)>,
        /// Chords to remove from the defaults, before the binds apply.
        unbind: Vec<String>,
    }
}

section! {
    /// Keybindings: the prompt's keymap and the navigation keymaps.
    KeysSection = "defarnes-keys" {
        /// The prompt (Claude Code's action ids: `chat:*`, `app:*`,
        /// `history:*`, `edit:*`).
        prompt: KeyEdits,
        nav: NavKeysSection,
    }
}

section! {
    /// egaku's navigation keymaps (`nav:*` action ids).
    NavKeysSection = "defarnes-nav-keys" {
        /// Every list and pager: help, panels, `/mcp`, the approval
        /// question, the scrollback.
        menu: KeyEdits,
        /// Pickers, where typed letters filter (`@`, `/`, `/model`).
        filter: KeyEdits,
    }
}

section! {
    /// MCP servers: how they start and how their tools reach the model.
    McpSection = "defarnes-mcp" {
        /// `auto` (default), `all` or `deferred`. Flat twin `:mcp-tools`.
        exposure: Option<String>,
        /// Under `auto`, tools sent inline up to this many. 1..=2000,
        /// default 40.
        inline_max: Option<u32>,
        /// A server's start and tool listing, seconds. 1..=300, default 15.
        connect_timeout_secs: Option<u32>,
        /// One tool call, seconds. 1..=3600, default 120.
        call_timeout_secs: Option<u32>,
        /// `background` (default: the TUI draws at once, servers attach as
        /// they come up) or `block` (wait for every server first).
        startup: Option<String>,
        /// Globs of servers not started until their first call.
        lazy: Vec<String>,
        /// Globs of the servers to use; empty = every discovered one.
        only: Vec<String>,
        /// Globs of servers never started.
        off: Vec<String>,
        /// anvil scopes whose rendered files (`scope-dir/<scope>.json`) are
        /// read after `:anvil-json`.
        scopes: Vec<String>,
        /// Where the scope files live. Default
        /// `$XDG_CONFIG_HOME/arnes/mcp`.
        scope_dir: Option<String>,
        /// Read `./.mcp.json` in the working directory (Claude Code's
        /// project file). Default off: a cloned repository does not start
        /// processes unasked.
        project: Option<bool>,
        health: McpHealthSection,
    }
}

section! {
    /// When each MCP server is probed (docs/profiles.md §1): rarely while
    /// healthy, soon and then with backoff when failing, not at all while
    /// quarantined for flapping.
    McpHealthSection = "defarnes-mcp-health" {
        /// `on` (default) or `off`: no probing, no reconnecting.
        enabled: Option<String>,
        /// First interval once up, and the floor, ms. 1000..=3600000, default 15000.
        probe_min_ms: Option<u32>,
        /// Ceiling for a healthy server, ms. 1000..=86400000, default 300000.
        probe_max_ms: Option<u32>,
        /// Healthy interval growth per ok probe, percent. 100..=1000, default 200.
        growth_pct: Option<u32>,
        /// First retry after a failure, ms. 100..=600000, default 2000.
        fail_ms: Option<u32>,
        /// Ceiling of the reconnect backoff, ms. 1000..=3600000, default 120000.
        reconnect_max_ms: Option<u32>,
        /// Jitter added to each interval, percent. 0..=100, default 10.
        jitter_pct: Option<u32>,
        /// Up/down flips inside the window that quarantine a server; 0 = never.
        /// 0..=100, default 4.
        quarantine_flaps: Option<u32>,
        /// 1..=86400, default 300.
        quarantine_window_secs: Option<u32>,
        /// How long a quarantine lasts. 1..=86400, default 600.
        quarantine_secs: Option<u32>,
        /// One probe's answer, ms. 100..=60000, default 2000.
        timeout_ms: Option<u32>,
        /// `on` (default) or `off`: share routine probes of a healthy server
        /// with other arnes processes through `vigy.db`.
        lease: Option<String>,
    }
}

section! {
    /// What arnes keeps on disk (docs/persistence.md). Each key also has a
    /// flat twin (`:sessions`, `:sessions-keep`, …).
    PersistenceSection = "defarnes-persistence" {
        /// `on` (default) or `off`.
        sessions: Option<String>,
        /// Saved sessions kept, newest first; 0 = unbounded. Default 200.
        sessions_keep: Option<u32>,
        /// Saved sessions older than this are removed; 0 = never. Default 90.
        sessions_max_age_days: Option<u32>,
        /// A log past this many MiB is rotated at startup. Default 8.
        log_max_mb: Option<u32>,
        /// The usage ledger: a path or `off`. Default
        /// `$XDG_STATE_HOME/arnes/usage.jsonl`.
        ledger: Option<String>,
    }
}

section! {
    /// The free route arnes starts on when the configured provider cannot
    /// serve and no fallback can: a local llama.cpp server, then (macOS)
    /// Apple's on-device model.
    FreeRouteSection = "defarnes-free-route" {
        /// Default on. Off: an unusable provider is an error at startup.
        enabled: Option<bool>,
        /// The llama.cpp endpoint. Default `http://127.0.0.1:8080`.
        endpoint: Option<String>,
        /// Add Apple's on-device model after it. Default on on macOS.
        on_device: Option<bool>,
        /// Default `http://127.0.0.1:8434`.
        on_device_endpoint: Option<String>,
        /// Default `apple-on-device`.
        on_device_model: Option<String>,
    }
}

section! {
    RouteFacts = "defarnes-route-facts" {
        auth: Option<String>,
        describe: Option<String>,
        tier: Option<u32>,
        price_in: Option<f32>,
        price_out: Option<f32>,
        price_cache: Option<f32>,
        context_window: Option<u32>,
    }
}

section! {
    RoutingSection = "defarnes-routing" {
        enabled: Option<bool>,
        budget_ms: Option<u32>,
        tail_kib: Option<u32>,
        log: Option<String>,
        judge: Option<String>,
        min_confidence: Option<u32>,
        stick_turns: Option<u32>,
        cooldown_secs: Option<u32>,
        allow_paid: Option<bool>,
    }
}

section! {
    RouteSection = "defarnes-route" {
        label: Option<String>,
        tools: Option<bool>,
        facts: RouteFacts,
    }
}

section! {
    /// The provider's model catalogue cache.
    CatalogSection = "defarnes-catalog" {
        /// Hours a cached catalogue is trusted. 0..=720, default 24.
        max_age_hours: Option<u32>,
    }
}

section! {
    /// `@path` mentions.
    MentionsSection = "defarnes-mentions" {
        /// Files the `@` picker lists. 1..=1000000, default 20000.
        max_files: Option<u32>,
        /// Largest file attached, KiB. 1..=16384, default 256.
        max_kib: Option<u32>,
    }
}

section! {
    /// Prompt history: what `Up`/`Down` walk and `Ctrl-R` searches. The keys
    /// themselves are `keys.prompt` (`history:previous|next|search`).
    HistorySection = "defarnes-history" {
        /// `on` (default) or `off`: prompts are kept in
        /// `$XDG_STATE_HOME/arnes/history.jsonl` and recalled in later
        /// sessions. Off, history is this session's only.
        record: Option<String>,
        /// `project-then-global` (default), `project` or `global`.
        scope: Option<String>,
        /// Prompts kept on disk, newest first; 0 = unbounded.
        /// 0..=1000000, default 10000.
        keep: Option<u32>,
        /// `Ctrl-R`: `picker` (default: ranked fuzzy picker) or
        /// `incremental` (reverse-i-search).
        search: Option<String>,
        /// The picker's order: `frecency` (default) or `recency`.
        ranking: Option<String>,
        /// The most recent prompt stays first in the picker. Default on.
        pin_recent: Option<bool>,
    }
}

section! {
    /// One turn of the agent loop.
    TurnSection = "defarnes-turn" {
        /// Most model calls in one turn (each tool round is one). 1..=256,
        /// default 16.
        max_rounds: Option<u32>,
    }
}

section! {
    /// Skills: Claude-Code-style `SKILL.md` directories (docs/profiles.md §3).
    SkillsSection = "defarnes-skills" {
        /// Default on.
        enabled: Option<bool>,
        /// Directories holding one skill directory each. Default
        /// `~/.claude/skills` and `./.claude/skills`.
        dirs: Vec<String>,
        /// Globs of skill names to load; empty = all.
        only: Vec<String>,
        /// Globs of skill names never loaded.
        off: Vec<String>,
        /// Most skills listed to the model. 0..=2000, default 200.
        max_listed: Option<u32>,
    }
}

section! {
    /// What is added to the system prompt (docs/profiles.md §4).
    ContextSection = "defarnes-context" {
        /// Files appended under their path (`~` expands).
        files: Vec<String>,
        /// Read `CLAUDE.md` and `AGENTS.md` in the working directory.
        /// Default on.
        discover: Option<bool>,
        /// A fragment appended after the files.
        prompt: Option<String>,
        /// Largest context file read, KiB. 1..=4096, default 64.
        max_kib: Option<u32>,
    }
}

section! {
    /// What this session may reach (docs/profiles.md §2).
    AccessSection = "defarnes-access" {
        /// Directories (`~` expands) that `arnes_read_file` and `@path`
        /// attachments may read under; empty = anywhere.
        read_roots: Vec<String>,
        /// The API key files a route may read; empty = any. A route naming
        /// another is refused alone.
        key_files: Vec<String>,
    }
}

/// An integer knob read against its bounds: the value when inside
/// `lo..=hi`, else the default and a refusal naming the key. Unset is the
/// default with nothing said.
#[must_use]
pub fn bounded(key: &str, v: Option<u32>, lo: u32, hi: u32, default: u32) -> (u32, Option<String>) {
    match v {
        None => (default, None),
        Some(x) if (lo..=hi).contains(&x) => (x, None),
        Some(x) => (default, Some(format!("{key} {x} is outside {lo}..={hi}; using {default}"))),
    }
}

/// A word knob read against its legal set: the value when legal, else the
/// default and a refusal naming the key.
#[must_use]
pub fn one_of<'a>(key: &str, v: Option<&'a str>, legal: &[&'a str], default: &'a str) -> (&'a str, Option<String>) {
    match v {
        None => (default, None),
        Some(x) if legal.contains(&x) => (x, None),
        Some(x) => (default, Some(format!("{key} {x:?} is not one of {legal:?}; using {default}"))),
    }
}
