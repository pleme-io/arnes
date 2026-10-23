//! `arnes` — the agent-harness binary.
//!
//! A Rust/tatara-lisp open-source Claude-Code-equivalent, powered by:
//!   - **shikumi** for config (lisp / yaml / nix / toml)
//!   - **ratatui** for the TUI
//!   - **anvil-compatible MCP config** for shared tool infrastructure
//!   - **anthropic API** for the LLM
//!   - **deep integration with escriba**: the editor exposes escriba-mcp
//!     so arnes can drive it as an MCP tool server.
//!
//! Usage:
//!   arnes                             # open the TUI with config-loaded defaults
//!   arnes --message "summarize ~/x"   # one-shot message to stdout
//!   arnes --config ./demo.lisp        # override config path
//!   arnes --model deepseek-v4-pro     # override model (checked against the catalogue)
//!   arnes --models                    # list the provider's live model catalogue
//!   arnes --dump-config               # show the loaded config + exit
//!   arnes config show --sources       # the effective config and where each value came from
//!   arnes config validate [FILE]      # every refused key, exit 1 when any
//!   arnes config schema               # the JSON schema (docs/configuration.md)

// mimalloc as the global allocator: the TUI, the tatara-lisp runtime and
// the streaming paths are all allocation-bound.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use arnes_anthropic::{AnthropicClient, Fallback};
use arnes_config::ArnesConfig;
use arnes_core::{Conversation, ConversationId, Message};
use arnes_loop::permission::{
    Approver, ApprovalRequest, GatedDispatcher, NoHuman, Verdict,
};
use arnes_loop::ledger::Ledger;
use arnes_loop::run_turn_recorded;
use arnes_mcp_client::McpPool;
use arnes_core::{Effort, ThinkingMode};
use arnes_tui::{ApprovalView, Effect, Keymap, Overlay, Resolution, TuiState};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use egaku_term::{Buffer, Terminal, render_diff};
use tokio::sync::mpsc;


#[derive(Parser)]
#[command(
    name = "arnes",
    version,
    about = "agent harness — claude + mcp + tui, pleme-io native"
)]
struct Args {
    /// Override config path (otherwise shikumi discovers via $`ARNES_CONFIG` / $`XDG_CONFIG_HOME`).
    #[arg(long)]
    config: Option<std::path::PathBuf>,
    /// Override the model from the config.
    #[arg(long)]
    model: Option<String>,
    /// Send one message, print the reply to stdout, exit. Skips the TUI.
    #[arg(long)]
    message: Option<String>,
    /// Dump the loaded config as JSON + exit.
    #[arg(long)]
    dump_config: bool,
    /// Dump the merged MCP registry as JSON + exit.
    #[arg(long)]
    dump_mcp: bool,
    /// List the provider's model catalogue (refreshing the cache) + exit.
    #[arg(long)]
    models: bool,
    /// Resume a saved session: the latest for this directory, or the one an
    /// id prefix or list number (as `/resume` shows them) names.
    #[arg(long, value_name = "ID", num_args = 0..=1, default_missing_value = "")]
    resume: Option<String>,
    /// Start in this profile (docs/profiles.md); otherwise this directory's
    /// last one, the `choose-profile` hook's, or the config's `:profile`.
    #[arg(long, env = "ARNES_PROFILE")]
    profile: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// The configuration surface: show, schema, validate, tier.
    #[command(subcommand)]
    Config(config_cmd::ConfigCmd),
}

mod config_cmd;
mod routing;
mod mcp_health;
mod notice;
mod prompt_history;
mod record;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(Command::Config(cmd)) = args.command {
        ArnesConfig::register_domains();
        let code = config_cmd::run(cmd, args.config.as_deref())?;
        if code != 0 {
            std::process::exit(i32::from(code));
        }
        return Ok(());
    }
    // The TUI owns the terminal unless this is a one-shot or a dump.
    let surface = if args.message.is_some() || args.dump_config || args.dump_mcp || args.models {
        notice::Surface::Plain
    } else {
        notice::Surface::Tui
    };
    let toasts = notice::init(surface);

    ArnesConfig::register_domains();

    let mut cfg = if let Some(path) = &args.config {
        ArnesConfig::load_file(path)
            .map_err(anyhow::Error::msg)
            .context("loading config from --config path")?
    } else {
        ArnesConfig::load()
    };
    if let Some(model) = args.model {
        cfg.modelo = model;
    }
    // The file's config stays whole for switching; the session runs on the
    // chosen profile applied over it.
    let file_cfg = cfg.clone();
    let (cfg, active) = start_profile(cfg, args.profile.as_deref());

    if args.dump_config {
        println!("{}", serde_json::to_string_pretty(&cfg)?);
        return Ok(());
    }

    if args.dump_mcp {
        let reg = cfg.to_mcp_registry();
        println!("{}", serde_json::to_string_pretty(&reg)?);
        return Ok(());
    }

    // A provider whose key cannot be read cannot serve a single turn, so the
    // session starts on the first fallback that can, and says so.
    let cfg = match cfg.api_key() {
        Ok(_) => cfg,
        Err(e) => {
            if let Some(i) = cfg.fallbacks.iter().position(|f| f.resolve().is_ok()) {
                tracing::info!(target: notice::TARGET, "{e}; starting on fallback {} ({})", i + 1, cfg.fallbacks[i].provedor);
                cfg.promote(i)
            } else if let Some(free) = cfg.free_route() {
                tracing::info!(target: notice::TARGET, "{e}; starting on the free local route");
                free
            } else {
                anyhow::bail!("{e} (free-route is off, and no fallback resolves)");
            }
        }
    };
    let key = cfg.api_key().map_err(anyhow::Error::msg)?;
    // The catalogue is the authority on model ids: a retired or mistyped
    // name is refused here, naming what exists, instead of being silently
    // remapped by the provider. A failed refresh is blind, not empty. A
    // provider with no catalogue (a local server) serves what it was
    // started with, and nothing is checked.
    let provider = cfg.provider().map_err(anyhow::Error::msg)?;
    let (knobs, refused_knobs) = cfg.knobs();
    for r in refused_knobs {
        tracing::warn!("config {r}");
    }
    let max_age = if args.models { std::time::Duration::ZERO } else { knobs.catalog_max_age };
    let catalog = match (provider.models_url(), &key) {
        (Some((models_url, auth)), Some(key)) => {
            Some(arnes_anthropic::catalog::load_or_refresh(provider.name(), models_url, auth, key, max_age).await)
        }
        _ if args.models => anyhow::bail!("{} has no model catalogue; it serves the model it was started with", provider.name()),
        _ => None,
    };
    match catalog {
        None => {}
        Some(Ok(catalog)) if args.models => {
            for m in &catalog.models {
                println!(
                    "{:<24} context={:<9} max_output={:<9} effort={:<14} system={}",
                    m.id,
                    m.context_window.map_or("-".into(), |v| v.to_string()),
                    m.max_output_tokens.map_or("-".into(), |v| v.to_string()),
                    m.supported_levels().join("/"),
                    m.system_prompt_update().unwrap_or("-"),
                );
            }
            return Ok(());
        }
        Some(Ok(catalog)) => {
            catalog.require(&cfg.model()).map_err(anyhow::Error::msg)?;
        }
        Some(Err(e)) if args.models => anyhow::bail!("{e}"),
        Some(Err(e)) => tracing::warn!("model catalogue unavailable ({e}); not checking the model id"),
    }

    // Bound what accumulates on disk, off the startup path.
    sweep(&cfg);

    let (client, refused) = build_client(&cfg).map_err(anyhow::Error::msg)?;
    for r in refused {
        tracing::warn!("{r}; route skipped");
    }
    report_blue_refusals(&cfg);

    if let Some(msg) = args.message {
        return one_shot(&client, &cfg, &msg).await;
    }

    let watch = args.config.clone().or_else(ArnesConfig::discover_path);
    Box::pin(run_tui(&client, &cfg, Session { file: file_cfg, profile: active, watch, resume: args.resume }, toasts)).await
}

/// What the idle activity line says about the route: which one, and
/// whether it costs money.
fn route_label(provedor: &str, model: &str) -> String {
    match provedor {
        "local" if model == "local" || model.is_empty() => "local llama.cpp \u{b7} free".into(),
        "local" => format!("{model} \u{b7} local \u{b7} free"),
        p => format!("{model} \u{b7} {p} \u{b7} paid"),
    }
}

/// Reports each tool call to the activity line, then dispatches it.
struct Observed {
    inner: Arc<GatedDispatcher>,
    tx: std::sync::mpsc::Sender<arnes_tui::activity::Event>,
}

#[async_trait::async_trait]
impl arnes_loop::ToolDispatcher for Observed {
    async fn dispatch(&self, name: &str, input: &serde_json::Value) -> arnes_loop::Result<String> {
        use arnes_tui::activity::Event;
        let _ = self.tx.send(Event::ToolStart(name.to_owned()));
        let out = self.inner.dispatch(name, input).await;
        let _ = self.tx.send(Event::ToolEnd { name: name.to_owned(), ok: out.is_ok() });
        out
    }
}

/// The client `cfg` describes: its provider, then every fallback that
/// resolves, in order. Fallbacks that do not resolve come back by name so
/// the caller can say so; they never stop the others.
fn build_client(cfg: &ArnesConfig) -> std::result::Result<(AnthropicClient, Vec<String>), String> {
    let version = |c: AnthropicClient| match &cfg.api_version {
        Some(v) => c.with_api_version(v.clone()),
        None => c,
    };
    let (routes, refused) = cfg.fallback_routes();
    let mut fallbacks = Vec::with_capacity(routes.len());
    for r in routes {
        let params = cfg.route_params(&r)?;
        fallbacks.push(Fallback {
            label: r.label.clone(),
            model: r.model,
            client: version(
                AnthropicClient::new(r.key)
                    .with_label(r.label)
                    .with_base_url(r.endpoint)
                    .with_auth(r.auth)
                    .with_provider(r.provider.name())
                    .with_params(params)
                    .with_system_note(r.system_note),
            ),
            tools: r.tools,
        });
    }
    let p = cfg.primary()?;
    let client = AnthropicClient::new(p.key)
        .with_label(p.label)
        .with_base_url(p.endpoint)
        .with_auth(p.auth)
        .with_provider(p.provider.name())
        .with_tools(p.tools)
        .with_params(cfg.request_params()?)
        .with_system_note(p.system_note)
        .with_fallbacks(fallbacks);
    Ok((version(client), refused))
}

fn route_prices(cfg: &ArnesConfig) -> std::collections::HashMap<String, arnes_loop::ledger::Price> {
    let (routes, _) = cfg.fallback_routes();
    cfg.primary()
        .into_iter()
        .chain(routes)
        .filter_map(|r| {
            let f = &r.facts;
            let price = arnes_loop::ledger::Price {
                cache_hit: f64::from(f.price_cache.or(f.price_in)?),
                cache_miss: f64::from(f.price_in?),
                output: f64::from(f.price_out?),
            };
            Some((r.label, price))
        })
        .collect()
}

async fn one_shot(client: &AnthropicClient, cfg: &ArnesConfig, text: &str) -> Result<()> {
    let mut conv = Conversation::new(ConversationId(1), cfg.model());
    if let Some(sys) = &cfg.prompt_sistema {
        conv.system = Some(sys.clone());
    }
    conv.max_tokens = cfg.max_tokens;
    conv.push(Message::user_text(text));
    let pool = Arc::new(connect_mcp(cfg, cfg.knobs().0).await?);
    let (hosted, refused) = arnes_blue::bidamas::Registry::load(&arnes_blue::bidamas::BidamaPath::from_env(), &hosted_names(cfg));
    for r in refused {
        tracing::warn!("{r}; not hosted");
    }
    let (skills, notes) = session_skills(cfg, &hosted);
    let (system, more) = system_prompt(cfg, &skills);
    for n in notes.into_iter().chain(more) {
        tracing::warn!("{n}");
    }
    conv.system = system;
    let skills = Arc::new(skills);
    let tools = session_tools(cfg, &pool, &skills)?;
    let policy = cfg.permission_policy().map_err(anyhow::Error::msg)?;
    let gate = with_builtins(cfg, GatedDispatcher::new(pool, Arc::new(NoHuman), policy), &skills);
    let ledger = Ledger::new(cfg.ledger_path());
    ledger.set_prices(route_prices(cfg));
    let rounds = cfg.knobs().0.max_rounds;
    let _ = run_turn_recorded(client, &mut conv, &tools, &gate, rounds, Some(&ledger)).await?;
    // Print only the last assistant-text response.
    if let Some(last) = conv.messages.last() {
        println!("{}", last.text());
    }
    Ok(())
}

/// The skills this session lists (docs/profiles.md §3): `skills.dirs` (or
/// `~/.claude/skills` and `./.claude/skills`), selected by `skills.only` and
/// `skills.off`, then the hosted `skills` hooks, which receive them as JSON
/// and return the names to keep, one per line (nil keeps all).
fn session_skills(cfg: &ArnesConfig, hosted: &arnes_blue::bidamas::Registry) -> (arnes_tools::skills::Skills, Vec<String>) {
    use arnes_tools::skills::Skills;
    if cfg.skills.enabled == Some(false) {
        return (Skills::default(), Vec::new());
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let dirs: Vec<std::path::PathBuf> = if cfg.skills.dirs.is_empty() {
        arnes_tools::skills::default_dirs(std::env::var_os("HOME").map(std::path::PathBuf::from).as_deref(), &cwd)
    } else {
        cfg.skills.dirs.iter().map(|d| arnes_blue::expand_home(std::path::Path::new(d))).collect()
    };
    let (max, bad) = arnes_config::sections::bounded("skills.max-listed", cfg.skills.max_listed, 0, 2000, 200);
    let (mut skills, mut notes) = Skills::discover(&dirs, &cfg.skills.only, &cfg.skills.off, max as usize);
    notes.extend(bad);
    let hooks = hosted.hooks("skills", &cfg.modules_off);
    match arnes_blue::policy("skills", &hooks, &skills.json(), module_budget(cfg)) {
        Some(Ok(keep)) => skills.keep(&keep.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>()),
        Some(Err(e)) => notes.push(format!("{e}; every skill kept")),
        None => {}
    }
    (skills, notes)
}

/// The system prompt: `:prompt-sistema`, then the context files (`CLAUDE.md`
/// and `AGENTS.md` here with `context.discover`, then `context.files`), then
/// `context.prompt`, then the skills listing (docs/profiles.md §4). A file
/// that cannot be read, is outside `access.read-roots`, or is over
/// `context.max-kib` is named and skipped.
fn system_prompt(cfg: &ArnesConfig, skills: &arnes_tools::skills::Skills) -> (Option<String>, Vec<String>) {
    let mut parts: Vec<String> = cfg.prompt_sistema.iter().cloned().collect();
    let mut notes = Vec::new();
    let (max_kib, bad) = arnes_config::sections::bounded("context.max-kib", cfg.context.max_kib, 1, 4096, 64);
    notes.extend(bad);
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    if cfg.context.discover != Some(false) {
        files.extend(["CLAUDE.md", "AGENTS.md"].iter().map(std::path::PathBuf::from).filter(|p| p.is_file()));
    }
    files.extend(cfg.context.files.iter().map(|f| arnes_blue::expand_home(std::path::Path::new(f))));
    for f in files {
        if !cfg.read_allowed(&f) {
            notes.push(format!("context {}: outside access.read-roots; skipped", f.display()));
            continue;
        }
        match std::fs::metadata(&f) {
            Ok(m) if m.len() > u64::from(max_kib) * 1024 => notes.push(format!("context {}: over {max_kib} KiB; skipped", f.display())),
            Ok(_) => match std::fs::read_to_string(&f) {
                Ok(text) => parts.push(format!("Context from {}:\n\n{}", f.display(), text.trim_end())),
                Err(e) => notes.push(format!("context {}: {e}; skipped", f.display())),
            },
            Err(e) => notes.push(format!("context {}: {e}; skipped", f.display())),
        }
    }
    parts.extend(cfg.context.prompt.iter().cloned());
    parts.extend(skills.listing());
    ((!parts.is_empty()).then(|| parts.join("\n\n")), notes)
}

fn new_conversation(cfg: &ArnesConfig) -> Conversation {
    let mut conv = Conversation::new(ConversationId(1), cfg.model());
    if let Some(sys) = &cfg.prompt_sistema {
        conv.system = Some(sys.clone());
    }
    conv.max_tokens = cfg.max_tokens;
    conv
}

/// A finished turn, sent back from its task.
struct TurnDone {
    conversation: Conversation,
    result: std::result::Result<(), String>,
}

/// Bracketed paste + the Kitty keyboard protocol's escape disambiguation
/// (what makes `Shift-Enter` distinguishable from `Enter`), undone on drop.
/// Owed upstream: these belong in `egaku_term::Terminal` as options, so
/// every fleet TUI gets them (docs/tui-parity.md §5).
struct Protocols;

impl Protocols {
    fn enable() -> std::io::Result<Self> {
        use crossterm::event::{
            EnableBracketedPaste, KeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
        };
        let mut out = std::io::stdout();
        crossterm::execute!(out, EnableBracketedPaste)?;
        // Terminals without the protocol ignore the push; the portable
        // newline fallbacks (Alt-Enter, Ctrl-J, `\`+Enter) still work there.
        let _ = crossterm::execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
        Ok(Self)
    }
}

impl Drop for Protocols {
    fn drop(&mut self) {
        use crossterm::event::{DisableBracketedPaste, PopKeyboardEnhancementFlags};
        let mut out = std::io::stdout();
        let _ = crossterm::execute!(out, PopKeyboardEnhancementFlags, DisableBracketedPaste);
    }
}

/// `Alt-T`: off → low → high → max → off. Claude Code toggles thinking;
/// `DeepSeek`'s real control is effort, so arnes cycles it (§4.9).
fn next_thinking(t: ThinkingMode) -> ThinkingMode {
    match t {
        ThinkingMode::Off => ThinkingMode::On { effort: Some(Effort::Low), budget: None },
        ThinkingMode::On { effort: Some(Effort::Low), budget } => {
            ThinkingMode::On { effort: Some(Effort::High), budget }
        }
        ThinkingMode::On { effort: Some(Effort::High), budget } => {
            ThinkingMode::On { effort: Some(Effort::Max), budget }
        }
        ThinkingMode::On { effort: Some(Effort::Max), .. } => ThinkingMode::Off,
        ThinkingMode::ProviderDefault | ThinkingMode::On { effort: None, .. } => {
            ThinkingMode::On { effort: Some(Effort::Low), budget: None }
        }
    }
}

fn thinking_label(t: ThinkingMode) -> String {
    match t {
        ThinkingMode::ProviderDefault => "default".into(),
        ThinkingMode::Off => "off".into(),
        ThinkingMode::On { effort: Some(e), .. } => e.name().into(),
        ThinkingMode::On { effort: None, .. } => "on".into(),
    }
}

/// Resolve one key press: typed text goes straight to the prompt; every
/// command goes through the keymap to an `Action` and the reducer.
fn handle_key(state: &mut TuiState, keymap: &mut Keymap, ke: KeyEvent) -> Vec<Effect> {
    use crossterm::event::KeyModifiers;
    // Vim: Normal/Command keys go through escriba's pipeline; Insert keeps
    // the Claude Code keys; Ctrl chords stay global (Ctrl-C, Ctrl-D…).
    if let Some(mode) = state.input.vim_mode() {
        let ctrl = ke.modifiers.contains(KeyModifiers::CONTROL);
        match mode {
            escriba_core::Mode::Insert if ke.code == KeyCode::Esc && !ctrl => {
                return state.input.vim_key(&escriba_keymap::Key::Esc, state.busy);
            }
            escriba_core::Mode::Insert => {}
            _ if ctrl => {}
            escriba_core::Mode::Normal if ke.code == KeyCode::Enter => {
                return state.input.apply(arnes_tui::Action::Submit, state.busy, Instant::now());
            }
            escriba_core::Mode::Normal
                if ke.code == KeyCode::Esc
                    && state.busy
                    && !state.input.vim.as_ref().is_some_and(arnes_tui::vim::VimInput::is_pending) =>
            {
                return state.input.apply(arnes_tui::Action::Cancel, state.busy, Instant::now());
            }
            _ => {
                if let Some(k) = to_vim_key(&ke) {
                    return state.input.vim_key(&k, state.busy);
                }
            }
        }
    }
    // An open picker reads its navigation keys first (Ctrl-j/k, Ctrl-n/p,
    // arrows, Tab); typed letters still filter.
    if state.input.picker.is_some()
        && let Some(k) = arnes_tui::nav::nav_key(&ke)
        && let Some(effects) = state.input.picker_nav_key(k)
    {
        return effects;
    }
    if let KeyCode::Char(c) = ke.code
        && !ke.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            if keymap.is_pending() {
                keymap.cancel_pending();
                return vec![];
            }
            return state.input.type_char(c);
        }
    let Some(hk) = egaku_term::event::to_hotkey(&ke) else { return vec![] };
    match keymap.resolve(hk) {
        Resolution::Action(a) => state.input.apply(a, state.busy, Instant::now()),
        Resolution::Pending | Resolution::Unbound => vec![],
    }
}

/// crossterm → escriba's key vocabulary (Ctrl chords are handled globally).
fn to_vim_key(ke: &KeyEvent) -> Option<escriba_keymap::Key> {
    use crossterm::event::KeyModifiers;
    use escriba_keymap::Key;
    Some(match ke.code {
        KeyCode::Char(c) if ke.modifiers.contains(KeyModifiers::ALT) => Key::Alt(c),
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Esc => Key::Esc,
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab => Key::Tab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        _ => return None,
    })
}

/// Files a `@` mention can name: everything under the working directory
/// that git would not ignore (.gitignore / .ignore / global excludes),
/// relative paths, sorted, capped so a huge tree cannot stall the picker.
/// Uses the `ignore` crate directly — codesearch has a fuller walker locked
/// inside a heavy crate; a second consumer is the signal to lift one
/// (docs/integrations.md).
fn mention_index(max_files: usize) -> Vec<String> {
    let Ok(cwd) = std::env::current_dir() else { return Vec::new() };
    let mut files: Vec<String> = ignore::WalkBuilder::new(&cwd)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|e| e.path().strip_prefix(&cwd).ok().map(|p| p.to_string_lossy().into_owned()))
        .take(max_files)
        .collect();
    files.sort();
    files
}

/// Attach the contents of every `@path` in `prompt` that names a readable
/// UTF-8 file of at most `max_bytes` (`mentions.max-kib`). The prompt text is kept as typed; each
/// file follows it in a delimited block. Returns the expanded prompt and
/// how many files were attached.
fn expand_mentions(prompt: &str, max_bytes: u64, allowed: &dyn Fn(&std::path::Path) -> bool) -> (String, usize) {
    let mut out = prompt.to_owned();
    let mut attached = 0;
    let mut seen = std::collections::HashSet::new();
    for token in prompt.split_whitespace() {
        let Some(path) = token.strip_prefix('@') else { continue };
        let path = path.trim_end_matches([',', '.', ';', ':', ')']);
        if path.is_empty() || !seen.insert(path.to_owned()) {
            continue;
        }
        let p = std::path::Path::new(path);
        let small = allowed(p) && std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() <= max_bytes);
        if let (true, Ok(text)) = (small, std::fs::read_to_string(p)) {
            out.push_str(&format!("\n\n--- @{path} ---\n{text}"));
            if !text.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&format!("--- end @{path} ---"));
            attached += 1;
        }
    }
    (out, attached)
}

/// Open `text` in `$VISUAL` / `$EDITOR` (default `vi`) and return the
/// edited result. The caller has already restored the terminal.
fn edit_externally(text: &str) -> Result<String> {
    let path = std::env::temp_dir().join(format!("arnes-prompt-{}.md", std::process::id()));
    std::fs::write(&path, text)?;
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let status = std::process::Command::new(program).args(parts).arg(&path).status()?;
    let edited = std::fs::read_to_string(&path)?;
    let _ = std::fs::remove_file(&path);
    anyhow::ensure!(status.success(), "{program} exited with {status}");
    Ok(edited)
}

/// What one turn runs against; cloned into the turn's task, so a reload or
/// a route switch mid-turn changes the next turn and never this one.
struct TurnKit<'a> {
    client: &'a AnthropicClient,
    gate: &'a Arc<GatedDispatcher>,
    tools: &'a Arc<Vec<arnes_core::Tool>>,
    ledger: &'a Arc<Ledger>,
    tx: &'a mpsc::Sender<TurnDone>,
    activity: &'a std::sync::mpsc::Sender<arnes_tui::activity::Event>,
    rounds: u32,
    /// The route's endpoint and whether it is the free local one, for the
    /// failure message.
    route: (String, bool),
}

/// A failed turn in one line an operator can act on. An unreachable local
/// server is the common case of the free default, so it says what to start
/// and how to route around it rather than printing a transport error.
fn turn_failure(e: &arnes_loop::LoopError, route: &(String, bool)) -> String {
    use arnes_loop::LoopError;
    match e {
        LoopError::Anthropic(a) if a.brief() == "unreachable" && route.1 => format!(
            "the local model at {} is not reachable: start llama-server there (pleme.llm.server), or pick another route with /model",
            route.0
        ),
        LoopError::Anthropic(a) => format!("no route could answer: {} ({})", a.brief(), route.0),
        other => other.to_string(),
    }
}

struct Routed {
    answer: Option<std::result::Result<String, String>>,
    facts: String,
}

fn begin_turn(state: &mut TuiState, live: &Live, kit: &TurnKit<'_>, prompt: &str, routed: &mpsc::UnboundedSender<Routed>) -> tokio::task::JoinHandle<()> {
    let hooks = live.hosted.hooks(routing::EVENT, &live.cfg.modules_off);
    if !routing::enabled(&live.cfg) || hooks.is_empty() {
        return start_turn(state, kit);
    }
    let ask = routing::Ask {
        cfg: &live.cfg,
        base: &live.base,
        client: &live.client,
        totals: kit.ledger.totals(),
        conversation: &state.conversation,
        prompt,
        source: live.route_source,
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let facts = routing::facts(&ask, &conversation_text(&state.conversation), now);
    let (budget, reach, tx) = (routing::budget(&live.cfg), live.reach.clone(), routed.clone());
    state.set_busy(true);
    state.activity.note("choosing a route", false, Instant::now());
    tokio::spawn(async move {
        let asked = facts.clone();
        let answer = tokio::task::spawn_blocking(move || arnes_blue::policy_with(routing::EVENT, &hooks, &asked, budget, reach))
            .await
            .ok()
            .flatten()
            .map(|r| r.map_err(|e| e.to_string()));
        let _ = tx.send(Routed { answer, facts });
    })
}

fn apply_routed(live: &mut Live, state: &mut TuiState, answer: Option<std::result::Result<String, String>>) -> String {
    match answer {
        None => "{\"abstained\":true}".to_owned(),
        Some(Err(e)) => {
            state.toast(arnes_tui::ToastLevel::Warn, format!("route choice: {e}; the current route kept"));
            serde_json::json!({ "error": e }).to_string()
        }
        Some(Ok(text)) => {
            let a = routing::answer(&text);
            if let Some(why) = &a.why {
                state.activity.note(format!("route: {why}"), false, Instant::now());
            }
            if let Some(label) = a.route.filter(|l| *l != routing::label_of(&live.cfg)) {
                if routing::labels(&live.base).contains(&label) {
                    match switch_model(&live.base, &live.cfg, &label, None).and_then(|next| live.switch_route(&next, state).map(|_| next)) {
                        Ok(next) => {
                            state.conversation.system = system_prompt(&next, &live.skills).0;
                            live.cfg = next;
                            live.route_source = "auto";
                        }
                        Err(e) => state.toast(arnes_tui::ToastLevel::Warn, format!("route {label}: {e}; the current route kept")),
                    }
                } else {
                    state.toast(arnes_tui::ToastLevel::Warn, format!("route choice named {label:?}, which is not declared; the current route kept"));
                }
            }
            text
        }
    }
}

fn start_turn(state: &mut TuiState, kit: &TurnKit<'_>) -> tokio::task::JoinHandle<()> {
    let (rounds, route) = (kit.rounds, kit.route.clone());
    state.set_busy(true);
    state.last_error = None;
    // The turn owns a snapshot; the UI keeps rendering and reading keys, so
    // Esc / Ctrl-C can interrupt it (no lock held across the network call).
    let mut conversation = state.conversation.clone();
    let (client, gate, tools, tx, ledger) =
        (kit.client.clone(), kit.gate.clone(), kit.tools.clone(), kit.tx.clone(), kit.ledger.clone());
    let observed = Observed { inner: gate, tx: kit.activity.clone() };
    tokio::spawn(async move {
        let result = run_turn_recorded(&client, &mut conversation, &tools, &observed, rounds, Some(&ledger))
            .await
            .map(|_| ())
            .map_err(|e| turn_failure(&e, &route));
        let _ = tx.send(TurnDone { conversation, result }).await;
    })
}

/// A reconciled MCP pool, back from its task.
struct McpSwap {
    pool: McpPool,
    report: arnes_mcp_client::Reconciled,
    registry: arnes_mcp_client::McpRegistry,
}

/// Everything a session swaps when its config changes. The conversation, the
/// prompt, the scrollback, the notices and the session file live in
/// [`TuiState`] and the recorder and are never touched by a swap.
struct Live {
    cfg: ArnesConfig,
    /// The file's config, before any `/model` switch.
    base: ArnesConfig,
    client: AnthropicClient,
    registry: arnes_mcp_client::McpRegistry,
    pool: Arc<McpPool>,
    tools: Arc<Vec<arnes_core::Tool>>,
    gate: Arc<GatedDispatcher>,
    knobs: arnes_config::Knobs,
    keymap: Keymap,
    commands: Vec<arnes_config::CommandDecl>,
    /// The hosted bidamas' exports (`docs/kernel.md`), re-read on reload.
    hosted: arnes_blue::bidamas::Registry,
    hooks: arnes_blue::Hooks,
    notifications: bool,
    /// The pool the vigy probe reads; follows every reconcile, so a removed
    /// server is not kept alive by the probe.
    pool_tx: tokio::sync::watch::Sender<Arc<McpPool>>,
    /// The health scheduler's view of the servers, the schedule and the
    /// `mcp-probe` hooks, each following reloads and profile switches.
    registry_tx: tokio::sync::watch::Sender<arnes_mcp_client::McpRegistry>,
    health_tx: tokio::sync::watch::Sender<Option<(arnes_mcp_client::health::Policy, Duration, Duration)>>,
    probe_hooks_tx: tokio::sync::watch::Sender<Vec<arnes_blue::Script>>,
    /// The file's config without the profile, and the active profile: every
    /// reload and every switch applies the one over the other.
    file: ArnesConfig,
    profile: Option<String>,
    /// The skills listed and served this session.
    skills: Arc<arnes_tools::skills::Skills>,
    route_source: &'static str,
    reach: Option<arnes_blue::Reach>,
}

impl Live {
    /// `loaded` (the file) with the active profile applied. A profile that
    /// no longer exists is dropped with a notice and the file applies alone.
    fn profiled(&mut self, mut loaded: arnes_config::Loaded, state: &mut TuiState) -> arnes_config::Loaded {
        match loaded.config.with_profile(self.profile.as_deref()) {
            Ok((config, refused)) => {
                loaded.config = config;
                loaded.refused.extend(refused);
            }
            Err(e) => {
                state.toast(arnes_tui::ToastLevel::Warn, format!("{e}; no profile active"));
                self.profile = None;
            }
        }
        state.profile = self.profile.clone().unwrap_or_default();
        loaded
    }

    /// Switch to `name` (`None` or `-`: no profile), remember it for this
    /// directory, and re-apply the file through the reload path, so MCP
    /// servers are diffed and everything else swaps as on a file edit.
    fn switch_profile(&mut self, name: &str, state: &mut TuiState, reapply: &mpsc::UnboundedSender<std::result::Result<arnes_config::Loaded, String>>) {
        let next = match name.trim() {
            "" | "-" | "none" => None,
            n if self.file.profiles.iter().any(|p| p.name == n) => Some(n.to_owned()),
            n => {
                let known: Vec<String> = self.file.profile_choices().into_iter().map(|(n, _)| n).collect();
                state.set_error(format!("/profile: no profile {n:?}; declared: {}", if known.is_empty() { "none".into() } else { known.join(", ") }));
                return;
            }
        };
        self.profile = next;
        #[cfg(not(test))]
        if let (Some(layout), Ok(dir)) = (arnes_store::Layout::from_env(), std::env::current_dir())
            && let Err(e) = arnes_store::profiles::remember(&layout, &dir, self.profile.as_deref())
        {
            tracing::debug!("profile not remembered: {e}");
        }
        let file = arnes_config::Loaded { config: self.file.clone(), refused: Vec::new(), file: serde_json::Value::Null, env: serde_json::Value::Null };
        let _ = reapply.send(Ok(file));
        state.input.hint = Some(match &self.profile {
            Some(p) => format!("profile {p}"),
            None => "no profile".to_owned(),
        });
    }

    /// Point the health scheduler at the current config.
    fn feed_health(&self) {
        let (policy, timeout, _, _) = self.cfg.mcp_health();
        self.health_tx.send_replace(policy.map(|p| (p, timeout, self.knobs.mcp_connect)));
        self.probe_hooks_tx.send_replace(self.hosted.hooks("mcp-probe", &self.cfg.modules_off));
        self.registry_tx.send_replace(self.registry.clone());
    }

    /// One server finished connecting (at startup, or reconnected by the
    /// health scheduler): attach it, unless the config moved on and no longer
    /// names it as it was.
    fn arrived(&mut self, arrival: arnes_mcp_client::Arrival, state: &mut TuiState) {
        use arnes_tui::ToastLevel;
        if self.registry.get(&arrival.spec.name) != Some(&arrival.spec) {
            return;
        }
        let name = arrival.spec.name.clone();
        let failed = arrival.error().map(str::to_owned);
        let required = arrival.spec.required;
        self.pool = Arc::new(self.pool.with_arrival(arrival));
        self.pool_tx.send_replace(self.pool.clone());
        self.rebuild_gate();
        self.rebuild_tools(state);
        let (up, all) = (self.pool.servers().len(), self.registry.servers.iter().filter(|s| !s.lazy).count());
        match failed {
            Some(why) if required => state.toast(ToastLevel::Error, format!("required MCP server {name} failed to start: {why}")),
            Some(why) => state.toast(ToastLevel::Warn, format!("MCP server {name} unavailable: {why}")),
            None => state.activity.note(format!("MCP {name} up ({up}/{all})"), true, Instant::now()),
        }
        if let Some(m) = state.mcp.as_mut() {
            *m = mcp_browser(&self.cfg, &self.pool, None).with_keymap(&state.nav_menu);
        }
    }

    fn route(&self) -> (String, bool) {
        let endpoint = self.cfg.resolved_endpoint().unwrap_or_default();
        (endpoint, self.cfg.provider() == Ok(arnes_config::Provider::Local))
    }

    /// Rebuild the gate over the current pool, keeping the session's
    /// answers and mode.
    fn rebuild_gate(&mut self) {
        let next = self.gate.successor(self.pool.clone(), self.gate.policy());
        self.gate = Arc::new(with_builtins(&self.cfg, next, &self.skills));
    }

    fn rebuild_tools(&mut self, state: &mut TuiState) {
        match session_tools(&self.cfg, &self.pool, &self.skills) {
            Ok(t) => {
                state.tools = t.len();
                self.tools = Arc::new(t);
            }
            Err(e) => state.toast(arnes_tui::ToastLevel::Warn, format!("tool list kept: {e}")),
        }
    }

    /// Move the conversation onto `next`'s primary route: the client is
    /// rebuilt, the thinking the new route cannot accept is dropped (and
    /// said so), and the next turn goes there. Nothing else changes.
    fn switch_route(&mut self, next: &ArnesConfig, state: &mut TuiState) -> std::result::Result<Vec<String>, String> {
        let (mut rebuilt, refused) = build_client(next)?;
        rebuilt.params.thinking = self.client.params.thinking;
        rebuilt.health = self.client.health.clone();
        if next.drops_foreign_thinking(&self.cfg) {
            let n = state.conversation.drop_thinking();
            if n > 0 {
                state.toast(
                    arnes_tui::ToastLevel::Info,
                    format!("{n} thinking block(s) from {} dropped: {} cannot accept another provider's signed reasoning", self.cfg.provedor, next.provedor),
                );
            }
        }
        self.client = rebuilt;
        state.model = next.model();
        state.conversation.model = next.model();
        state.activity.route = route_label(&next.provedor, &next.model());
        Ok(refused)
    }

    /// Apply a reloaded config in place. Every part that changed is swapped;
    /// a value that cannot apply is named in a notice and the running one
    /// kept. MCP servers are reconciled in the background (only the changed
    /// ones start or stop) and arrive through `mcp_tx`.
    fn reload(&mut self, loaded: arnes_config::Loaded, state: &mut TuiState, depth: egaku_term::theme::ColorDepth, mcp_tx: &mpsc::UnboundedSender<McpSwap>) -> ReloadEffects {
        use arnes_tui::ToastLevel;
        for r in &loaded.refused {
            state.toast(ToastLevel::Warn, format!("config {r}; default kept"));
        }
        let file = loaded.config;
        if self.base.route_differs(&file) {
            self.route_source = "config";
        }
        let (reach, refused_endpoints) = routing::reach_for(&file);
        self.reach = reach;
        for r in refused_endpoints {
            state.toast(ToastLevel::Warn, r);
        }
        // A reload that leaves the file's route alone keeps the one /model chose.
        let new = if self.base.route_differs(&file) { file.clone() } else { file.with_route_of(&self.cfg) };
        let plan = self.cfg.reload_plan(&new);
        let mut effects = ReloadEffects::default();
        match build_client(&new) {
            Ok(_) if plan.route => match self.switch_route(&new, state) {
                Ok(refused) if !refused.is_empty() => state.toast(ToastLevel::Warn, format!("route skipped: {}", refused.join("; "))),
                Ok(_) => {}
                Err(e) => state.toast(ToastLevel::Error, format!("config: {e}; the running route kept")),
            },
            Ok((mut rebuilt, refused)) => {
                rebuilt.params.thinking = if new.thinking_mode() == self.cfg.thinking_mode() { self.client.params.thinking } else { rebuilt.params.thinking };
                rebuilt.health = self.client.health.clone();
                self.client = rebuilt;
                state.model = new.model();
                state.conversation.model = new.model();
                for r in refused {
                    state.toast(ToastLevel::Warn, format!("{r}; route skipped"));
                }
            }
            Err(e) => state.toast(ToastLevel::Error, format!("config: {e}; the running route kept")),
        }
        state.conversation.max_tokens = new.max_tokens;
        state.effort = thinking_label(self.client.params.thinking);
        match new.permission_policy() {
            Ok(p) => {
                self.gate.set_policy(p);
                state.permission_mode = self.gate.mode().name().into();
            }
            Err(e) => state.toast(ToastLevel::Warn, format!("config {e}; the running permission rules kept")),
        }
        self.hosted = host_bidamas(&new, state);
        // Skills and context swap for the next turn; the messages stay.
        let (skills, notes) = session_skills(&new, &self.hosted);
        let (system, more) = system_prompt(&new, &skills);
        for n in notes.into_iter().chain(more) {
            state.toast(ToastLevel::Warn, n);
        }
        let skills_changed = *self.skills != skills;
        self.skills = Arc::new(skills);
        state.conversation.system = system;
        let (keymap, refused) = state.apply_config(&new, depth);
        self.keymap = keymap;
        let (keymap, bad) = hosted_keymap(&new, &self.hosted, std::mem::replace(&mut self.keymap, Keymap::claude_code()));
        self.keymap = keymap;
        for r in bad {
            state.toast(ToastLevel::Warn, format!("{r}; not bound"));
        }
        apply_hosted_colors(&new, &self.hosted, state);
        for r in refused {
            state.toast(ToastLevel::Warn, format!("config {r}"));
        }
        let (knobs, refused) = new.knobs();
        for r in refused {
            state.toast(ToastLevel::Warn, format!("config {r}"));
        }
        effects.pacer = knobs.frame_rate != self.knobs.frame_rate;
        effects.mentions = knobs.mention_max_files != self.knobs.mention_max_files;
        self.knobs = knobs;
        self.notifications = new.notifications_mode().as_deref() != Some("off");
        state.activity.route = route_label(&new.provedor, &new.model());
        self.commands = blue_commands(&new);
        self.hooks = new.blue_hooks().0;
        state.input.user_commands = picker_commands(&new, &self.hosted, &self.commands);
        report_blue_refusals(&new);
        effects.sessions = (self.cfg.persistence().sessions != new.persistence().sessions).then_some(new.persistence().sessions);
        effects.sweep = plan.sweep;
        effects.vigies = plan.vigies;
        let (registry, notes) = mcp_registry(&new, &self.hosted);
        for n in notes {
            state.toast(ToastLevel::Warn, n);
        }
        let servers_changed = registry.servers != self.registry.servers;
        self.cfg = new;
        self.base = file;
        if plan.gate || skills_changed {
            self.rebuild_gate();
        }
        if plan.tools || plan.gate || skills_changed {
            self.rebuild_tools(state);
        }
        if servers_changed || plan.mcp_call_timeout {
            let (pool, previous, timeout, call) = (self.pool.clone(), self.registry.clone(), self.knobs.mcp_connect, self.knobs.mcp_call);
            let tx = mcp_tx.clone();
            tokio::spawn(async move {
                let (next, report) = pool.reconcile(&previous, &registry, timeout, notice::mcp_log_dir().as_deref()).await;
                let _ = tx.send(McpSwap { pool: next.with_call_timeout(call), report, registry });
            });
            state.activity.note("reconnecting changed MCP servers", true, Instant::now());
        }
        self.feed_health();
        for r in plan.restart {
            state.toast(ToastLevel::Warn, format!("{r}; applies on restart"));
        }
        state.input.hint = Some("config reloaded".into());
        effects
    }

    /// A reconciled pool arrived: swap it in, with the gate and tool list
    /// over it. Unchanged servers never disconnected.
    fn mcp_swapped(&mut self, swap: McpSwap, state: &mut TuiState) {
        use arnes_tui::ToastLevel;
        let McpSwap { pool, report, registry } = swap;
        self.pool = Arc::new(pool);
        self.pool_tx.send_replace(self.pool.clone());
        self.registry = registry;
        self.feed_health();
        self.rebuild_gate();
        self.rebuild_tools(state);
        for (server, why) in &report.failed {
            state.toast(ToastLevel::Warn, format!("MCP server {server} unavailable: {why}"));
        }
        if !report.is_noop() {
            let say = |what: &str, v: &[String]| (!v.is_empty()).then(|| format!("{what} {}", v.join(", ")));
            let parts: Vec<String> = [say("started", &report.started), say("stopped", &report.stopped)].into_iter().flatten().collect();
            state.activity.note(format!("MCP {}; {} kept", parts.join("; "), report.kept.len()), true, Instant::now());
        }
        if let Some(m) = state.mcp.as_mut() {
            *m = mcp_browser(&self.cfg, &self.pool, None).with_keymap(&state.nav_menu);
        }
    }
}

/// What a reload asks of the loop around it: independent flags, one per
/// part of the session the loop owns.
#[derive(Default)]
#[allow(clippy::struct_excessive_bools)]
struct ReloadEffects {
    /// The frame rate changed: a new pacer.
    pacer: bool,
    /// The mention limit changed: list the files again on the next `@`.
    mentions: bool,
    /// Session recording turned on (`Some(true)`) or off.
    sessions: Option<bool>,
    /// Retention changed: sweep now.
    sweep: bool,
    /// The declared vigies changed.
    vigies: bool,
}

/// Sweep the store against `cfg`'s retention, off the UI thread.
fn sweep(cfg: &ArnesConfig) {
    if let Some(layout) = arnes_store::Layout::from_env() {
        let policy = cfg.persistence();
        std::thread::spawn(move || {
            let s = arnes_store::sweep(&layout, &policy, std::time::SystemTime::now());
            tracing::debug!(sessions_removed = s.sessions_removed.len(), logs_rotated = s.logs_rotated.len(), "store sweep");
        });
    }
}

/// Tell the operator, before the first turn, when the free local route is
/// down: a TCP connect with a short deadline, off the UI thread. The turn
/// itself would still say so; this says it while they are still typing.
fn probe_local(cfg: &ArnesConfig) {
    if cfg.provider() != Ok(arnes_config::Provider::Local) {
        return;
    }
    let Ok(endpoint) = cfg.resolved_endpoint() else { return };
    let has_fallback = !cfg.fallbacks.is_empty();
    tokio::spawn(async move {
        let authority = endpoint.split("://").nth(1).unwrap_or(&endpoint).split('/').next().unwrap_or_default().to_owned();
        let addr = if authority.contains(':') { authority } else { format!("{authority}:80") };
        let reached = tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(&addr)).await;
        if !matches!(reached, Ok(Ok(_))) {
            let then = if has_fallback { "turns go to the next route until it is up" } else { "start llama-server there, or pick another route with /model" };
            tracing::warn!(target: notice::TARGET, "the local model at {endpoint} is not reachable; {then}");
        }
    });
}

/// What a TUI session starts from besides its effective config.
struct Session {
    /// The file's config, without the profile.
    file: ArnesConfig,
    profile: Option<String>,
    watch: Option<std::path::PathBuf>,
    resume: Option<String>,
}

async fn run_tui(
    client: &AnthropicClient,
    cfg: &ArnesConfig,
    session: Session,
    toasts: std::sync::mpsc::Receiver<(arnes_tui::ToastLevel, String)>,
) -> Result<()> {
    let Session { file, profile, watch, resume } = session;
    // The file is watched here and read through the one load path, so a
    // reload refuses a bad key alone exactly as startup does; a file that
    // does not parse at all arrives as its error.
    let (reload_tx, mut reload_rx) = mpsc::unbounded_channel::<std::result::Result<arnes_config::Loaded, String>>();
    // A profile switch re-applies the file through the same reload path.
    let reapply = reload_tx.clone();
    let _watcher = watch.as_deref().and_then(|path| {
        let file = path.to_path_buf();
        shikumi::ConfigWatcher::watch(path, move |ev| {
            // A removal is a rebuild mid-swap: wait for the replacement.
            if shikumi::WatchEventClass::classify(&ev.kind) != shikumi::WatchEventClass::Reload {
                return;
            }
            let _ = reload_tx.send(ArnesConfig::load_report(&file));
        })
        .map_err(|e| tracing::warn!(error = %e, "config hot reload unavailable"))
        .ok()
    });
    let (knobs, _) = cfg.knobs();
    let mut state = TuiState::new(new_conversation(cfg), cfg.model());
    let hosted = host_bidamas(cfg, &mut state);
    let (skills, notes) = session_skills(cfg, &hosted);
    let (system, more) = system_prompt(cfg, &skills);
    for n in notes.into_iter().chain(more) {
        state.toast(arnes_tui::ToastLevel::Warn, n);
    }
    state.conversation.system = system;
    let skills = Arc::new(skills);
    let (registry, notes) = mcp_registry(cfg, &hosted);
    for n in notes {
        tracing::warn!("{n}");
    }
    // Background startup (docs/profiles.md §1): the first frame draws with
    // only the dormant servers in the pool; each other server connects on its
    // own task and is attached as it arrives. `mcp.startup "block"` waits.
    let (background, refused) = cfg.mcp_background();
    if let Some(r) = refused {
        tracing::warn!("config {r}");
    }
    let (health_out, mut health_rx) = mpsc::unbounded_channel::<mcp_health::Msg>();
    let pool = if background {
        let dormant = arnes_mcp_client::McpRegistry { servers: registry.servers.iter().filter(|s| s.lazy).cloned().collect() };
        let pool = McpPool::connect_logging(&dormant, knobs.mcp_connect, notice::mcp_log_dir()).await?.with_call_timeout(knobs.mcp_call);
        for spec in registry.servers.iter().filter(|s| !s.lazy).cloned() {
            let tx = health_out.clone();
            let timeout = knobs.mcp_connect;
            tokio::spawn(async move {
                let a = McpPool::connect_server(spec, timeout, notice::mcp_log_dir().as_deref()).await;
                let _ = tx.send(mcp_health::Msg::Arrived(a));
            });
        }
        Arc::new(pool)
    } else {
        Arc::new(connect_mcp(cfg, knobs).await?)
    };
    let (approval_tx, mut approval_rx) = mpsc::channel::<PendingApproval>(4);
    let gate = Arc::new(with_builtins(
        cfg,
        GatedDispatcher::new(
            pool.clone(),
            Arc::new(TuiApprover { tx: approval_tx }),
            cfg.permission_policy().map_err(anyhow::Error::msg)?,
        ),
        &skills,
    ));
    let tools = Arc::new(session_tools(cfg, &pool, &skills)?);
    let pool_for_probe = pool.clone();
    let depth = egaku_term::theme::ColorDepth::detect();
    let (keymap, refused) = state.apply_config(cfg, depth);
    for r in refused {
        state.toast(arnes_tui::ToastLevel::Warn, format!("config {r}"));
    }
    let mut live = Live {
        cfg: cfg.clone(),
        base: cfg.clone(),
        client: client.clone(),
        registry,
        pool,
        tools,
        gate,
        knobs,
        keymap,
        commands: blue_commands(cfg),
        hosted: arnes_blue::bidamas::Registry::default(),
        // Submit and after-turn hooks follow the file on reload; before-tool
        // hooks sit in the gate, which a reload rebuilds when they change.
        hooks: cfg.blue_hooks().0,
        notifications: cfg.notifications_mode().as_deref() != Some("off"),
        pool_tx: tokio::sync::watch::channel(pool_for_probe).0,
        registry_tx: tokio::sync::watch::channel(arnes_mcp_client::McpRegistry::new()).0,
        health_tx: tokio::sync::watch::channel(None).0,
        probe_hooks_tx: tokio::sync::watch::channel(Vec::new()).0,
        file,
        profile,
        skills,
        route_source: "config",
        reach: None,
    };
    let (reach, refused_endpoints) = routing::reach_for(cfg);
    live.reach = reach;
    for r in refused_endpoints {
        state.toast(arnes_tui::ToastLevel::Warn, r);
    }
    state.profile = live.profile.clone().unwrap_or_default();
    live.hosted = hosted;
    live.feed_health();
    let (suspect_tx, suspect_rx) = mpsc::unbounded_channel::<String>();
    let facts = arnes_vigy::Facts::default();
    mcp_health::spawn(
        mcp_health::Inputs {
            pool: live.pool_tx.subscribe(),
            registry: live.registry_tx.subscribe(),
            policy: live.health_tx.subscribe(),
            hooks: live.probe_hooks_tx.subscribe(),
            suspect: suspect_rx,
            lease: mcp_lease(cfg).await,
            log_dir: notice::mcp_log_dir(),
        },
        health_out,
        facts.clone(),
    );
    if background && !live.registry.servers.is_empty() {
        state.activity.note(format!("connecting {} MCP servers", live.registry.servers.iter().filter(|s| !s.lazy).count()), true, Instant::now());
    }
    let (keymap, bad) = hosted_keymap(cfg, &live.hosted, std::mem::replace(&mut live.keymap, Keymap::claude_code()));
    live.keymap = keymap;
    for r in bad {
        state.toast(arnes_tui::ToastLevel::Warn, format!("{r}; not bound"));
    }
    apply_hosted_colors(cfg, &live.hosted, &mut state);
    let (mcp_tx, mut mcp_rx) = mpsc::unbounded_channel::<McpSwap>();
    let mut approval_reply: Option<tokio::sync::oneshot::Sender<Verdict>> = None;
    let ledger = Arc::new(Ledger::new(cfg.ledger_path()));
    ledger.set_prices(route_prices(cfg));
    let mut ticking = start_vigies(cfg, &live.hosted, facts.clone()).await;
    let mut vigy_runs = ticking.as_ref().map(arnes_vigy::Ticking::subscribe);
    let mut recorder = record::Recorder::open(cfg.persistence().sessions, ledger.session(), &state.conversation);
    if let Some(query) = resume {
        match recorder.as_mut().map(|r| r.resume(&query).map(|c| (c, r.describe()))) {
            Some(Ok((conversation, what))) => {
                state.conversation = conversation;
                state.input.hint = Some(format!("resumed {what}"));
            }
            Some(Err(e)) => state.set_error(format!("--resume: {e}")),
            None => state.set_error("--resume: no state directory to keep sessions in".to_owned()),
        }
    }
    state.activity.route = route_label(&cfg.provedor, &cfg.model());
    probe_local(cfg);
    let (act_tx, act_rx) = std::sync::mpsc::channel::<arnes_tui::activity::Event>();
    state.effort = thinking_label(live.client.params.thinking);
    state.tools = live.tools.len();
    state.input.user_commands = picker_commands(cfg, &live.hosted, &live.commands);
    state.permission_mode = live.gate.mode().name().into();
    let mut segment_memo = SegmentMemo::default();
    let (tx, mut rx) = mpsc::channel::<TurnDone>(4);
    let (routed_tx, mut routed_rx) = mpsc::unbounded_channel::<Routed>();
    let mut turn: Option<tokio::task::JoinHandle<()>> = None;

    // Drop-safe terminal lifecycle from egaku-term, plus input protocols.
    let mut term = Terminal::enter()?;
    let mut protocols = Some(Protocols::enable()?);
    let mut title = format!("arnes · {}", cfg.model());
    emit(&egaku::vt::window_title(&title));
    // Frames are paced: at most `ui.timing.frame-rate` a second while
    // something moves, and input is polled at the idle rate when nothing does.
    let mut pacer = egaku_term::anim::FramePacer::new(live.knobs.frame_rate);
    let mut repaint_all = false;
    let mut turn_started: Option<Instant> = None;
    // Built on the first `@`, reused for the session.
    let mut mention_files: Option<Vec<String>> = None;
    let mut prompt_history = prompt_history::PromptHistory::open(cfg, &mut state.input);
    let (mut cols, mut rows) = term.size()?;
    let mut prev = Buffer::empty(cols, rows);
    let mut back = Buffer::empty(cols, rows);
    term.clear()?;
    term.flush()?;
    let mut dirty = true;

    macro_rules! kit {
        () => {
            TurnKit {
                client: &live.client,
                gate: &live.gate,
                tools: &live.tools,
                ledger: &ledger,
                tx: &tx,
                activity: &act_tx,
                rounds: live.knobs.max_rounds,
                route: live.route(),
            }
        };
    }

    loop {
        // Prompts the reducer recorded since the last pass go to disk.
        prompt_history.record(&mut state.input);
        // Notices from anywhere in arnes (and its libraries' warnings).
        while let Ok((level, text)) = toasts.try_recv() {
            state.toast(level, text);
            dirty = true;
        }
        // Each vigy tick: a failure is said once, as it happens.
        let mut ticked = false;
        while let Some(Ok(run)) = vigy_runs.as_mut().map(tokio::sync::broadcast::Receiver::try_recv) {
            if let Some(e) = run.error.as_deref().filter(|_| matches!(run.result, vigy_types::ResultStatus::Failed)) {
                state.toast(arnes_tui::ToastLevel::Warn, format!("vigy tick failed: {e}"));
            }
            ticked = true;
            dirty = true;
        }
        // An open /vigy panel follows each tick; the navigator keeps its place.
        if ticked
            && let (Some(t), Some((title, lines))) = (ticking.as_ref(), state.panel.as_mut())
            && title == VIGY_PANEL
        {
            *lines = vigy_panel(t).await;
        }
        // Keep repainting while a toast lives, so it fades and then goes.
        if state.expire_toasts(Instant::now()) || !state.toasts.is_empty() {
            dirty = true;
        }

        // Finished turns.
        while let Ok(done) = rx.try_recv() {
            turn = None;
            state.receive_conversation(done.conversation, Instant::now());
            if let Some(r) = recorder.as_mut() {
                r.save(&state.conversation);
            }
            state.set_busy(false);
            // Name the route that actually answered, not the configured one.
            if let Some(s) = live.client.served() {
                if s.fallback {
                    state.activity.note(format!("answered by {} (fallback)", s.label), false, Instant::now());
                }
                state.activity.route = route_label(if s.label == "on-device" { "local" } else { &s.label }, &s.model);
                state.model = match (s.fallback, s.model == s.label) {
                    (false, _) => s.model,
                    (true, true) => format!("{} (fallback)", s.model),
                    (true, false) => format!("{} via {} (fallback)", s.model, s.label),
                };
            }
            if live.notifications && turn_started.is_some_and(|t| t.elapsed() >= live.knobs.notify_after) {
                let body = if done.result.is_ok() { "turn finished" } else { "turn failed" };
                notify("arnes", body);
            }
            if let Err(e) = done.result {
                state.activity.note("the turn failed; see the notice", false, Instant::now());
                state.set_error(e);
            }
            if live.hooks.any(arnes_blue::HookEvent::AfterTurn) {
                for r in live.hooks.after_turn(&conversation_text(&state.conversation)) {
                    match r {
                        Ok(out) => out.effects.into_iter().for_each(|e| apply_blue_effect(&mut state, e)),
                        Err(e) => state.toast(arnes_tui::ToastLevel::Error, e.to_string()),
                    }
                }
            }
            let queued = state.input.take_queued().and_then(|next| {
                match live.hooks.before_submit(&next, &conversation_text(&state.conversation)) {
                    Ok((next, effects)) => {
                        for e in effects {
                            apply_blue_effect(&mut state, e);
                        }
                        Some(next)
                    }
                    Err(e) => {
                        state.toast(arnes_tui::ToastLevel::Error, format!("{e}; queued prompt not sent"));
                        state.input.set_text(&next);
                        None
                    }
                }
            });
            if let Some(next) = queued.and_then(|next| hosted_before_submit(&mut state, &live, next)) {
                state.conversation.push(Message::user_text(next.clone()));
                turn_started = Some(Instant::now());
                turn = Some(begin_turn(&mut state, &live, &kit!(), &next, &routed_tx));
            }
            dirty = true;
        }

        while let Ok(r) = routed_rx.try_recv() {
            let outcome = apply_routed(&mut live, &mut state, r.answer);
            routing::log(&live.cfg, &r.facts, &outcome);
            turn = Some(start_turn(&mut state, &kit!()));
            dirty = true;
        }

        // The config file changed on disk (edit, or a rebuild re-rendering it).
        while let Ok(loaded) = reload_rx.try_recv() {
            match loaded {
                Err(e) => {
                    state.input.hint = None;
                    state.set_error(format!("config reload refused: {e} (running config kept)"));
                }
                Ok(loaded) => {
                    live.file = loaded.config.clone();
                    let loaded = live.profiled(loaded, &mut state);
                    let fx = live.reload(loaded, &mut state, depth, &mcp_tx);
                    ledger.set_prices(route_prices(&live.cfg));
                    segment_memo = SegmentMemo::default();
                    if fx.pacer {
                        pacer = egaku_term::anim::FramePacer::new(live.knobs.frame_rate);
                    }
                    if fx.mentions {
                        mention_files = None;
                    }
                    prompt_history.reconfigure(&live.cfg, &mut state.input);
                    match fx.sessions {
                        Some(false) => recorder = None,
                        Some(true) if recorder.is_none() => {
                            recorder = record::Recorder::open(true, ledger.session(), &state.conversation);
                        }
                        _ => {}
                    }
                    if fx.sweep {
                        sweep(&live.cfg);
                    }
                    if fx.vigies {
                        // Redeclared on the running runtime; the first vigy
                        // declared starts it, and none left disables them all.
                        match ticking.as_ref() {
                            Some(t) => match t.redeclare(all_vigies(&live.cfg, &live.hosted)).await {
                                Ok((next, refused)) => {
                                    for r in refused {
                                        state.toast(arnes_tui::ToastLevel::Warn, format!("{r}; skipped"));
                                    }
                                    vigy_runs = Some(next.subscribe());
                                    ticking = Some(next);
                                }
                                Err(e) => state.toast(arnes_tui::ToastLevel::Warn, format!("vigies kept as they were: {e}")),
                            },
                            None => {
                                ticking = start_vigies(&live.cfg, &live.hosted, facts.clone()).await;
                                vigy_runs = ticking.as_ref().map(arnes_vigy::Ticking::subscribe);
                            }
                        }
                        state.activity.note("vigies redeclared", true, Instant::now());
                    }
                }
            }
            dirty = true;
        }

        // Servers arriving, reconnecting, changing health.
        while let Ok(msg) = health_rx.try_recv() {
            match msg {
                mcp_health::Msg::Arrived(a) => live.arrived(a, &mut state),
                mcp_health::Msg::Notice(warn, text) => {
                    let level = if warn { arnes_tui::ToastLevel::Warn } else { arnes_tui::ToastLevel::Info };
                    state.toast(level, text);
                }
                mcp_health::Msg::Probed => {
                    state.segments = mcp_segment(&live.cfg, &live.hosted, Some(&facts)).into_iter().collect();
                }
            }
            dirty = true;
        }

        // Changed MCP servers finished connecting.
        while let Ok(swap) = mcp_rx.try_recv() {
            live.mcp_swapped(swap, &mut state);
            dirty = true;
        }

        // A tool wants approval: show the dialog; the turn waits on the reply.
        while let Ok(PendingApproval { request, reply }) = approval_rx.try_recv() {
            state.approval = Some(ApprovalView {
                tool: request.tool,
                input: serde_json::to_string(&request.input).unwrap_or_default(),
                selected: 0,
                warning: request.warning,
            });
            approval_reply = Some(reply);
            if live.notifications {
                let tool = state.approval.as_ref().map_or("", |a| a.tool.as_str());
                notify("arnes needs approval", tool);
            } else {
                notify_attention();
            }
            dirty = true;
        }

        let now = Instant::now();
        while let Ok(ev) = act_rx.try_recv() {
            // A failed MCP call is the best probe there is: the scheduler
            // checks that server now.
            if let arnes_tui::activity::Event::ToolEnd { name, ok: false } = &ev
                && let Some((server, _)) = arnes_mcp_client::split_name(name)
            {
                let _ = suspect_tx.send(server.to_owned());
            }
            state.activity_event(ev, now);
        }
        state.observe(now);
        let animating = state.animating(now);
        let wait = pacer.timeout(now, animating || dirty, live.knobs.idle_poll);
        if event::poll(wait)? {
            let mut effects = Vec::new();
            match event::read()? {
                Event::Key(ke) if ke.kind != KeyEventKind::Release => {
                    // The overlay in front (approval, /mcp, help, a panel,
                    // the scrollback) reads the key first, through the one
                    // navigation grammar; the prompt gets it otherwise.
                    state.viewport = (cols, rows);
                    let asking = state.approval.as_ref().map(|a| a.tool.clone());
                    match state.overlay_key(&ke, &live.keymap) {
                        Overlay::Approval(choice) => {
                            let verdict = approval_verdict(choice);
                            let tool = asking.unwrap_or_default();
                            state.answered(&tool, !matches!(verdict, Verdict::Deny(_)), Instant::now());
                            state.approval = None;
                            if let Some(reply) = approval_reply.take() {
                                let _ = reply.send(verdict);
                            }
                        }
                        Overlay::Consumed => {}
                        Overlay::None | Overlay::Pass => effects = handle_key(&mut state, &mut live.keymap, ke),
                    }
                    if let Some(text) = state.clipboard.take() {
                        emit(&egaku::vt::osc52_copy(&text));
                        state.input.hint = Some("copied".into());
                    }
                }
                Event::Paste(text) => state.input.paste(&text),
                Event::Resize(c, r) => {
                    // No clear: the next frame repaints every cell inside
                    // one synchronized update, so a resize never flashes.
                    cols = c;
                    rows = r;
                    prev.resize(cols, rows);
                    back.resize(cols, rows);
                    repaint_all = true;
                }
                _ => {}
            }
            dirty = true;
            for effect in effects {
                match effect {
                    Effect::Exit => {
                        if let Some(t) = turn.take() {
                            t.abort();
                        }
                        return Ok(());
                    }
                    Effect::Submit(prompt) => {
                        // blue `before-submit` hooks may rewrite the prompt. One
                        // that fails sends nothing and hands the text back.
                        let prompt = match live.hooks.before_submit(&prompt, &conversation_text(&state.conversation)) {
                            Ok((prompt, effects)) => {
                                for e in effects {
                                    apply_blue_effect(&mut state, e);
                                }
                                prompt
                            }
                            Err(e) => {
                                state.toast(arnes_tui::ToastLevel::Error, format!("{e}; not sent"));
                                state.input.set_text(&prompt);
                                continue;
                            }
                        };
                        let Some(prompt) = hosted_before_submit(&mut state, &live, prompt) else {
                            continue;
                        };
                        state.conversation.push(Message::user_text(prompt.clone()));
                        turn_started = Some(Instant::now());
                        turn = Some(begin_turn(&mut state, &live, &kit!(), &prompt, &routed_tx));
                    }
                    Effect::Interrupt => {
                        if let Some(t) = turn.take() {
                            t.abort();
                            // Keep the transcript valid for the next turn: the
                            // user's prompt stays, answered by a marker.
                            state
                                .conversation
                                .push(Message::assistant_text("[interrupted by user]"));
                            state.set_busy(false);
                        }
                    }
                    Effect::Redraw => {
                        term.clear()?;
                        term.flush()?;
                        prev = Buffer::empty(cols, rows);
                    }
                    Effect::OpenEditor => {
                        // Hand the real terminal to the editor, then take it back.
                        drop(protocols.take());
                        drop(term);
                        let edited = edit_externally(&state.input.editor.text());
                        term = Terminal::enter()?;
                        protocols = Some(Protocols::enable()?);
                        term.clear()?;
                        term.flush()?;
                        prev = Buffer::empty(cols, rows);
                        match edited {
                            Ok(text) => state.input.set_text(&text),
                            Err(e) => state.set_error(format!("editor: {e}")),
                        }
                    }
                    Effect::ToggleTranscript => state.show_transcript = !state.show_transcript,
                    Effect::Scrollback => {
                        state.viewport = (cols, rows);
                        if !state.enter_scrollback() {
                            state.input.hint = Some("nothing to scroll".into());
                        }
                    }
                    Effect::OpenMentionPicker => {
                        let max = live.knobs.mention_max_files;
                        let files = mention_files.get_or_insert_with(|| mention_index(max)).clone();
                        state.input.open_mention_picker(files);
                    }
                    Effect::Command { name, args } => {
                    // A user declaration, then the hosted bidama, then the
                    // kernel (docs/kernel.md §2). A module's arnes_clear() and
                    // arnes_exit() run the very same kernel arms below.
                    let mut kernel_names: Vec<(String, String)> = Vec::new();
                    match serve(&name, &live.commands, &live.hosted, &live.cfg) {
                        Serve::User(decl) => kernel_names.extend(run_blue_command(&mut state, decl, &args, module_budget(&live.cfg))),
                        Serve::Module => kernel_names.extend(run_std_command(&mut state, &live.hosted, &name, &args, &live.keymap, &recent_sessions(recorder.as_ref()), module_budget(&live.cfg), &live.cfg.read_roots())),
                        Serve::Off => state.input.hint = Some(format!("/{name} is off (:modules-off)")),
                        Serve::Kernel => kernel_names.push((name.clone(), args.clone())),
                    }
                    for (name, args) in kernel_names { match name.as_str() {
                        "exit" => {
                            if let Some(t) = turn.take() {
                                t.abort();
                            }
                            return Ok(());
                        }
                        "help" => state.input.show_help = true,
                        "clear" => {
                            if let Some(t) = turn.take() {
                                t.abort();
                            }
                            state.conversation = new_conversation(&live.cfg);
                            state.conversation.system = system_prompt(&live.cfg, &live.skills).0;
                            if let Some(r) = recorder.as_mut() {
                                r.restart(&state.conversation);
                            }
                            state.set_busy(false);
                            state.last_error = None;
                            state.input.hint = Some("new conversation".into());
                        }
                        "vigy" => vigy_command(&mut state, ticking.as_ref(), &args).await,
                        "resume" => match (recorder.as_mut(), args.trim()) {
                            (None, _) => state.set_error("/resume: no state directory to keep sessions in".to_owned()),
                            (Some(r), "") => {
                                for line in r.recent(5) {
                                    state.toast(arnes_tui::ToastLevel::Info, line);
                                }
                                state.input.hint = Some("/resume <n> or /resume <id prefix>".into());
                            }
                            (Some(r), query) => match r.resume(query) {
                                Ok(conversation) => {
                                    if let Some(t) = turn.take() {
                                        t.abort();
                                    }
                                    state.conversation = conversation;
                                    state.set_busy(false);
                                    state.last_error = None;
                                    state.input.hint = Some(format!("resumed {}", r.describe()));
                                }
                                Err(e) => state.set_error(format!("/resume: {e}")),
                            },
                        },
                        "vim" => {
                            if state.input.vim.is_some() {
                                state.input.vim = None;
                                state.input.hint = Some("vim editing off".into());
                            } else {
                                state.input.enable_vim();
                            }
                        }
                        "mode" => match arnes_loop::permission::PermissionMode::parse(args.trim()) {
                            Ok(m) => {
                                live.gate.set_mode(m);
                                state.permission_mode = m.name().into();
                            }
                            Err(e) => state.set_error(e),
                        },
                        "mcp" => {
                            let health = match ticking.as_ref() {
                                Some(t) => t.health().await,
                                None => None,
                            };
                            state.mcp = Some(mcp_browser(&live.cfg, &live.pool, health.as_ref()).with_keymap(&state.nav_menu));
                        }
                        "notices" => state.panel = Some(("notices".into(), state.notice_lines())),
                        "profile" if args.trim().is_empty() => {
                            let mut choices = live.file.profile_choices();
                            choices.push(("-".into(), "no profile: the file alone".into()));
                            let choices = choices.into_iter().map(|(n, d)| {
                                let mark = if live.profile.as_deref() == Some(n.as_str()) { "\u{2022}" } else { " " };
                                (n.clone(), format!("{mark} {n:<16} {d}"))
                            });
                            state.input.open_choice_picker("profile", choices.collect());
                        }
                        "profile" => live.switch_profile(&args, &mut state, &reapply),
                        "model" => {
                            let pick = args.trim();
                            let catalog = model_catalog(&live.base, live.knobs.catalog_max_age).await;
                            if pick.is_empty() {
                                state.input.open_model_picker(model_choices(&live.base, &live.cfg, catalog.as_ref()));
                            } else {
                                // The conversation moves with the switch: same
                                // messages, same prompt, the next turn on the new route.
                                match switch_model(&live.base, &live.cfg, pick, catalog.as_ref()) {
                                    Ok(next) => match live.switch_route(&next, &mut state) {
                                        Ok(refused) => {
                                            state.conversation.system = system_prompt(&next, &live.skills).0;
                                            state.input.hint = Some(format!("now talking to {} on {}", next.model(), next.provedor));
                                            if !refused.is_empty() {
                                                state.set_error(format!("route skipped: {}", refused.join("; ")));
                                            }
                                            live.cfg = next;
                                            live.route_source = "manual";
                                            probe_local(&live.cfg);
                                        }
                                        Err(e) => state.set_error(format!("/model: {e}")),
                                    },
                                    Err(e) => state.set_error(format!("/model: {e}")),
                                }
                            }
                        }
                        other => state.set_error(format!("unknown command /{other}")),
                    } }
                    }
                    Effect::CycleMode => {
                        let next = live.gate.mode().next();
                        live.gate.set_mode(next);
                        state.permission_mode = next.name().into();
                    }
                    Effect::CycleEffort => {
                        live.client.params.thinking = next_thinking(live.client.params.thinking);
                        state.effort = thinking_label(live.client.params.thinking);
                    }
                }
            }
        }

        let t = ledger.totals();
        let usage = arnes_tui::UsageView {
            calls: t.calls,
            prompt_tokens: u64::from(t.usage.input_tokens)
                + u64::from(t.usage.cache_read_input_tokens.unwrap_or(0))
                + u64::from(t.usage.cache_creation_input_tokens.unwrap_or(0)),
            output_tokens: u64::from(t.usage.output_tokens),
            cache_hit_ratio: t.cache_hit_ratio(),
            cost_usd: t.cost_usd,
            priced: t.all_priced,
        };
        if segment_memo.refresh(&mut state, &live.cfg, &live.hosted) {
            dirty = true;
        }
        if usage != state.usage {
            state.usage = usage;
            dirty = true;
        }

        // Repaint only when something changed; while busy, keep ticking so
        // the status line stays live.
        let now = Instant::now();
        if (dirty || state.animating(now)) && pacer.due(now) {
            back.reset();
            arnes_tui::draw_frame_at(&mut back, &state, &live.keymap, now);
            arnes_tui::look::project(&mut back, depth);
            if repaint_all {
                // A front buffer no frame can equal: every cell is re-sent.
                for y in 0..rows {
                    prev.hline(0, y, cols, '\u{1}', egaku_term::Style::default());
                }
                repaint_all = false;
            }
            let sync = term.sync_output();
            render_diff(term.out(), &prev, &back, sync)?;
            std::mem::swap(&mut prev, &mut back);
            pacer.mark(now);
            dirty = false;
            // The window title follows the session: working, waiting on
            // an approval, or idle.
            let next = if state.approval.is_some() {
                format!("arnes · approval needed · {}", state.model)
            } else if state.busy {
                format!("arnes · working · {}", state.model)
            } else {
                format!("arnes · {}", state.model)
            };
            if next != title {
                emit(&egaku::vt::window_title(&next));
                title = next;
            }
        }
    }
}

/// A question from the gate, waiting on the dialog.
struct PendingApproval {
    request: ApprovalRequest,
    reply: tokio::sync::oneshot::Sender<Verdict>,
}

/// The TUI's approver: hands the question to the UI loop and waits.
struct TuiApprover {
    tx: mpsc::Sender<PendingApproval>,
}

#[async_trait::async_trait]
impl Approver for TuiApprover {
    async fn ask(&self, request: ApprovalRequest) -> Verdict {
        let (reply, answer) = tokio::sync::oneshot::channel();
        if self.tx.send(PendingApproval { request, reply }).await.is_err() {
            return Verdict::Deny("the terminal UI is gone".into());
        }
        answer.await.unwrap_or_else(|_| Verdict::Deny("the question was dismissed".into()))
    }
}

/// The approval question's option → the gate's verdict.
fn approval_verdict(choice: usize) -> Verdict {
    match choice {
        0 => Verdict::Allow,
        1 => Verdict::AllowAlways,
        _ => Verdict::Deny("no — the user chose not to run it".into()),
    }
}

/// Ring the terminal bell so a waiting approval is noticed (OSC 9 desktop
/// notifications arrive with the notification integration).
/// Attention without a notification: the bell (the approval dialog's
/// fallback when notifications are off).
fn notify_attention() {
    emit(egaku::vt::BELL);
}

/// A desktop notification in every terminal dialect at once (`egaku::vt` —
/// the builders mado also parses), plus the bell.
fn notify(title: &str, body: &str) {
    let mut bytes = egaku::vt::notify_all(title, body);
    bytes.extend_from_slice(egaku::vt::BELL);
    emit(&bytes);
}

fn emit(bytes: &[u8]) {
    let mut out = std::io::stdout();
    let _ = out.write_all(bytes);
    let _ = out.flush();
}


/// The tools sent every turn: the built-ins (a fixed, sorted block) then
/// the MCP exposure — computed once, so the request prefix never drifts.
fn session_tools(cfg: &ArnesConfig, pool: &McpPool, skills: &arnes_tools::skills::Skills) -> Result<Vec<arnes_core::Tool>> {
    let mut tools = if cfg.builtin_tools.unwrap_or(true) { arnes_tools::Builtins::tools_except(&cfg.modules_off) } else { Vec::new() };
    if !skills.is_empty() {
        tools.push(arnes_tools::skills::Skills::tool());
        tools.sort_by(|a, b| a.name.cmp(&b.name));
    }
    tools.extend(blue_tools(cfg).tools());
    tools.extend(pool.exposed_tools(cfg.mcp_exposure().map_err(anyhow::Error::msg)?));
    Ok(tools)
}

/// The config's blue tools and the refusals, one per tool that does not
/// resolve. The built-ins' names are reserved whether or not `builtin_tools`
/// is on, so turning them on later cannot change what a name means; a
/// built-in named in `:modules-off` frees its name, so a blue tool can
/// replace it.
fn blue_tools_checked(cfg: &ArnesConfig) -> (arnes_blue::Tools, Vec<String>) {
    let reserved: Vec<String> = arnes_tools::Builtins::tools_except(&cfg.modules_off).into_iter().map(|t| t.name).collect();
    let reserved: Vec<&str> = reserved.iter().map(String::as_str).collect();
    cfg.blue_tools(&reserved)
}

/// The blue tools that resolve (refusals are reported once, by
/// [`report_blue_refusals`]).
fn blue_tools(cfg: &ArnesConfig) -> arnes_blue::Tools {
    blue_tools_checked(cfg).0
}

/// The config's blue commands and the refusals, one per command that
/// shadows a built-in or an earlier declaration.
fn blue_commands_checked(cfg: &ArnesConfig) -> (Vec<arnes_config::CommandDecl>, Vec<String>) {
    // A module command may be replaced by name; a kernel command may not.
    let reserved: Vec<&str> =
        arnes_tui::COMMANDS.iter().map(|(n, _)| *n).filter(|n| !arnes_blue::bidamas::KERNEL_TWINS.contains(n)).collect();
    cfg.blue_commands(&reserved)
}

fn blue_commands(cfg: &ArnesConfig) -> Vec<arnes_config::CommandDecl> {
    blue_commands_checked(cfg).0
}

/// Every refused blue declaration, as a warning toast each.
fn report_blue_refusals(cfg: &ArnesConfig) {
    let refused = blue_tools_checked(cfg)
        .1
        .into_iter()
        .chain(blue_commands_checked(cfg).1)
        .chain(cfg.blue_hooks().1)
        .chain(cfg.blue_vigies().1);
    for r in refused {
        tracing::warn!("{r}; skipped");
    }
}

/// The vigy runtime for host `arnes`, when any vigy is declared, and the
/// task that probes the MCP servers on the vigies' cadence so each tick
/// reads fresh facts without probing itself.
async fn start_vigies(
    cfg: &ArnesConfig,
    hosted: &arnes_blue::bidamas::Registry,
    facts: arnes_vigy::Facts,
) -> Option<arnes_vigy::Ticking> {
    let decls = all_vigies(cfg, hosted);
    if decls.is_empty() {
        return None;
    }
    let Some(path) = arnes_vigy::store_path() else {
        tracing::warn!("vigies declared but there is no vigy store path; not ticking");
        return None;
    };
    let (ticking, refused) = match arnes_vigy::Ticking::open(&path, decls, facts).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("vigy store {}: {e}; not ticking", path.display());
            return None;
        }
    };
    for r in refused {
        tracing::warn!("{r}; skipped");
    }
    // The facts are filled by the health scheduler (`mcp_health`), not here.
    Some(ticking)
}

/// `/vigy` lists each declared vigy and its last result; `/vigy tick <name>`
/// ticks one now.
const VIGY_PANEL: &str = "vigy";

/// The `/vigy` panel's lines: each vigy's last result, then each server's
/// health as the store recorded it.
async fn vigy_panel(t: &arnes_vigy::Ticking) -> Vec<String> {
    arnes_vigy::panel_lines(&t.status().await, t.health().await.as_ref())
}

async fn vigy_command(state: &mut TuiState, ticking: Option<&arnes_vigy::Ticking>, args: &str) {
    let Some(t) = ticking else {
        state.input.hint = Some("no vigies declared (:vigies in the config)".into());
        return;
    };
    match args.split_whitespace().collect::<Vec<_>>().as_slice() {
        [] => state.panel = Some((VIGY_PANEL.into(), vigy_panel(t).await)),
        ["tick", name] => match t.tick(name).await {
            Ok(run) => {
                let st = arnes_vigy::Status { name: (*name).to_owned(), every: std::time::Duration::ZERO, last: Some(run) };
                state.toast(arnes_tui::ToastLevel::Info, format!("{name}: {}", st.text()));
            }
            Err(e) => state.set_error(format!("/vigy: {e}")),
        },
        _ => state.set_error("/vigy, or /vigy tick <name>".to_owned()),
    }
}

/// The conversation as blue's `arnes_conversation()` sees it.
fn conversation_text(conv: &Conversation) -> String {
    conv.messages
        .iter()
        .map(|m| {
            let role = match m.role {
                arnes_core::Role::User => "user",
                arnes_core::Role::Assistant => "assistant",
                arnes_core::Role::System => "system",
            };
            format!("{role}: {}", m.text())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Run a blue slash command and apply what it asked for. A result that fits
/// the status line is a hint; a longer one is a toast. A failure is an error
/// toast naming the command and the stage.
fn run_blue_command(state: &mut TuiState, decl: &arnes_config::CommandDecl, args: &str, budget: Duration) -> Vec<(String, String)> {
    let name = format!("/{}", decl.name);
    let (ext, script, args, conversation) = (name.clone(), decl.script(), args.to_owned(), conversation_text(&state.conversation));
    let outcome = arnes_blue::within_budget(&name, budget, move || arnes_blue::invoke(&ext, &script, &[&args], &conversation));
    apply_command_outcome(state, &name, outcome)
}

/// Run a hosted bidama command; see [`apply_command_outcome`].
#[allow(clippy::too_many_arguments)] // each is one read the command's context captures
fn run_std_command(
    state: &mut TuiState,
    hosted: &arnes_blue::bidamas::Registry,
    name: &str,
    args: &str,
    keymap: &Keymap,
    sessions: &str,
    budget: Duration,
    read_roots: &[std::path::PathBuf],
) -> Vec<(String, String)> {
    let sessions = sessions.to_owned();
    let read_roots = read_roots.to_vec();
    let conversation = conversation_text(&state.conversation);
    let bindings = arnes_tui::render::help_lines(keymap, state.nav.keymap()).join("\n");
    let notices = state.notice_lines().join("\n");
    let snapshot = state_snapshot(state);
    let (cmd, args, hosted) = (name.to_owned(), args.to_owned(), hosted.clone());
    let outcome = arnes_blue::within_budget(&format!("/{name}"), budget, move || {
        let ctx = arnes_blue::Context {
            conversation: &conversation,
            bindings: &bindings,
            notices: &notices,
            state: &snapshot,
            sessions: &sessions,
            read_roots: &read_roots,
            reach: None,
        };
        hosted.command(&arnes_blue::bidamas::BidamaPath::from_env(), &cmd, &args, ctx)
    });
    apply_command_outcome(state, &format!("/{name}"), outcome)
}

/// The configured budget for one blue call from the prompt.
fn module_budget(cfg: &ArnesConfig) -> Duration {
    cfg.module_budget_ms.map_or(arnes_blue::DEFAULT_BUDGET, Duration::from_millis)
}

/// Apply a command's effects and show its result. A result that fits the
/// status line is a hint; a longer one is a toast; a failure is an error
/// toast naming the command and the stage. Returns the kernel commands the
/// program asked for (`arnes_clear()`, `arnes_exit()`), in order, for the
/// caller to run through the kernel's own arms.
fn apply_command_outcome(
    state: &mut TuiState,
    name: &str,
    outcome: Result<arnes_blue::Outcome, arnes_blue::ExtError>,
) -> Vec<(String, String)> {
    let mut kernel = Vec::new();
    match outcome {
        Ok(outcome) => {
            let text = outcome.text().to_owned();
            for e in outcome.effects {
                match e {
                    arnes_blue::Effect::Arm { arm, args } => kernel.push((arm, args)),
                    arnes_blue::Effect::Set { key, value } => match setting_arm(state, &key, &value) {
                        Ok(Some(arm)) => kernel.push(arm),
                        Ok(None) => {}
                        Err(e) => state.set_error(format!("{name}: {e}")),
                    },
                    other => apply_blue_effect(state, other),
                }
            }
            if text.contains('\n') || text.chars().count() > 80 {
                state.toast(arnes_tui::ToastLevel::Info, format!("{name}: {text}"));
            } else if !text.is_empty() {
                state.input.hint = Some(text);
            }
        }
        Err(e) => state.toast(arnes_tui::ToastLevel::Error, e.to_string()),
    }
    kernel
}

/// Hosted theme colours (a theme bidama's `color` rows) over the fleet
/// palette `:tema` chose; each bad row is a warning toast.
fn apply_hosted_colors(cfg: &ArnesConfig, hosted: &arnes_blue::bidamas::Registry, state: &mut TuiState) {
    let (palette, refused) = arnes_tui::look::with_colors(state.palette, &hosted.colors(&cfg.modules_off));
    state.palette = palette;
    for r in refused {
        state.toast(arnes_tui::ToastLevel::Warn, format!("{r}; colour kept"));
    }
}

/// The keymap the hosted bindings define (a keymap bidama, `kenban` by
/// default), with `keys.prompt` edits on top. With no hosted bindings (the
/// bidama is absent or in `:modules-off`) the kernel's own keymap stands.
fn hosted_keymap(cfg: &ArnesConfig, hosted: &arnes_blue::bidamas::Registry, kernel: Keymap) -> (Keymap, Vec<String>) {
    let binds = hosted.binds(&cfg.modules_off);
    if binds.is_empty() {
        return (kernel, Vec::new());
    }
    let (base, mut refused) = Keymap::from_named(&binds);
    let (keymap, edits) = base.with_edits(&cfg.keys.prompt);
    refused.extend(edits);
    (keymap, refused)
}

/// The `mcp <up>/<all>` segment: a hosted bidama's (`kenkou`) when one
/// serves it, else the kernel's twin; nothing before the first probe.
fn mcp_segment(cfg: &ArnesConfig, hosted: &arnes_blue::bidamas::Registry, facts: Option<&arnes_vigy::Facts>) -> Option<String> {
    let facts = facts?;
    if hosted.get(arnes_blue::bidamas::Kind::Segment, "mcp").is_some() {
        return std_segment(cfg, hosted, "mcp", &facts.json()).filter(|s| !s.is_empty());
    }
    arnes_vigy::segment(&facts.get())
}

/// The declared vigies, then the hosted bidamas' vigies (`kenkou`'s
/// `mcp-health` when it is hosted), a declared one replacing a hosted one of
/// its name.
fn all_vigies(cfg: &ArnesConfig, hosted: &arnes_blue::bidamas::Registry) -> Vec<arnes_vigy::Decl> {
    let mut decls = cfg.blue_vigies().0;
    let (vs, refused) = hosted.vigies(&cfg.modules_off);
    for r in refused {
        tracing::warn!("{r}; not ticked");
    }
    for (name, script, every) in vs {
        if !decls.iter().any(|d| d.name == name) {
            decls.push(arnes_vigy::Decl { name, script, every });
        }
    }
    decls
}

/// The hosted `before-submit` hooks (`sanshou`'s `@path` attachments by
/// default), each under the module budget, after the declared ones. With none
/// hosted, the kernel's own mention expansion runs. `None`: a hook failed; the
/// text goes back to the prompt and nothing is sent.
fn hosted_before_submit(state: &mut TuiState, live: &Live, prompt: String) -> Option<String> {
    let hooks = live.hosted.hooks("before-submit", &live.cfg.modules_off);
    if hooks.is_empty() {
        let (prompt, attached) = expand_mentions(&prompt, live.knobs.mention_max_bytes, &|p| live.cfg.read_allowed(p));
        if attached > 0 {
            state.input.hint = Some(format!("attached {attached} file(s)"));
        }
        return Some(prompt);
    }
    let mut snapshot = state_snapshot(state);
    snapshot.push(("mention-max-bytes".into(), live.knobs.mention_max_bytes.to_string()));
    let mut prompt = prompt;
    for script in hooks {
        let (s, p, snap, roots) = (script.clone(), prompt.clone(), snapshot.clone(), live.cfg.read_roots());
        let name = format!("hook before-submit {}", script.function);
        let ext = name.clone();
        let out = arnes_blue::within_budget(&name, module_budget(&live.cfg), move || {
            arnes_blue::invoke_with(&ext, &s, &[&p], arnes_blue::Context { state: &snap, read_roots: &roots, ..arnes_blue::Context::default() })
        });
        match out {
            Ok(o) => {
                if let arnes_blue::Returned::Text(t) = &o.returned {
                    prompt.clone_from(t);
                }
                for e in o.effects {
                    apply_blue_effect(state, e);
                }
            }
            Err(e) => {
                state.toast(arnes_tui::ToastLevel::Error, format!("{e}; not sent"));
                state.input.set_text(&prompt);
                return None;
            }
        }
    }
    Some(prompt)
}

/// What `arnes_sessions()` reads: the recent sessions, one per line, or
/// empty when no state directory keeps them.
fn recent_sessions(recorder: Option<&record::Recorder>) -> String {
    recorder.map(|r| r.recent(5).join("\n")).unwrap_or_default()
}

/// What bidamas read through `arnes_state(key)`.
fn state_snapshot(state: &TuiState) -> Vec<(String, String)> {
    vec![
        ("model".into(), state.model.clone()),
        ("effort".into(), state.effort.clone()),
        ("permission-mode".into(), state.permission_mode.clone()),
        ("tools".into(), state.tools.to_string()),
        ("vim".into(), if state.input.vim.is_some() { "on" } else { "off" }.into()),
        ("busy".into(), if state.busy { "yes" } else { "no" }.into()),
        ("route".into(), state.activity.route.clone()),
    ]
}

/// The kernel arm an `arnes_set(key, value)` runs, validated here so a bad
/// value is a named error and never reaches the arm. `Ok(None)`: already so.
fn setting_arm(state: &TuiState, key: &str, value: &str) -> Result<Option<(String, String)>, String> {
    match key {
        "vim" => match value {
            "on" | "off" if (value == "on") == state.input.vim.is_some() => Ok(None),
            "on" | "off" => Ok(Some(("vim".into(), String::new()))),
            other => Err(format!("vim takes on | off, not {other:?}")),
        },
        "permission-mode" => arnes_loop::permission::PermissionMode::parse(value.trim())
            .map(|_| Some(("mode".into(), value.trim().to_owned()))),
        other => Err(format!("no setting {other:?}")),
    }
}

/// The bidamas to host: arnes's own, then `:bidamas`, each once.
fn hosted_names(cfg: &ArnesConfig) -> Vec<String> {
    let mut names: Vec<String> = arnes_blue::bidamas::DEFAULT_HOSTED.iter().map(|s| (*s).to_owned()).collect();
    for n in &cfg.bidamas {
        if !names.contains(n) {
            names.push(n.clone());
        }
    }
    names
}

/// Ask every hosted bidama for its exports; each refusal is a warning toast.
fn host_bidamas(cfg: &ArnesConfig, state: &mut TuiState) -> arnes_blue::bidamas::Registry {
    let (reg, refused) = arnes_blue::bidamas::Registry::load(&arnes_blue::bidamas::BidamaPath::from_env(), &hosted_names(cfg));
    for r in refused {
        tracing::warn!("{r}; not hosted");
        state.toast(arnes_tui::ToastLevel::Warn, format!("{r}; not hosted"));
    }
    reg
}

/// The `/` picker's extension rows: hosted commands not configured off, then
/// user commands, a user command replacing a hosted one of its name.
fn picker_commands(
    cfg: &ArnesConfig,
    hosted: &arnes_blue::bidamas::Registry,
    user: &[arnes_config::CommandDecl],
) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = hosted
        .commands()
        .into_iter()
        .filter(|(n, _)| !user.iter().any(|u| &u.name == n) && serve(n, &[], hosted, cfg) != Serve::Off)
        .collect();
    rows.extend(user.iter().map(|c| (c.name.clone(), c.description.clone())));
    rows
}

/// Who serves a slash command (`docs/kernel.md` §2).
#[derive(Debug, PartialEq)]
enum Serve<'a> {
    User(&'a arnes_config::CommandDecl),
    Module,
    Kernel,
    Off,
}

fn serve<'a>(
    name: &str,
    user: &'a [arnes_config::CommandDecl],
    hosted: &arnes_blue::bidamas::Registry,
    cfg: &ArnesConfig,
) -> Serve<'a> {
    if let Some(decl) = user.iter().find(|c| c.name == name) {
        return Serve::User(decl);
    }
    if !hosted.owns_command(name) {
        return Serve::Kernel;
    }
    if !cfg.modules_off.iter().any(|m| m == name) {
        Serve::Module
    } else if cfg.kernel_fallback.unwrap_or(false) {
        Serve::Kernel
    } else {
        Serve::Off
    }
}

/// The last argument each hosted segment was drawn for, so a segment runs
/// only when what it shows changed, never per frame.
#[derive(Default)]
struct SegmentMemo {
    last: std::collections::HashMap<&'static str, String>,
}

impl SegmentMemo {
    /// Redraw each hosted segment whose argument changed. True when any did.
    fn refresh(&mut self, state: &mut TuiState, cfg: &ArnesConfig, hosted: &arnes_blue::bidamas::Registry) -> bool {
        let args: [(&'static str, String); 6] = [
            ("model", state.model.clone()),
            ("effort", state.effort.clone()),
            ("tools", state.tools.to_string()),
            ("permission-mode", state.permission_mode.clone()),
            ("idle", if state.activity.route.is_empty() { state.model.clone() } else { state.activity.route.clone() }),
            ("profile", state.profile.clone()),
        ];
        let mut changed = false;
        for (name, arg) in args {
            if self.last.get(name) == Some(&arg) {
                continue;
            }
            match std_segment(cfg, hosted, name, &arg) {
                Some(text) => state.hosted_segments.insert(name.to_owned(), text),
                None => state.hosted_segments.remove(name),
            };
            self.last.insert(name, arg);
            changed = true;
        }
        changed
    }
}

/// A hosted bidama status segment's text: `None` draws the kernel's own twin
/// (the module is off and `:kernel-fallback` is on), `Some("")` hides it.
fn std_segment(cfg: &ArnesConfig, hosted: &arnes_blue::bidamas::Registry, segment: &str, arg: &str) -> Option<String> {
    if cfg.modules_off.iter().any(|m| m == segment) {
        return if cfg.kernel_fallback.unwrap_or(false) { None } else { Some(String::new()) };
    }
    let (reg, name, arg) = (hosted.clone(), segment.to_owned(), arg.to_owned());
    let outcome = arnes_blue::within_budget(&format!("segment {segment}"), module_budget(cfg), move || {
        reg.segment(&arnes_blue::bidamas::BidamaPath::from_env(), &name, &arg)
            .map(|text| arnes_blue::Outcome { returned: arnes_blue::Returned::Text(text), effects: Vec::new() })
    });
    match outcome.map(|o| o.text().to_owned()) {
        Ok(text) => Some(text),
        Err(e) => {
            tracing::warn!("{e}; drawing the kernel segment");
            None
        }
    }
}

fn apply_blue_effect(state: &mut TuiState, effect: arnes_blue::Effect) {
    use arnes_blue::{Effect, Level};
    use arnes_tui::ToastLevel;
    match effect {
        Effect::Arm { arm, .. } => {
            state.toast(ToastLevel::Warn, format!("{arm}: a hook cannot run a command's kernel arm"));
        }
        Effect::Set { .. } => {
            state.toast(ToastLevel::Warn, "arnes_set works from a command, not a hook".to_owned());
        }
        Effect::Panel { title, text } => state.panel = Some((title, text.lines().map(str::to_owned).collect())),
        Effect::Hint(t) => state.input.hint = Some(t),
        Effect::Toast(level, t) => state.toast(
            match level {
                Level::Info => ToastLevel::Info,
                Level::Warn => ToastLevel::Warn,
                Level::Error => ToastLevel::Error,
            },
            t,
        ),
        Effect::Insert(t) => state.input.editor.insert_str(&t),
    }
}

fn with_builtins(cfg: &ArnesConfig, gate: GatedDispatcher, skills: &Arc<arnes_tools::skills::Skills>) -> GatedDispatcher {
    let gate = gate.with_blue_tools(blue_tools(cfg)).with_hooks(cfg.blue_hooks().0).with_skills(skills.clone());
    if !cfg.builtin_tools.unwrap_or(true) {
        return gate;
    }
    let root = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let guard = if matches!(cfg.guardrail.as_deref(), Some("off")) {
        arnes_loop::permission::Guard::Off
    } else {
        let g = arnes_loop::permission::Guard::production();
        if let arnes_loop::permission::Guard::Unavailable(why) = &g {
            tracing::warn!("guardrail unavailable ({why}); every Bash command will be asked");
        }
        g
    };
    gate.with_builtins(arnes_tools::Builtins::new(root).with_off(&cfg.modules_off)).with_guard(guard)
}


/// The primary provider's model catalogue, from the day-old cache when it
/// can be; `None` for a provider without one or a failed refresh.
async fn model_catalog(cfg: &ArnesConfig, max_age: Duration) -> Option<arnes_anthropic::catalog::Catalog> {
    let provider = cfg.provider().ok()?;
    let (url, auth) = provider.models_url()?;
    let key = cfg.api_key().ok()??;
    arnes_anthropic::catalog::load_or_refresh(provider.name(), url, auth, &key, max_age).await.ok()
}

/// What `/model` offers: `route:N` for each provider route (0 is the
/// configured provider) and `model:<id>` for each model in the primary
/// provider's catalogue. The live choice is marked.
fn model_choices(base: &ArnesConfig, live: &ArnesConfig, catalog: Option<&arnes_anthropic::catalog::Catalog>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let routes = std::iter::once(base.primary_route()).chain(base.fallbacks.iter().cloned());
    for (i, r) in routes.enumerate() {
        let name = r.label.clone().unwrap_or_else(|| r.provedor.clone());
        let model = if r.modelo.is_empty() { "default".to_string() } else { r.modelo.clone() };
        let live_mark = if r.provedor == live.provedor && r.endpoint == live.endpoint { "●" } else { " " };
        out.push((format!("route:{i}"), format!("{live_mark} route  {name:<14} {model}")));
    }
    if let Some(c) = catalog {
        for m in &c.models {
            let live_mark = if base.provedor == live.provedor && m.id == live.model() { "●" } else { " " };
            let ctx = m.context_window.map_or(String::new(), |v| format!("{}k ctx", v / 1000));
            out.push((format!("model:{}", m.id), format!("{live_mark} {:<6} {:<28} {ctx}", base.provedor, m.id)));
        }
    }
    out
}

/// Resolve a `/model` argument against the startup config: `route:N`, a
/// route label or provider name, `model:<id>` or a bare model id (checked
/// against the catalogue when there is one).
fn switch_model(
    base: &ArnesConfig,
    live: &ArnesConfig,
    pick: &str,
    catalog: Option<&arnes_anthropic::catalog::Catalog>,
) -> std::result::Result<ArnesConfig, String> {
    let route = |i: usize| if i == 0 { Ok(base.clone()) } else if i <= base.fallbacks.len() { Ok(base.switch_to(i - 1)) } else { Err(format!("no route {i}")) };
    if let Some(n) = pick.strip_prefix("route:") {
        return route(n.parse().map_err(|_| format!("bad route {n:?}"))?);
    }
    let labelled = std::iter::once(base.primary_route())
        .chain(base.fallbacks.iter().cloned())
        .position(|r| r.label.as_deref() == Some(pick) || r.provedor == pick);
    if let Some(i) = labelled {
        return route(i);
    }
    let id = pick.strip_prefix("model:").unwrap_or(pick);
    if let Some(c) = catalog {
        c.require(id)?;
    }
    let mut next = if live.provedor == base.provedor { live.clone() } else { base.clone() };
    next.modelo = id.to_string();
    Ok(next)
}

/// The `/mcp` browser's model: every configured server, connected or not,
/// with its tools as the pool listed them.
fn mcp_browser(cfg: &ArnesConfig, pool: &McpPool, health: Option<&arnes_vigy::Health>) -> arnes_tui::McpBrowser {
    use arnes_tui::{ServerEntry, ServerStatus, ToolEntry};
    let registry = cfg.to_mcp_registry();
    let log_dir = notice::mcp_log_dir();
    let entry = |name: &str, status: ServerStatus| {
        let command = registry
            .get(name)
            .map(|s| std::iter::once(s.command.as_str()).chain(s.args.iter().map(String::as_str)).collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        let tools = pool
            .tools()
            .iter()
            .filter_map(|t| {
                let (srv, short) = arnes_mcp_client::split_name(&t.name)?;
                (srv == name).then(|| ToolEntry {
                    name: short.to_string(),
                    wire_name: t.name.clone(),
                    description: t.description.clone(),
                    read_only: pool.read_only(&t.name),
                    schema: t.input_schema.0.clone(),
                })
            })
            .collect();
        ServerEntry {
            name: name.to_string(),
            status,
            command,
            log: log_dir.as_ref().map(|d| d.join(format!("{name}.log")).display().to_string()),
            tools,
            health: health.and_then(|h| h.line(name)),
        }
    };
    let mut servers: Vec<ServerEntry> = pool.servers().into_iter().map(|s| entry(s, ServerStatus::Connected)).collect();
    servers.extend(pool.failures().iter().map(|(s, why)| entry(s, ServerStatus::Failed(why.clone()))));
    servers.sort_by(|a, b| a.name.cmp(&b.name));
    let builtin = format!(
        "built-in: {}  ·  {} MCP tools from {} server(s)",
        if cfg.builtin_tools.unwrap_or(true) { "Bash Edit Glob Grep Read Write" } else { "off" },
        pool.tools().len(),
        pool.servers().len()
    );
    arnes_tui::McpBrowser::new(servers, builtin)
}

/// `cfg` with the profile [`choose_profile`] picks applied, and its name;
/// one that cannot apply is named and the session starts without it.
fn start_profile(cfg: ArnesConfig, flag: Option<&str>) -> (ArnesConfig, Option<String>) {
    let active = choose_profile(&cfg, flag);
    match cfg.with_profile(active.as_deref()) {
        Ok((c, refused)) => {
            for r in refused {
                tracing::warn!("{r}");
            }
            (c, active)
        }
        Err(e) => {
            tracing::warn!("{e}; starting without a profile");
            (cfg, None)
        }
    }
}

/// The profile to start in: `--profile`, else this directory's last one,
/// else the hosted `choose-profile` hooks' answer (they receive `{cwd,
/// repo, hour, profiles}`), else `:profile`, else none. A remembered or
/// hooked name that is not declared is passed over.
fn choose_profile(cfg: &ArnesConfig, flag: Option<&str>) -> Option<String> {
    if let Some(f) = flag {
        return Some(f.to_owned());
    }
    if cfg.profiles.is_empty() {
        return None;
    }
    let declared = |n: &str| cfg.profiles.iter().any(|p| p.name == n);
    let cwd = std::env::current_dir().ok();
    if let (Some(layout), Some(dir)) = (arnes_store::Layout::from_env(), cwd.as_deref())
        && let Some(last) = arnes_store::profiles::last(&layout, dir).filter(|n| declared(n))
    {
        return Some(last);
    }
    let (hosted, _) = arnes_blue::bidamas::Registry::load(&arnes_blue::bidamas::BidamaPath::from_env(), &hosted_names(cfg));
    let hooks = hosted.hooks("choose-profile", &cfg.modules_off);
    if !hooks.is_empty() {
        let repo = cwd.as_deref().and_then(|d| d.ancestors().find(|a| a.join(".git").exists())).map(|r| r.display().to_string());
        let hour = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| (d.as_secs() / 3600) % 24);
        let arg = serde_json::json!({
            "cwd": cwd.as_ref().map(|d| d.display().to_string()),
            "repo": repo,
            "hour": hour,
            "profiles": cfg.profiles.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
        })
        .to_string();
        match arnes_blue::policy("choose-profile", &hooks, &arg, module_budget(cfg)) {
            Some(Ok(n)) if declared(n.trim()) => return Some(n.trim().to_owned()),
            Some(Ok(n)) => tracing::warn!("choose-profile hook: {n:?} is not a declared profile; ignored"),
            Some(Err(e)) => tracing::warn!("{e}"),
            None => {}
        }
    }
    cfg.profile.clone()
}

/// The fleet probe lease in the shared vigy store, when `mcp.health.lease`
/// is on and the store opens; otherwise every probe is this process's own.
async fn mcp_lease(cfg: &ArnesConfig) -> Option<Arc<dyn mcp_health::Lease>> {
    if !cfg.mcp_health().2 {
        return None;
    }
    let path = arnes_vigy::store_path()?;
    match arnes_vigy::Lease::open(&path).await {
        Ok(l) => Some(Arc::new(l)),
        Err(e) => {
            tracing::debug!("mcp lease unavailable ({e}); probing alone");
            None
        }
    }
}

/// The servers `cfg` names (`ArnesConfig::mcp_registry_report`), then the
/// hosted `mcp-lazy` hooks asked about each: `"lazy"` or `"eager"` decides,
/// nil leaves the `mcp.lazy` globs' answer. Notes and refusals come back to
/// say.
fn mcp_registry(cfg: &ArnesConfig, hosted: &arnes_blue::bidamas::Registry) -> (arnes_mcp_client::McpRegistry, Vec<String>) {
    let (mut reg, mut notes) = cfg.mcp_registry_report();
    let hooks = hosted.hooks("mcp-lazy", &cfg.modules_off);
    if hooks.is_empty() {
        return (reg, notes);
    }
    for spec in &mut reg.servers {
        let arg = serde_json::json!({"name": spec.name, "command": spec.command, "args": spec.args, "lazy": spec.lazy}).to_string();
        match arnes_blue::policy("mcp-lazy", &hooks, &arg, module_budget(cfg)) {
            Some(Ok(a)) if a.trim() == "lazy" => spec.lazy = true,
            Some(Ok(a)) if a.trim() == "eager" => spec.lazy = false,
            Some(Ok(a)) => notes.push(format!("mcp-lazy hook: {a:?} for {} is not lazy or eager; ignored", spec.name)),
            Some(Err(e)) => notes.push(format!("{e}; {} keeps its mcp.lazy answer", spec.name)),
            None => {}
        }
    }
    (reg, notes)
}

/// Launch every configured MCP server (config `mcp_servers` + the anvil
/// JSON). A server that fails is reported by name on stderr and kept out
/// of the tool list; a `required` one aborts startup.
async fn connect_mcp(cfg: &ArnesConfig, knobs: arnes_config::Knobs) -> Result<McpPool> {
    let registry = cfg.to_mcp_registry();
    let pool = McpPool::connect_logging(&registry, knobs.mcp_connect, notice::mcp_log_dir()).await?.with_call_timeout(knobs.mcp_call);
    for (server, why) in pool.failures() {
        tracing::warn!("MCP server {server} unavailable: {why}");
    }
    tracing::info!(servers = ?pool.servers(), tools = pool.tools().len(), "MCP connected");
    Ok(pool)
}


#[cfg(test)]
mod tests {
    use super::*;
    use arnes_core::{Conversation, ConversationId};

    fn reg() -> arnes_blue::bidamas::Registry {
        let path = arnes_blue::bidamas::BidamaPath::embedded_only();
        arnes_blue::bidamas::Registry::load(&path, &hosted_names(&ArnesConfig::default())).0
    }

    fn serve_d<'a>(name: &str, user: &'a [arnes_config::CommandDecl], cfg: &ArnesConfig) -> Serve<'a> {
        serve(name, user, &reg(), cfg)
    }

    #[test]
    fn the_picker_lists_hosted_commands_with_user_ones_replacing_them() {
        let cfg = ArnesConfig { modules_off: vec!["exit".into()], ..ArnesConfig::default() };
        let user = [decl("help")];
        let rows = picker_commands(&cfg, &reg(), &user);
        let names: Vec<_> = rows.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["clear", "mcp", "mode", "model", "notices", "profile", "resume", "vigy", "vim", "help"]);
        assert_eq!(rows[0].1, "start a new conversation");
        assert_eq!(picker_commands(&ArnesConfig::default(), &arnes_blue::bidamas::Registry::default(), &[]), vec![]);
    }

    fn decl(name: &str) -> arnes_config::CommandDecl {
        arnes_config::CommandDecl { name: name.into(), description: String::new(), blue: "x.b".into(), r#fn: "run".into() }
    }

    #[test]
    fn a_command_resolves_user_then_module_then_kernel() {
        let cfg = ArnesConfig::default();
        let user = [decl("clear")];
        assert_eq!(serve_d("clear", &user, &cfg), Serve::User(&user[0]));
        assert_eq!(serve_d("clear", &[], &cfg), Serve::Module);
        assert_eq!(serve_d("model", &[], &cfg), Serve::Module);
        assert_eq!(serve_d("nosuch", &[], &cfg), Serve::Kernel);
    }

    #[test]
    fn a_module_turned_off_is_off_unless_the_kernel_fallback_is_on() {
        let mut cfg = ArnesConfig { modules_off: vec!["help".into(), "tools".into()], ..ArnesConfig::default() };
        assert_eq!(serve_d("help", &[], &cfg), Serve::Off);
        assert_eq!(serve_d("exit", &[], &cfg), Serve::Module);
        assert_eq!(std_segment(&cfg, &reg(), "tools", "3"), Some(String::new()));
        cfg.kernel_fallback = Some(true);
        assert_eq!(serve_d("help", &[], &cfg), Serve::Kernel);
        assert_eq!(std_segment(&cfg, &reg(), "tools", "3"), None);
    }

    #[test]
    fn every_built_in_is_a_bidama_command_so_a_user_command_may_replace_any() {
        let cfg = ArnesConfig { commands: vec![decl("help"), decl("model"), decl("help")], ..ArnesConfig::default() };
        let (ok, refused) = blue_commands_checked(&cfg);
        assert_eq!(ok.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["help", "model"]);
        assert_eq!(refused.len(), 1);
        assert!(refused[0].contains("twice"));
        assert!(arnes_tui::COMMANDS.iter().all(|(n, _)| arnes_blue::bidamas::KERNEL_TWINS.contains(n)));
    }

    fn state() -> TuiState {
        TuiState::new(Conversation::new(ConversationId(1), "m"), "m")
    }

    #[test]
    fn the_module_help_panel_is_exactly_the_kernel_help_lines() {
        let keymap = Keymap::claude_code();
        let mut st = state();
        let kernel = arnes_tui::render::help_lines(&keymap, st.nav.keymap());
        assert!(run_std_command(&mut st, &reg(), "help", "", &keymap, "", arnes_blue::DEFAULT_BUDGET, &[]).is_empty());
        assert_eq!(st.panel, Some(("help".to_owned(), kernel)));
    }

    #[test]
    fn module_vim_mode_and_notices_run_the_kernel_arms_they_replace() {
        let keymap = Keymap::claude_code();
        let mut st = state();
        let b = arnes_blue::DEFAULT_BUDGET;
        assert_eq!(run_std_command(&mut st, &reg(), "vim", "", &keymap, "", b, &[]), [("vim".to_owned(), String::new())]);
        assert_eq!(run_std_command(&mut st, &reg(), "mode", "plan", &keymap, "", b, &[]), [("mode".to_owned(), "plan".to_owned())]);
        assert!(run_std_command(&mut st, &reg(), "mode", "sideways", &keymap, "", b, &[]).is_empty());
        assert!(st.last_error.as_deref().is_some_and(|e| e.contains("/mode")), "{:?}", st.last_error);
        st.toast(arnes_tui::ToastLevel::Warn, "careful".to_owned());
        let kernel = st.notice_lines();
        assert!(run_std_command(&mut st, &reg(), "notices", "", &keymap, "", b, &[]).is_empty());
        assert_eq!(st.panel, Some(("notices".to_owned(), kernel)));
    }

    #[test]
    fn module_resume_lists_sessions_in_a_panel_or_runs_the_kernel_arm() {
        let keymap = Keymap::claude_code();
        let mut st = state();
        let b = arnes_blue::DEFAULT_BUDGET;
        let listed = "1  2026-09-28  fix the build\n2  2026-09-27  plan";
        assert!(run_std_command(&mut st, &reg(), "resume", "", &keymap, listed, b, &[]).is_empty());
        assert_eq!(st.panel, Some(("sessions".to_owned(), vec!["1  2026-09-28  fix the build".to_owned(), "2  2026-09-27  plan".to_owned()])));
        assert_eq!(run_std_command(&mut st, &reg(), "resume", "2", &keymap, listed, b, &[]), [("resume".to_owned(), "2".to_owned())]);
        st.panel = None;
        assert!(run_std_command(&mut st, &reg(), "resume", "", &keymap, "", b, &[]).is_empty());
        assert_eq!(st.panel, Some(("sessions".to_owned(), vec![])));
    }

    #[test]
    fn the_kenban_keymap_is_exactly_the_kernel_keymap_and_off_keeps_the_kernel_one() {
        let cfg = ArnesConfig::default();
        let (k, refused) = hosted_keymap(&cfg, &reg(), Keymap::from_rows(&[]).unwrap());
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(k, Keymap::claude_code());
        let off = ArnesConfig { modules_off: vec!["kenban".into()], ..ArnesConfig::default() };
        let (kept, _) = hosted_keymap(&off, &reg(), Keymap::from_rows(&[]).unwrap());
        assert_eq!(kept, Keymap::from_rows(&[]).unwrap());
    }

    #[test]
    fn a_built_in_tool_turned_off_is_not_sent_and_its_name_is_free_for_a_blue_tool() {
        let on = ArnesConfig::default();
        let pool = McpPool::empty();
        let names = |cfg: &ArnesConfig| session_tools(cfg, &pool, &arnes_tools::skills::Skills::default()).unwrap().into_iter().map(|t| t.name).collect::<Vec<_>>();
        assert!(names(&on).contains(&"Bash".to_owned()));
        let off = ArnesConfig { modules_off: vec!["Bash".into()], ..ArnesConfig::default() };
        assert!(!names(&off).contains(&"Bash".to_owned()));
        assert!(names(&off).contains(&"Read".to_owned()));
        let tool = |cfg: ArnesConfig| ArnesConfig {
            tools: vec![arnes_config::ToolDecl {
                name: "Bash".into(),
                description: String::new(),
                blue: "b.b".into(),
                r#fn: "run".into(),
                schema: None,
                read_only: None,
            }],
            ..cfg
        };
        assert_eq!(blue_tools_checked(&tool(off)).1.len(), 0);
        assert_eq!(blue_tools_checked(&tool(on)).1.len(), 1);
        let b = arnes_tools::Builtins::new(".").with_off(&["Bash".to_owned(), "help".to_owned()]);
        assert!(!b.serves("Bash") && b.serves("Read") && !b.serves("help"));
    }

    /// A bidama on `$ARNES_BIDAMAS` for the policy-hook tests: each package
    /// gets its own directory, so tests running at once do not collide.
    fn policy_bidama(package: &str, body: &str) -> arnes_blue::bidamas::Registry {
        static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        let dir = DIR.get_or_init(|| {
            let d = tempfile::tempdir().unwrap();
            // SAFETY: set once, before any hook in this process reads it.
            unsafe { std::env::set_var("ARNES_BIDAMAS", d.path()) };
            d
        });
        std::fs::create_dir_all(dir.path().join(package)).unwrap();
        std::fs::write(dir.path().join(format!("{package}/{package}.b")), body).unwrap();
        let (reg, refused) = arnes_blue::bidamas::Registry::load(&arnes_blue::bidamas::BidamaPath::from_env(), &[package.to_owned()]);
        assert!(refused.is_empty(), "{refused:?}");
        reg
    }

    fn skill_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (n, d) in [("arnes", "develop arnes"), ("bidama", "blue packages"), ("kit", "go tools")] {
            std::fs::create_dir_all(dir.path().join(n)).unwrap();
            std::fs::write(dir.path().join(n).join("SKILL.md"), format!("---\nname: {n}\ndescription: {d}\n---\n{n} body\n")).unwrap();
        }
        dir
    }

    #[test]
    fn the_system_prompt_is_the_prompt_then_context_files_then_the_fragment_then_skills() {
        let dir = skill_dir();
        let ctx = tempfile::tempdir().unwrap();
        let inside = ctx.path().join("OPS.md");
        std::fs::write(&inside, "Terragrunt, never pangea.\n").unwrap();
        let big = ctx.path().join("BIG.md");
        std::fs::write(&big, "x".repeat(2048)).unwrap();
        let cfg = ArnesConfig {
            prompt_sistema: Some("You are arnes.".into()),
            context: arnes_config::ContextSection {
                files: vec![inside.display().to_string(), big.display().to_string(), "/etc/hosts".into()],
                discover: Some(false),
                prompt: Some("Be terse.".into()),
                max_kib: Some(1),
            },
            access: arnes_config::AccessSection { read_roots: vec![ctx.path().display().to_string()], key_files: vec![] },
            skills: arnes_config::SkillsSection { dirs: vec![dir.path().display().to_string()], ..Default::default() },
            ..ArnesConfig::default()
        };
        let (skills, notes) = session_skills(&cfg, &arnes_blue::bidamas::Registry::default());
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(skills.names(), ["arnes", "bidama", "kit"]);
        let (system, notes) = system_prompt(&cfg, &skills);
        let system = system.unwrap();
        let at = |needle: &str| system.find(needle).unwrap_or_else(|| panic!("{needle} missing from {system}"));
        assert!(at("You are arnes.") < at("never pangea") && at("never pangea") < at("Be terse.") && at("Be terse.") < at("- bidama: blue packages"));
        assert_eq!(notes.len(), 2, "the big file and the one outside the roots are named: {notes:?}");
        assert!(notes.iter().any(|n| n.contains("over 1 KiB")) && notes.iter().any(|n| n.contains("/etc/hosts")));
        let bare = system_prompt(&ArnesConfig { context: arnes_config::ContextSection { discover: Some(false), ..Default::default() }, ..ArnesConfig::default() }, &arnes_tools::skills::Skills::default());
        assert_eq!(bare, (None, vec![]), "nothing configured, nothing added");
        let off = ArnesConfig { skills: arnes_config::SkillsSection { enabled: Some(false), ..cfg.skills.clone() }, ..cfg };
        assert!(session_skills(&off, &arnes_blue::bidamas::Registry::default()).0.is_empty());
    }

    #[test]
    fn the_skills_hook_keeps_the_names_it_returns() {
        let dir = skill_dir();
        let cfg = ArnesConfig { skills: arnes_config::SkillsSection { dirs: vec![dir.path().display().to_string()], ..Default::default() }, ..ArnesConfig::default() };
        let reg = policy_bidama(
            "erabu",
            "def exports()\n  \"hook skills keep\"\nend\n\
             def keep(s)\n  join(filter(fn(n) n != \"kit\" end, map(fn(k) json_get(k, \"name\") end, json_parse(s))), \"\\n\")\nend\n",
        );
        let (skills, notes) = session_skills(&cfg, &reg);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(skills.names(), ["arnes", "bidama"]);
    }

    #[test]
    fn the_mcp_lazy_hook_decides_each_server_and_nil_keeps_the_globs() {
        let decl = |n: &str| arnes_config::McpServerDecl { nome: n.into(), command: n.into(), args: vec![], env: std::collections::HashMap::new(), cwd: None, required: false };
        let cfg = ArnesConfig {
            mcp_servers: vec![decl("splunk"), decl("github"), decl("codesearch")],
            mcp: arnes_config::McpSection { lazy: vec!["codesearch".into()], ..Default::default() },
            ..ArnesConfig::default()
        };
        let (plain, notes) = mcp_registry(&cfg, &arnes_blue::bidamas::Registry::default());
        assert!(notes.is_empty());
        let lazy = |r: &arnes_mcp_client::McpRegistry| r.servers.iter().filter(|s| s.lazy).map(|s| s.name.clone()).collect::<Vec<_>>();
        assert_eq!(lazy(&plain), ["codesearch"]);
        let reg = policy_bidama(
            "namake",
            "def exports()\n  \"hook mcp-lazy decide\"\nend\n\
             def decide(s)\n  n = json_get(json_parse(s), \"name\")\n  if n == \"splunk\"\n    \"lazy\"\n  elsif n == \"github\"\n    \"sometimes\"\n  else\n    nil\n  end\nend\n",
        );
        let (hooked, notes) = mcp_registry(&cfg, &reg);
        assert_eq!(lazy(&hooked), ["splunk", "codesearch"], "splunk by the hook, codesearch by the glob");
        assert_eq!(notes.len(), 1, "github's bad answer is named: {notes:?}");
        let off = ArnesConfig { modules_off: vec!["namake".into()], ..cfg };
        assert_eq!(lazy(&mcp_registry(&off, &reg).0), ["codesearch"], "a policy bidama configured off has no say");
    }

    #[test]
    fn hosted_vigies_join_the_declared_ones_and_a_declared_one_wins_its_name() {
        let path = arnes_blue::bidamas::BidamaPath::embedded_only();
        let (reg, _) = arnes_blue::bidamas::Registry::load(&path, &["kenkou".to_owned()]);
        let none = all_vigies(&ArnesConfig::default(), &arnes_blue::bidamas::Registry::default());
        assert!(none.is_empty());
        let hosted = all_vigies(&ArnesConfig::default(), &reg);
        assert_eq!(hosted.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["mcp-health"]);
        let decl = arnes_config::VigyDecl { name: "mcp-health".into(), blue: "mine.b".into(), r#fn: "t".into(), every_ms: None };
        let cfg = ArnesConfig { vigies: vec![decl], ..ArnesConfig::default() };
        let mine = all_vigies(&cfg, &reg);
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].script.function, "t");
    }

    #[test]
    fn the_mcp_segment_is_kenkous_when_hosted_and_matches_the_kernel_twin() {
        let path = arnes_blue::bidamas::BidamaPath::embedded_only();
        let (reg, _) = arnes_blue::bidamas::Registry::load(&path, &["kenkou".to_owned()]);
        let cfg = ArnesConfig::default();
        let f = arnes_vigy::Facts::default();
        let kernel = arnes_blue::bidamas::Registry::default();
        assert_eq!(mcp_segment(&cfg, &reg, Some(&f)), None);
        assert_eq!(mcp_segment(&cfg, &reg, None), None);
        let json = r#"[{"server":"a","up":true,"latency_ms":3,"tools":1,"error":null},{"server":"b","up":false,"latency_ms":null,"tools":0,"error":"x"}]"#;
        f.set(serde_json::from_str(json).unwrap());
        assert_eq!(mcp_segment(&cfg, &reg, Some(&f)).as_deref(), Some("mcp 1/2"));
        assert_eq!(mcp_segment(&cfg, &reg, Some(&f)), mcp_segment(&cfg, &kernel, Some(&f)));
    }

    #[test]
    fn sanshou_attaches_exactly_what_the_kernel_twin_attaches() {
        let dir = std::env::temp_dir().join(format!("arnes-sanshou-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        std::fs::write(dir.join("b.md"), "beta").unwrap();
        std::fs::write(dir.join("big"), "x".repeat(64)).unwrap();
        let d = dir.display();
        let prompt = format!("read @{d}/a.txt, then @{d}/b.md). skip @{d}/big @{d}/none @{d} and @{d}/a.txt again");
        let (kernel, n) = expand_mentions(&prompt, 32, &|_| true);
        assert_eq!(n, 2);
        let path = arnes_blue::bidamas::BidamaPath::embedded_only();
        let (reg, _) = arnes_blue::bidamas::Registry::load(&path, &hosted_names(&ArnesConfig::default()));
        let hooks = reg.hooks("before-submit", &[]);
        assert_eq!(hooks.len(), 1);
        let snap = vec![("mention-max-bytes".to_owned(), "32".to_owned())];
        let out = arnes_blue::invoke_with("t", &hooks[0], &[&prompt], arnes_blue::Context { state: &snap, ..arnes_blue::Context::default() }).unwrap();
        assert_eq!(out.text(), kernel);
        assert_eq!(out.effects, vec![arnes_blue::Effect::Hint("attached 2 file(s)".into())]);
        let none = arnes_blue::invoke_with("t", &hooks[0], &["no mentions"], arnes_blue::Context { state: &snap, ..arnes_blue::Context::default() }).unwrap();
        assert_eq!(none.returned, arnes_blue::Returned::Nothing);
        assert!(reg.hooks("before-submit", &["sanshou".to_owned()]).is_empty());
        // Outside access.read-roots, neither path attaches anything, and they still agree.
        let elsewhere = vec![std::path::PathBuf::from("/nonexistent-root")];
        let cfg = ArnesConfig { access: arnes_config::AccessSection { read_roots: vec!["/nonexistent-root".into()], key_files: vec![] }, ..ArnesConfig::default() };
        let (bounded, n) = expand_mentions(&prompt, 32, &|p| cfg.read_allowed(p));
        assert_eq!((bounded.as_str(), n), (prompt.as_str(), 0));
        let out = arnes_blue::invoke_with("t", &hooks[0], &[&prompt], arnes_blue::Context { state: &snap, read_roots: &elsewhere, ..arnes_blue::Context::default() }).unwrap();
        assert_eq!(out.returned, arnes_blue::Returned::Nothing, "sanshou reads nothing outside the roots");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn setting_arm_is_already_so_or_the_arm_or_a_named_error() {
        let st = state();
        assert_eq!(setting_arm(&st, "vim", "off"), Ok(None));
        assert_eq!(setting_arm(&st, "vim", "on"), Ok(Some(("vim".into(), String::new()))));
        assert!(setting_arm(&st, "vim", "maybe").is_err());
        assert!(setting_arm(&st, "colour", "red").is_err());
    }

    #[test]
    fn module_clear_and_exit_hand_the_kernel_its_own_arms() {
        let keymap = Keymap::claude_code();
        let mut st = state();
        assert_eq!(run_std_command(&mut st, &reg(), "clear", "", &keymap, "", arnes_blue::DEFAULT_BUDGET, &[]), [("clear".to_owned(), String::new())]);
        assert_eq!(run_std_command(&mut st, &reg(), "exit", "", &keymap, "", arnes_blue::DEFAULT_BUDGET, &[]), [("exit".to_owned(), String::new())]);
        assert!(st.panel.is_none());
    }

    #[test]
    fn hosted_segments_draw_what_the_kernel_drew_and_run_only_on_change() {
        let cfg = ArnesConfig::default();
        let mut st = state();
        st.effort = "high".into();
        st.permission_mode = "plan".into();
        let mut memo = SegmentMemo::default();
        assert!(memo.refresh(&mut st, &cfg, &reg()));
        assert_eq!(st.hosted_segments["model"], " m");
        assert_eq!(st.hosted_segments["effort"], " \u{b7} effort high");
        assert_eq!(st.hosted_segments["permission-mode"], " \u{b7} plan mode");
        assert_eq!(st.hosted_segments["idle"], "ready \u{b7} m");
        assert!(!memo.refresh(&mut st, &cfg, &reg()));
        st.effort = "low".into();
        assert!(memo.refresh(&mut st, &cfg, &reg()));
        assert_eq!(st.hosted_segments["effort"], " \u{b7} effort low");
    }

    #[test]
    fn the_module_tools_segment_draws_what_the_kernel_drew() {
        let cfg = ArnesConfig::default();
        assert_eq!(std_segment(&cfg, &reg(), "tools", "7").as_deref(), Some(" \u{b7} 7 tools"));
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use arnes_anthropic::testing::one_shot_server;
    use arnes_config::{KeyEdits, KeysSection, NavKeysSection, RouteDecl, UiSection};

    const THINKING_REPLY: &str = r#"{"id":"a","type":"message","role":"assistant","model":"local","content":[{"type":"thinking","thinking":"hmm","signature":""},{"type":"text","text":"pong-a"}],"stop_reason":"end_turn"}"#;
    const PLAIN_REPLY: &str = r#"{"id":"b","type":"message","role":"assistant","model":"claude-opus-5-5","content":[{"type":"text","text":"pong-b"}],"stop_reason":"end_turn"}"#;

    #[test]
    fn a_route_choice_moves_the_turn_and_an_abstention_an_error_or_an_undeclared_label_keeps_it() {
        let cfg = ArnesConfig {
            fallbacks: vec![arnes_config::RouteDecl { provedor: "local".into(), endpoint: Some("http://127.0.0.1:9".into()), label: Some("gpubox".into()), ..Default::default() }],
            ..ArnesConfig::default()
        };
        let mut live = live(&cfg);
        let mut state = TuiState::new(new_conversation(&cfg), cfg.model());
        assert_eq!(routing::label_of(&live.cfg), "local");
        apply_routed(&mut live, &mut state, None);
        apply_routed(&mut live, &mut state, Some(Err("over budget".into())));
        apply_routed(&mut live, &mut state, Some(Ok(r#"{"route":"nowhere","why":"x"}"#.into())));
        assert_eq!((routing::label_of(&live.cfg).as_str(), live.route_source), ("local", "config"), "nothing moved");
        apply_routed(&mut live, &mut state, Some(Ok(r#"{"route":"gpubox","why":"floor"}"#.into())));
        assert_eq!((routing::label_of(&live.cfg).as_str(), live.route_source), ("gpubox", "auto"));
        let out = apply_routed(&mut live, &mut state, Some(Ok("local".into())));
        assert_eq!((routing::label_of(&live.cfg).as_str(), out.as_str()), ("local", "local"));
    }

    fn live(cfg: &ArnesConfig) -> Live {
        let pool = Arc::new(McpPool::empty());
        let gate = Arc::new(GatedDispatcher::new(pool.clone(), Arc::new(NoHuman), cfg.permission_policy().unwrap()));
        let (knobs, _) = cfg.knobs();
        Live {
            cfg: cfg.clone(),
            base: cfg.clone(),
            client: build_client(cfg).unwrap().0,
            registry: cfg.to_mcp_registry(),
            tools: Arc::new(session_tools(cfg, &pool, &arnes_tools::skills::Skills::default()).unwrap()),
            pool,
            gate,
            knobs,
            keymap: Keymap::claude_code(),
            commands: Vec::new(),
            hosted: arnes_blue::bidamas::Registry::default(),
            hooks: cfg.blue_hooks().0,
            notifications: true,
            pool_tx: tokio::sync::watch::channel(Arc::new(McpPool::empty())).0,
            registry_tx: tokio::sync::watch::channel(arnes_mcp_client::McpRegistry::new()).0,
            health_tx: tokio::sync::watch::channel(None).0,
            probe_hooks_tx: tokio::sync::watch::channel(Vec::new()).0,
            file: cfg.clone(),
            profile: None,
            skills: Arc::new(arnes_tools::skills::Skills::default()),
            route_source: "config",
            reach: None,
        }
    }

    #[tokio::test]
    async fn a_profile_switch_reapplies_the_file_with_the_overlay_and_keeps_the_conversation() {
        let ops = arnes_config::ProfileDecl {
            name: "ops".into(),
            description: Some("read-mostly".into()),
            inherits: vec![],
            config: serde_json::json!({"permission_mode": "plan", "deny_tools": ["Bash"], "context": {"prompt": "Terragrunt, never pangea."}}),
        };
        let cfg = ArnesConfig { profiles: vec![ops], ..ArnesConfig::default() };
        let mut live = live(&cfg);
        let mut state = TuiState::new(new_conversation(&cfg), cfg.model());
        state.conversation.push(Message::user_text("keep me"));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (mcp_tx, _mcp_rx) = mpsc::unbounded_channel();
        live.switch_profile("nope", &mut state, &tx);
        assert!(rx.try_recv().is_err(), "an unknown profile changes nothing");
        assert!(state.last_error.as_deref().is_some_and(|e| e.contains("ops")));
        live.switch_profile("ops", &mut state, &tx);
        let loaded = rx.try_recv().unwrap().unwrap();
        assert_eq!(loaded.config, live.file, "the file is re-applied, not the running config");
        let loaded = live.profiled(loaded, &mut state);
        assert_eq!(state.profile, "ops");
        live.reload(loaded, &mut state, egaku_term::theme::ColorDepth::TrueColor, &mcp_tx);
        assert_eq!(state.permission_mode, "plan", "the gate took the profile's mode");
        assert_eq!(live.cfg.deny_tools, ["Bash"]);
        assert!(state.conversation.system.as_deref().is_some_and(|s| s.contains("never pangea")), "the context applies to the next turn");
        assert_eq!(state.conversation.messages.len(), 1, "the conversation is kept");
        live.switch_profile("-", &mut state, &tx);
        let loaded = live.profiled(rx.try_recv().unwrap().unwrap(), &mut state);
        live.reload(loaded, &mut state, egaku_term::theme::ColorDepth::TrueColor, &mcp_tx);
        assert_eq!((state.profile.as_str(), state.permission_mode.as_str()), ("", "default"), "no profile: the file alone");
    }

    #[tokio::test]
    async fn an_arrival_attaches_only_while_the_config_still_names_that_server() {
        let cfg = ArnesConfig {
            mcp_servers: vec![arnes_config::McpServerDecl {
                nome: "ghost".into(),
                command: "/nonexistent/arnes-mcp".into(),
                args: vec![],
                env: std::collections::HashMap::new(),
                cwd: None,
                required: false,
            }],
            ..ArnesConfig::default()
        };
        let mut live = live(&cfg);
        let mut state = TuiState::new(new_conversation(&cfg), cfg.model());
        let spec = live.registry.get("ghost").unwrap().clone();
        let a = McpPool::connect_server(spec.clone(), Duration::from_secs(2), None).await;
        live.arrived(a, &mut state);
        assert_eq!(live.pool.failures().len(), 1, "its failure is recorded");
        assert_eq!(live.pool_tx.borrow().failures().len(), 1, "and the scheduler sees the new pool");
        let mut outdated = spec;
        outdated.command = "/nonexistent/arnes-old".into();
        live.arrived(McpPool::connect_server(outdated, Duration::from_secs(2), None).await, &mut state);
        let kept = &live.pool.failures()[0].1;
        assert!(kept.contains("arnes-mcp") && !kept.contains("arnes-old"), "an arrival for a spec the config no longer names is dropped: {kept}");
        live.feed_health();
        assert!(live.health_tx.borrow().is_some(), "probing is on by default");
        assert_eq!(live.registry_tx.borrow().names(), ["ghost"]);
    }

    fn key_file() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("arnes-live-key-{}", std::process::id()));
        std::fs::write(&p, "sk-test").unwrap();
        p
    }

    #[tokio::test]
    async fn a_route_switch_mid_conversation_keeps_it_and_sends_the_next_turn_there() {
        let (a_url, a_seen) = one_shot_server("200 OK", THINKING_REPLY);
        let (b_url, b_seen) = one_shot_server("200 OK", PLAIN_REPLY);
        let cfg = ArnesConfig {
            provedor: "local".into(),
            endpoint: Some(a_url),
            builtin_tools: Some(false),
            fallbacks: vec![RouteDecl {
                provedor: "anthropic".into(),
                modelo: String::new(),
                endpoint: Some(b_url),
                api_key_file: Some(key_file()),
                key_env: None,
                timeout_secs: None,
                system_note: None,
                tools: None,
                label: Some("b".into()),
                facts: arnes_config::RouteFacts::default(),
            }],
            ..ArnesConfig::default()
        };
        let mut live = live(&cfg);
        let mut state = TuiState::new(new_conversation(&cfg), cfg.model());
        state.conversation.push(Message::user_text("hello"));
        run_turn_recorded(&live.client, &mut state.conversation, &live.tools, &*live.gate, 4, None).await.unwrap();
        assert!(a_seen.join().unwrap().contains("hello"));
        assert_eq!(state.conversation.messages.len(), 2);
        state.input.set_text("a half-typed prompt");

        let next = switch_model(&live.base, &live.cfg, "b", None).unwrap();
        live.switch_route(&next, &mut state).unwrap();
        live.cfg = next;
        assert_eq!(state.conversation.messages.len(), 2, "the conversation stays");
        assert!(state.conversation.messages[1].text().contains("pong-a"));
        assert!(state.toasts.iter().any(|t| t.text.contains("thinking block")), "the dropped reasoning is said");
        assert_eq!(state.input.editor.text(), "a half-typed prompt", "the prompt stays");
        assert_eq!(state.model, "claude-opus-5-5");

        state.conversation.push(Message::user_text("again"));
        run_turn_recorded(&live.client, &mut state.conversation, &live.tools, &*live.gate, 4, None).await.unwrap();
        let sent = b_seen.join().unwrap();
        assert!(sent.contains("hello") && sent.contains("pong-a") && sent.contains("again"), "the new route got the whole conversation: {sent}");
        assert!(!sent.contains("\"thinking\""), "and none of the old route's reasoning: {sent}");
        assert!(sent.to_ascii_lowercase().contains("x-api-key: sk-test"));
        assert!(state.conversation.messages.last().unwrap().text().contains("pong-b"));
    }

    fn loaded(config: ArnesConfig) -> arnes_config::Loaded {
        arnes_config::Loaded { config, refused: Vec::new(), file: serde_json::Value::Null, env: serde_json::Value::Null }
    }

    #[tokio::test]
    async fn a_reload_swaps_keys_look_knobs_and_mcp_without_touching_the_conversation() {
        let cfg = ArnesConfig { builtin_tools: Some(false), ..ArnesConfig::default() };
        let mut live = live(&cfg);
        let mut state = TuiState::new(new_conversation(&cfg), cfg.model());
        state.conversation.push(Message::user_text("keep me"));
        state.input.set_text("draft");
        let (mcp_tx, mut mcp_rx) = mpsc::unbounded_channel();
        let next = ArnesConfig {
            ui: UiSection {
                motion: Some("off".into()),
                theme: Some("vellum".into()),
                timing: arnes_config::TimingSection { frame_rate: Some(30), ..Default::default() },
                ..UiSection::default()
            },
            keys: KeysSection {
                prompt: KeyEdits { bind: vec![("ctrl+q".into(), "app:exit".into())], unbind: vec![] },
                nav: NavKeysSection::default(),
            },
            mcp_servers: vec![arnes_config::McpServerDecl {
                nome: "ghost".into(),
                command: "/nonexistent/arnes-mcp".into(),
                args: vec![],
                env: std::collections::HashMap::new(),
                cwd: None,
                required: false,
            }],
            builtin_tools: Some(true),
            ..cfg.clone()
        };
        let fx = live.reload(loaded(next), &mut state, egaku_term::theme::ColorDepth::TrueColor, &mcp_tx);
        assert!(!state.motion);
        assert!(fx.pacer, "a new frame rate asks for a new pacer");
        assert_eq!(live.keymap.clone().resolve(awase::Hotkey::parse("ctrl+q").unwrap()), arnes_tui::Resolution::Action(arnes_tui::Action::Exit));
        assert!(live.tools.iter().any(|t| t.name == "Read"), "built-ins turned on live");
        assert_eq!(state.conversation.messages.len(), 1);
        assert_eq!(state.input.editor.text(), "draft");
        let swap = tokio::time::timeout(Duration::from_secs(10), mcp_rx.recv()).await.unwrap().unwrap();
        assert_eq!(swap.report.failed.len(), 1);
        live.mcp_swapped(swap, &mut state);
        assert!(state.toasts.iter().any(|t| t.text.contains("ghost")), "the server that could not start is named");
        assert_eq!(live.registry.servers.len(), 1);
    }
}
