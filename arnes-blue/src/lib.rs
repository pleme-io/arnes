//! `arnes-blue`: arnes runs user-authored blue.
//!
//! A slash command, a model-callable tool or a hook is declared in the config
//! and implemented as a function in a `.b` file. This crate runs it through
//! `blue_lang_runtime::hosted` (blue's one embedding seam) with a host that
//! gives the program a typed view of arnes:
//!
//! | primitive | effect |
//! |---|---|
//! | `arnes_conversation()` | the conversation so far, `role: text` per message |
//! | `arnes_hint(text)` | the one-line status hint |
//! | `arnes_toast(level, text)` | a toast, level `"info"` / `"warn"` / `"error"` |
//! | `arnes_insert(text)` | text inserted into the prompt at the caret |
//! | `arnes_store_get(key)` | this program's saved value for `key`, or `nil` |
//! | `arnes_store_set(key, value)` | save a string (both strings) |
//! | `arnes_store_del(key)` | forget `key` |
//! | `arnes_clear()` / `arnes_exit()` | ask the kernel to start a new conversation / quit (commands only) |
//! | `arnes_panel(title, text)` | open a navigable panel (commands only) |
//! | `arnes_bindings()` | the keymap's help lines |
//! | `arnes_read_file(path, max)` | a text file of at most `max` bytes, else `nil` |
//!
//! [`bidamas`] is how arnes hosts bidamas: its own distribution (`bidamas/`), resolved by name
//! (`docs/kernel.md`).
//!
//! Effects are collected while the program runs and returned with its value,
//! so a program that raises applies nothing but the error. Every invocation
//! builds a fresh interpreter and reads the file again, and an edited `.b`
//! takes effect on the next one. The only state that survives is the store:
//! `$XDG_DATA_HOME/arnes/ext/<file stem>.json`, shared by every function in
//! that `.b`, bounded by [`arnes_store::ext::MAX_KEYS`] and
//! [`arnes_store::ext::MAX_BYTES`], and written only when the call succeeds.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arnes_core::{Tool, ToolInputSchema};
use blue_lang_host::{Entry, Hosted, Interpreter, IntoValue, Value};

/// Where an extension's code is: a `.b` file and the function to call in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub path: PathBuf,
    pub function: String,
}

impl Script {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, function: impl Into<String>) -> Self {
        Self { path: path.into(), function: function.into() }
    }
}

impl fmt::Display for Script {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.path.display(), self.function)
    }
}

/// How loud a toast a program asked for is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    pub const ALL: [(&'static str, Self); 3] = [("info", Self::Info), ("warn", Self::Warn), ("error", Self::Error)];

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().find(|(n, _)| *n == s).map(|(_, l)| *l)
    }
}

/// Something a program asked arnes to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Hint(String),
    Toast(Level, String),
    Insert(String),
    /// Run one of the kernel's own command arms with `args`: `clear`
    /// (`arnes_clear()`), `exit` (`arnes_exit()`), `resume`
    /// (`arnes_resume(q)`), `model` (`arnes_pick("model")`,
    /// `arnes_route_switch(name)`), `mcp` and `vigy` (`arnes_open(view, args)`).
    /// Each host call validates its own domain, so the arm and its arguments
    /// are always ones the kernel serves.
    Arm { arm: String, args: String },
    /// Open a navigable panel in the conversation region (`arnes_panel(title, text)`).
    Panel { title: String, text: String },
    /// Change a setting (`arnes_set(key, value)`); the kernel validates it.
    Set { key: String, value: String },
}

/// The pickers `arnes_pick(kind)` opens.
pub const PICKERS: &[&str] = &["model", "profile"];

/// The kernel views `arnes_open(view, args)` opens.
pub const VIEWS: &[&str] = &["mcp", "vigy"];

fn arm(arm: &str, args: &str) -> Effect {
    Effect::Arm { arm: arm.to_owned(), args: args.to_owned() }
}

#[derive(Debug, thiserror::Error)]
enum Refusal {
    #[error("unknown {what}{value:?}; expected one of {legal:?}")]
    Unknown { what: &'static str, value: String, legal: Vec<String> },
    #[error("{0}")]
    Empty(&'static str),
    #[error("arnes_http is available only to choose-route hooks")]
    NoReach,
    #[error("at most {MAX_REACH_CALLS} arnes_http calls per hook")]
    OverReach,
    #[error("{0}")]
    Reached(String),
}

fn unknown<'a>(what: &'static str, value: String, legal: impl IntoIterator<Item = &'a str>) -> Refusal {
    Refusal::Unknown { what, value, legal: legal.into_iter().map(str::to_owned).collect() }
}

fn one_of(value: String, legal: &[&str]) -> Result<String, Refusal> {
    if legal.contains(&value.as_str()) { Ok(value) } else { Err(unknown("", value, legal.iter().copied())) }
}

fn named(value: &str, empty: &'static str) -> Result<String, Refusal> {
    let value = value.trim();
    if value.is_empty() { Err(Refusal::Empty(empty)) } else { Ok(value.to_owned()) }
}

/// The settings `arnes_set` accepts, and what each takes.
pub const SETTINGS: &[(&str, &str)] = &[("vim", "on | off"), ("permission-mode", "default | plan | bypass")];

#[derive(Clone)]
pub struct Reach(pub Arc<dyn Fn(&str, &str) -> Result<String, String> + Send + Sync>);

impl std::fmt::Debug for Reach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reach")
    }
}

/// What a program can read of arnes, captured before the call.
#[derive(Debug, Clone, Copy, Default)]
pub struct Context<'a> {
    /// `arnes_conversation()`.
    pub conversation: &'a str,
    /// `arnes_bindings()`: the help lines the keymap renders, one per line.
    pub bindings: &'a str,
    /// `arnes_notices()`: this session's notices, newest first, one per line.
    pub notices: &'a str,
    /// `arnes_state(key)`: a snapshot of arnes's state, `(key, value)`.
    pub state: &'a [(String, String)],
    /// `arnes_sessions()`: the recent sessions, one per line.
    pub sessions: &'a str,
    /// `access.read-roots`: where `arnes_read_file` may read; empty = anywhere.
    pub read_roots: &'a [PathBuf],
    pub reach: Option<&'a Reach>,
}

/// Whether `path` lies under one of `roots` (both resolved, `~` expanded),
/// or `roots` is empty: the one rule for `access.read-roots`
/// (docs/profiles.md §2).
#[must_use]
pub fn within_roots(roots: &[PathBuf], path: &Path) -> bool {
    if roots.is_empty() {
        return true;
    }
    let resolve = |p: &Path| {
        let p = expand_home(p);
        std::fs::canonicalize(&p).unwrap_or(p)
    };
    let abs = resolve(path);
    roots.iter().any(|r| abs.starts_with(resolve(r)))
}

/// What a program's function returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Returned {
    Text(String),
    Nothing,
}

/// A finished invocation: its value and the effects it asked for, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub returned: Returned,
    pub effects: Vec<Effect>,
}

impl Outcome {
    /// The returned text, or empty when the function returned `nil`.
    #[must_use]
    pub fn text(&self) -> &str {
        match &self.returned {
            Returned::Text(t) => t,
            Returned::Nothing => "",
        }
    }
}

/// Which stage an extension failed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The file could not be read.
    Read,
    /// Parse, import, type or top-level runtime error while loading.
    Load,
    /// The function is missing, or raised.
    Call,
    /// The function returned something that is not a string or `nil`.
    Return,
    /// It succeeded, but its store writes were refused (over the bounds, or
    /// the file could not be written). Nothing was saved.
    Store,
    /// It did not finish within its wall-clock budget. Its effects and store
    /// writes are dropped.
    Budget,
}

impl Stage {
    const fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Load => "load",
            Self::Call => "call",
            Self::Return => "return",
            Self::Store => "store",
            Self::Budget => "budget",
        }
    }
}

/// A failed invocation, naming the extension, the stage and the cause.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{extension} ({script}): {stage} failed: {message}", stage = .stage.name())]
pub struct ExtError {
    pub extension: String,
    pub script: String,
    pub stage: Stage,
    pub message: String,
}

/// What the program sees of arnes while it runs.
#[derive(Debug, Default)]
struct Host {
    conversation: String,
    bindings: String,
    notices: String,
    state: Vec<(String, String)>,
    sessions: String,
    read_roots: Vec<PathBuf>,
    effects: Vec<Effect>,
    store: arnes_store::ext::ExtStore,
    writes: Vec<arnes_store::ext::Write>,
    reach: Option<Reach>,
    reach_calls: u32,
}

impl Host {
    /// A read sees this call's own earlier writes.
    fn stored(&self, key: &str) -> Option<&str> {
        match self.writes.iter().rev().find(|(k, _)| k == key) {
            Some((_, v)) => v.as_deref(),
            None => self.store.get(key),
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
#[blue_lang_host::host(prefix = "arnes_")]
impl Host {
    fn conversation(&self) -> String {
        self.conversation.clone()
    }

    fn bindings(&self) -> String {
        self.bindings.clone()
    }

    fn read_file(&self, path: String, max: u64) -> Option<String> {
        let p = Path::new(&path);
        let small = within_roots(&self.read_roots, p) && std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() <= max);
        small.then(|| std::fs::read_to_string(p).ok()).flatten()
    }

    fn notices(&self) -> String {
        self.notices.clone()
    }

    fn state(&self, key: String) -> Result<String, Refusal> {
        let found = self.state.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone());
        found.ok_or_else(|| unknown("key ", key, self.state.iter().map(|(k, _)| k.as_str())))
    }

    fn set(&mut self, key: String, value: String) -> Result<(), Refusal> {
        if !SETTINGS.iter().any(|(k, _)| *k == key) {
            return Err(unknown("setting ", key, SETTINGS.iter().map(|(k, _)| *k)));
        }
        self.effects.push(Effect::Set { key, value });
        Ok(())
    }

    fn sessions(&self) -> String {
        self.sessions.clone()
    }

    fn resume(&mut self, query: String) -> Result<(), Refusal> {
        let query = named(&query, "names a session: a number or an id prefix")?;
        self.effects.push(arm("resume", &query));
        Ok(())
    }

    fn clear(&mut self) {
        self.effects.push(arm("clear", ""));
    }

    fn exit(&mut self) {
        self.effects.push(arm("exit", ""));
    }

    fn pick(&mut self, kind: String) -> Result<(), Refusal> {
        let kind = one_of(kind, PICKERS)?;
        self.effects.push(arm(&kind, ""));
        Ok(())
    }

    fn route_switch(&mut self, name: String) -> Result<(), Refusal> {
        let name = named(&name, "names a model or route")?;
        self.effects.push(arm("model", &name));
        Ok(())
    }

    fn profile(&mut self, name: String) -> Result<(), Refusal> {
        let name = named(&name, "names a profile, or - for none")?;
        self.effects.push(arm("profile", &name));
        Ok(())
    }

    fn open(&mut self, view: String, args: String) -> Result<(), Refusal> {
        let view = one_of(view, VIEWS)?;
        self.effects.push(arm(&view, &args));
        Ok(())
    }

    fn panel(&mut self, title: String, text: String) {
        self.effects.push(Effect::Panel { title, text });
    }

    fn hint(&mut self, text: String) {
        self.effects.push(Effect::Hint(text));
    }

    fn insert(&mut self, text: String) {
        self.effects.push(Effect::Insert(text));
    }

    fn toast(&mut self, level: String, text: String) -> Result<(), Refusal> {
        let level = Level::parse(&level).ok_or_else(|| unknown("level ", level, Level::ALL.iter().map(|(n, _)| *n)))?;
        self.effects.push(Effect::Toast(level, text));
        Ok(())
    }

    fn store_get(&self, key: String) -> Option<String> {
        self.stored(&key).map(str::to_owned)
    }

    fn store_set(&mut self, key: String, value: String) {
        self.writes.push((key, Some(value)));
    }

    fn store_del(&mut self, key: String) {
        self.writes.push((key, None));
    }

    fn http(&mut self, endpoint: String, body: String) -> Result<String, Refusal> {
        let reach = self.reach.clone().ok_or(Refusal::NoReach)?;
        if self.reach_calls >= MAX_REACH_CALLS {
            return Err(Refusal::OverReach);
        }
        self.reach_calls += 1;
        (reach.0)(&endpoint, &body).map_err(Refusal::Reached)
    }
}

pub const MAX_REACH_CALLS: u32 = 4;

/// Where `script`'s state lives: `ext/<file stem>.json` under the data root.
#[must_use]
pub fn state_path(script: &Script) -> Option<PathBuf> {
    let stem = match script.path.to_str().and_then(|p| p.strip_prefix(bidamas::SCHEME)) {
        Some(package) => package.to_owned(),
        None => script.path.file_stem()?.to_string_lossy().into_owned(),
    };
    arnes_store::Layout::from_env().map(|l| l.ext(&stem))
}

/// Run `script`'s function with string `args`, the conversation text visible
/// to `arnes_conversation()`. `extension` names the caller in errors
/// (`/wc`, `tool shout`, `hook before-submit`).
///
/// # Errors
///
/// [`ExtError`] at the stage that failed. Effects collected before a failure
/// are discarded with it.
pub fn invoke(extension: &str, script: &Script, args: &[&str], conversation: &str) -> Result<Outcome, ExtError> {
    invoke_with_state(extension, script, args, conversation, state_path(script))
}

/// [`invoke`] with a full [`Context`] (state, notices, bindings), for the
/// hosted hooks the kernel calls itself.
///
/// # Errors
/// As [`invoke`].
pub fn invoke_with(extension: &str, script: &Script, args: &[&str], ctx: Context<'_>) -> Result<Outcome, ExtError> {
    let fail = |stage, message: String| ExtError {
        extension: extension.to_owned(),
        script: script.to_string(),
        stage,
        message,
    };
    let Some(package) = script.path.to_str().and_then(|p| p.strip_prefix(bidamas::SCHEME)) else {
        return Err(fail(Stage::Read, "a hosted hook names a bidama (bidama:<package>)".into()));
    };
    bidamas::run_bidama(&fail, package, &script.function, args, ctx, state_path(script), &bidamas::BidamaPath::from_env())
}

/// [`invoke`] with the store file named explicitly (`None`: state lives for
/// this call only).
///
/// # Errors
/// As [`invoke`], plus [`Stage::Store`] when the writes are refused.
pub fn invoke_with_state(
    extension: &str,
    script: &Script,
    args: &[&str],
    conversation: &str,
    state: Option<PathBuf>,
) -> Result<Outcome, ExtError> {
    invoke_in(extension, script, args, Context { conversation, ..Context::default() }, state)
}

fn invoke_in(extension: &str, script: &Script, args: &[&str], ctx: Context<'_>, state: Option<PathBuf>) -> Result<Outcome, ExtError> {
    let fail = |stage, message: String| ExtError {
        extension: extension.to_owned(),
        script: script.to_string(),
        stage,
        message,
    };
    if let Some(package) = script.path.to_str().and_then(|p| p.strip_prefix(bidamas::SCHEME)) {
        return bidamas::run_bidama(&fail, package, &script.function, args, ctx, state, &bidamas::BidamaPath::from_env());
    }
    let path = expand_home(&script.path);
    let text = std::fs::read_to_string(&path).map_err(|e| fail(Stage::Read, format!("{}: {e}", path.display())))?;
    run(&fail, Some(&path), &path.display().to_string(), &text, &script.function, args, ctx, state, &bidamas::BidamaPath::from_env())
}

/// The one load-call-commit path, for a file on disk and an embedded module alike.
#[allow(clippy::too_many_arguments)]
fn run(
    fail: &dyn Fn(Stage, String) -> ExtError,
    path: Option<&Path>,
    label: &str,
    text: &str,
    function: &str,
    args: &[&str],
    ctx: Context<'_>,
    state: Option<PathBuf>,
    loader: &dyn blue_lang_runtime::uses::Loader,
) -> Result<Outcome, ExtError> {
    let mut host = Host {
        conversation: ctx.conversation.to_owned(),
        bindings: ctx.bindings.to_owned(),
        notices: ctx.notices.to_owned(),
        state: ctx.state.to_vec(),
        sessions: ctx.sessions.to_owned(),
        read_roots: ctx.read_roots.to_vec(),
        store: arnes_store::ext::ExtStore::load(state),
        reach: ctx.reach.cloned(),
        ..Host::default()
    };
    let entry = Entry { path, text };
    let mut program: Hosted<Host> =
        blue_lang_runtime::hosted::load_hosted(entry, loader, &mut host, |interp: &mut Interpreter<Host>| {
            <Host as blue_lang_host::Host>::install(interp);
            if let Some(flag) = INTERRUPT.with(|h| h.borrow().clone()) {
                interp.set_interrupt(flag);
            }
        })
        .map_err(|e| fail(Stage::Load, e.to_string()))?;
    if !program.defines(function) {
        return Err(fail(Stage::Call, format!("{label} defines no function named {function}")));
    }
    let args = args.iter().map(|a| a.into_value()).collect();
    let value = program.call(function, args, &mut host).map_err(|e| fail(Stage::Call, e.to_string()))?;
    let returned = match value {
        Value::Str(s) => Returned::Text(s.to_string()),
        Value::Nil => Returned::Nothing,
        other => return Err(fail(Stage::Return, format!("expected a string or nil, got {other:?}"))),
    };
    host.store.commit(&host.writes).map_err(|e| fail(Stage::Store, e.to_string()))?;
    Ok(Outcome { returned, effects: host.effects })
}

thread_local! {
    /// The interrupt of the budgeted call running on this thread, if any: every
    /// interpreter built here stops on it (see [`within_budget`]).
    static INTERRUPT: std::cell::RefCell<Option<Arc<std::sync::atomic::AtomicBool>>> = const { std::cell::RefCell::new(None) };
}

/// The default wall-clock budget for one module call from the UI.
pub const DEFAULT_BUDGET: std::time::Duration = std::time::Duration::from_millis(250);

/// Run `call` on a worker thread and wait at most `budget` for it, so a module
/// that loops cannot freeze the caller. Over budget, the caller gets a
/// [`Stage::Budget`] error naming `extension`, and the program is interrupted:
/// every interpreter the worker builds stops on one flag
/// (`Interpreter::set_interrupt`), set when the budget runs out, so its next
/// evaluation step raises `EvalError::Halted` and the worker ends.
///
/// # Errors
/// [`Stage::Budget`] over budget, or whatever `call` returned.
pub fn within_budget<F>(extension: &str, budget: std::time::Duration, call: F) -> Result<Outcome, ExtError>
where
    F: FnOnce() -> Result<Outcome, ExtError> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_flag = Arc::clone(&flag);
    let spawned = std::thread::Builder::new().name(format!("arnes-blue {extension}")).spawn(move || {
        INTERRUPT.with(|h| *h.borrow_mut() = Some(worker_flag));
        let _ = tx.send(call());
    });
    let over = |message: String| ExtError { extension: extension.to_owned(), script: String::new(), stage: Stage::Budget, message };
    if let Err(e) = spawned {
        return Err(over(format!("could not start a worker: {e}")));
    }
    match rx.recv_timeout(budget) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
            Err(over(format!("did not finish within {} ms; interrupted, its effects are dropped", budget.as_millis())))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(over("the worker ended without a result".into())),
    }
}

/// Ask the policy hooks `scripts`, in order, about `arg` (a JSON document):
/// the first that returns text decides, and `None` means none had an opinion
/// (every hook returned `nil`, or there are none). A policy is a pure
/// question: one that asks for an effect is refused, and each call runs
/// within `budget` (docs/profiles.md: `mcp-probe`, `mcp-lazy`,
/// `choose-profile`, `skills`).
///
/// # Errors
/// The first hook that fails, over budget, or asks for an effect.
#[must_use]
pub fn policy(event: &str, scripts: &[Script], arg: &str, budget: std::time::Duration) -> Option<Result<String, ExtError>> {
    policy_with(event, scripts, arg, budget, None)
}

/// [`policy`], with `arnes_http` reaching the declared endpoints through `reach`.
#[must_use]
pub fn policy_with(event: &str, scripts: &[Script], arg: &str, budget: std::time::Duration, reach: Option<Reach>) -> Option<Result<String, ExtError>> {
    for script in scripts {
        let (s, a, r) = (script.clone(), arg.to_owned(), reach.clone());
        let extension = format!("{event} hook");
        let name = extension.clone();
        let out = within_budget(&extension, budget, move || {
            invoke_in(&name, &s, &[&a], Context { reach: r.as_ref(), ..Context::default() }, state_path(&s))
        });
        match out {
            Err(e) => return Some(Err(e)),
            Ok(o) if !o.effects.is_empty() => {
                return Some(Err(ExtError {
                    extension,
                    script: script.function.clone(),
                    stage: Stage::Call,
                    message: "a policy hook answers and may not ask for effects".into(),
                }));
            }
            Ok(o) if o.returned == Returned::Nothing => {}
            Ok(o) => return Some(Ok(o.text().to_owned())),
        }
    }
    None
}

/// The bidamas arnes hosts: its own distribution (`bidamas/` in this repo),
/// resolved by name through [`BidamaPath`] so a user's bidama of the same name
/// replaces the built one (`docs/kernel.md`).
pub mod bidamas {
    use std::path::PathBuf;

    use blue_lang_runtime::uses::Loader;

    use super::{Context, ExtError, Outcome, Stage};

    /// arnes's distribution, compiled in so the kernel runs its own features
    /// with no files on disk: `(package, [(file, source)])`.
    pub const EMBEDDED: &[(&str, &[(&str, &str)])] = &[
        ("meirei", &[("bidamas/meirei/meirei.b", include_str!("../../bidamas/meirei/meirei.b"))]),
        ("joutai", &[("bidamas/joutai/joutai.b", include_str!("../../bidamas/joutai/joutai.b"))]),
        ("kenban", &[("bidamas/kenban/kenban.b", include_str!("../../bidamas/kenban/kenban.b"))]),
        ("kenkou", &[("bidamas/kenkou/kenkou.b", include_str!("../../bidamas/kenkou/kenkou.b"))]),
        ("sanshou", &[("bidamas/sanshou/sanshou.b", include_str!("../../bidamas/sanshou/sanshou.b"))]),
        ("yushutsu", &[("bidamas/yushutsu/yushutsu.b", include_str!("../../bidamas/yushutsu/yushutsu.b"))]),
        ("wariate", &[("bidamas/wariate/wariate.b", include_str!("../../bidamas/wariate/wariate.b"))]),
    ];

    /// A script path naming a hosted bidama instead of a file:
    /// `bidama:kenkou` runs `use("kenkou")` and then the function.
    pub const SCHEME: &str = "bidama:";

    /// Kernel commands that are the Rust twin of a hosted bidama command:
    /// served only with `:kernel-fallback`, and replaceable by a user command.
    pub const KERNEL_TWINS: &[&str] = &["clear", "exit", "help", "mcp", "mode", "model", "notices", "profile", "resume", "vigy", "vim"];

    /// The bidamas arnes hosts when the config names no others.
    pub const DEFAULT_HOSTED: &[&str] = &["meirei", "joutai", "kenban", "sanshou", "kenkou"];

    /// What a hosted bidama exports.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Kind {
        /// A slash command: may use every command capability.
        Command,
        /// A status segment: returns text and may not ask for effects.
        Segment,
        /// A key binding: `bind <action> <chord>`, data only, no function.
        Bind,
        /// A vigy: `vigy <name> <function> <every-ms>`, ticked by the shared
        /// vigy runtime like a declared one.
        Vigy,
        /// A theme colour: `color <slot> <#RRGGBB>`, data only.
        Color,
        /// A hook: `hook <event> <function> <description>`; the kernel runs
        /// hosted `before-submit` hooks after the declared ones.
        Hook,
    }

    impl Kind {
        const ALL: [(&'static str, Self); 6] = [
            ("command", Self::Command),
            ("segment", Self::Segment),
            ("bind", Self::Bind),
            ("vigy", Self::Vigy),
            ("color", Self::Color),
            ("hook", Self::Hook),
        ];
    }

    /// One export: `/name` or segment `name`, served by `package#function`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Export {
        pub kind: Kind,
        pub name: String,
        pub package: String,
        pub function: String,
        pub description: String,
    }

    /// Read a bidama's `exports()` lines, each `<kind> <name> <function>
    /// <description>`. A bad line is refused alone, named; the rest are kept.
    #[must_use]
    pub fn parse_exports(package: &str, text: &str) -> (Vec<Export>, Vec<String>) {
        let mut ok = Vec::new();
        let mut refused = Vec::new();
        for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            let mut f = line.trim().splitn(4, ' ');
            let (kind, name, function, description) = (f.next(), f.next(), f.next(), f.next().unwrap_or(""));
            let Some(kind) = kind.and_then(|k| Kind::ALL.iter().find(|(n, _)| *n == k).map(|(_, k)| *k)) else {
                refused.push(format!("{package} export {}: the kind is one of command, segment, bind, vigy, color, hook", i + 1));
                continue;
            };
            if kind == Kind::Bind {
                // `bind <action> <chord>`: the chord is the rest of the line,
                // since a two-key chord has a space in it.
                let mut b = line.trim().splitn(3, ' ').skip(1);
                match (b.next(), b.next()) {
                    (Some(action), Some(chord)) if !chord.trim().is_empty() => ok.push(Export {
                        kind,
                        name: chord.trim().to_owned(),
                        package: package.to_owned(),
                        function: action.to_owned(),
                        description: String::new(),
                    }),
                    _ => refused.push(format!("{package} export {}: bind needs an action and a chord", i + 1)),
                }
                continue;
            }
            match (name, function) {
                (Some(name), Some(function)) => ok.push(Export {
                    kind,
                    name: name.to_owned(),
                    package: package.to_owned(),
                    function: function.to_owned(),
                    description: description.to_owned(),
                }),
                _ => refused.push(format!("{package} export {}: needs a name and a function", i + 1)),
            }
        }
        (ok, refused)
    }

    /// Every export of the hosted bidamas, read once at start and on reload.
    /// A name exported twice keeps the first (in hosting order).
    #[derive(Debug, Clone, Default)]
    pub struct Registry {
        exports: Vec<Export>,
    }

    impl Registry {
        /// Ask each of `packages` for its exports through `path`. A bidama that
        /// fails to load or has no `exports` is refused by name and the
        /// others are hosted.
        #[must_use]
        pub fn load(path: &BidamaPath, packages: &[String]) -> (Self, Vec<String>) {
            let mut reg = Self::default();
            let mut refused = Vec::new();
            for package in packages {
                match call(path, package, "exports", &format!("bidama {package}"), &[], Context::default(), None) {
                    Ok(out) => {
                        let (exports, bad) = parse_exports(package, out.text());
                        refused.extend(bad);
                        for e in exports {
                            if reg.get(e.kind, &e.name).is_some() {
                                refused.push(format!("{package} export {}: already hosted", e.name));
                            } else {
                                reg.exports.push(e);
                            }
                        }
                    }
                    Err(e) => refused.push(e.to_string()),
                }
            }
            (reg, refused)
        }

        #[must_use]
        pub fn get(&self, kind: Kind, name: &str) -> Option<&Export> {
            self.exports.iter().find(|e| e.kind == kind && e.name == name)
        }

        #[must_use]
        pub fn owns_command(&self, name: &str) -> bool {
            self.get(Kind::Command, name).is_some()
        }

        /// The hosted hooks on `event` as bidama scripts, in hosting order,
        /// skipping packages named in `off`.
        #[must_use]
        pub fn hooks(&self, event: &str, off: &[String]) -> Vec<super::Script> {
            self.exports
                .iter()
                .filter(|e| e.kind == Kind::Hook && e.name == event && !off.contains(&e.package))
                .map(|e| super::Script::new(format!("{SCHEME}{}", e.package), e.function.clone()))
                .collect()
        }

        /// The hosted theme colours, `(slot, #RRGGBB)`, in hosting order,
        /// skipping packages named in `off`.
        #[must_use]
        pub fn colors(&self, off: &[String]) -> Vec<(String, String)> {
            self.exports
                .iter()
                .filter(|e| e.kind == Kind::Color && !off.contains(&e.package))
                .map(|e| (e.name.clone(), e.function.clone()))
                .collect()
        }

        /// The hosted key bindings, `(chord, action id)`, in hosting order,
        /// skipping packages named in `off`.
        #[must_use]
        pub fn binds(&self, off: &[String]) -> Vec<(String, String)> {
            self.exports
                .iter()
                .filter(|e| e.kind == Kind::Bind && !off.contains(&e.package))
                .map(|e| (e.name.clone(), e.function.clone()))
                .collect()
        }

        /// The hosted vigies as `(name, script, every)`, the script naming its
        /// bidama (`bidama:<package>`), skipping names in `off`. A bad interval
        /// is refused by name.
        #[must_use]
        pub fn vigies(&self, off: &[String]) -> (Vec<(String, super::Script, std::time::Duration)>, Vec<String>) {
            let mut ok = Vec::new();
            let mut refused = Vec::new();
            for e in self.exports.iter().filter(|e| e.kind == Kind::Vigy && !off.contains(&e.name)) {
                match e.description.trim().parse::<u64>() {
                    Ok(ms) if ms >= 100 => ok.push((
                        e.name.clone(),
                        super::Script::new(format!("{SCHEME}{}", e.package), e.function.clone()),
                        std::time::Duration::from_millis(ms),
                    )),
                    _ => refused.push(format!("{} vigy {}: every-ms is a whole number of at least 100", e.package, e.name)),
                }
            }
            (ok, refused)
        }

        /// The hosted commands, `(name, description)`, for the `/` picker.
        #[must_use]
        pub fn commands(&self) -> Vec<(String, String)> {
            self.exports.iter().filter(|e| e.kind == Kind::Command).map(|e| (e.name.clone(), e.description.clone())).collect()
        }

        /// Run the hosted command `name`.
        ///
        /// # Errors
        /// No hosted bidama exports `name`, or [`ExtError`] from the call.
        pub fn command(&self, path: &BidamaPath, name: &str, args: &str, ctx: Context<'_>) -> Result<Outcome, ExtError> {
            let Some(e) = self.get(Kind::Command, name) else {
                return Err(not_served(format!("/{name}"), "command"));
            };
            call(path, &e.package, &e.function, &format!("/{name}"), &[args], ctx, super::state_path(&super::Script::new(format!("{SCHEME}{}", e.package), "")))
        }

        /// Evaluate the hosted segment `name`. A segment is scoped to text: one
        /// that asks for an effect is refused, and nothing it asked for applies.
        ///
        /// # Errors
        /// No hosted bidama exports `name`, [`ExtError`] from the call, or an
        /// effect asked for.
        pub fn segment(&self, path: &BidamaPath, name: &str, arg: &str) -> Result<String, ExtError> {
            let extension = format!("segment {name}");
            let Some(e) = self.get(Kind::Segment, name) else {
                return Err(not_served(extension, "segment"));
            };
            let out = call(path, &e.package, &e.function, &extension, &[arg], Context::default(), None)?;
            if !out.effects.is_empty() {
                return Err(ExtError {
                    extension,
                    script: format!("{}#{}", e.package, e.function),
                    stage: Stage::Call,
                    message: "a segment returns text and may not ask for effects".into(),
                });
            }
            Ok(out.text().to_owned())
        }
    }

    /// Where a `use("name")` resolves, first match wins: the user's bidamas
    /// (`$XDG_CONFIG_HOME/arnes/bidamas`), then `$ARNES_BIDAMAS` (a path list,
    /// what a nix install sets), then the embedded distribution, then
    /// `$BLUE_PATH` (a checkout or private distribution), then blue's standard
    /// distribution compiled in, so a hosted bidama can `use` any of it with
    /// nothing installed.
    pub struct BidamaPath {
        before: blue_lang_pkg::load_path::LoadPath,
        after: blue_lang_pkg::load_path::LoadPath,
    }

    impl BidamaPath {
        #[must_use]
        pub fn from_env() -> Self {
            let mut roots: Vec<PathBuf> = Vec::new();
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
            if let Some(c) = config {
                roots.push(c.join("arnes/bidamas"));
            }
            if let Some(extra) = std::env::var_os("ARNES_BIDAMAS") {
                roots.extend(std::env::split_paths(&extra));
            }
            Self::new(roots, blue_lang_pkg::load_path::Environment::from_env().load_path(None))
        }

        /// Only the compiled-in distribution: no user, nix or `$BLUE_PATH` roots.
        #[must_use]
        pub fn embedded_only() -> Self {
            Self::new(Vec::new(), blue_lang_pkg::load_path::LoadPath::default())
        }

        /// `before` the embedded distribution, `after` it.
        #[must_use]
        pub fn new(before: Vec<PathBuf>, after: blue_lang_pkg::load_path::LoadPath) -> Self {
            Self { before: blue_lang_pkg::load_path::Environment::from_vars(std::ffi::OsStr::new(&before.iter().map(|r| r.to_string_lossy()).collect::<Vec<_>>().join(":")), std::ffi::OsStr::new("")).load_path(None), after }
        }
    }

    impl Loader for BidamaPath {
        fn load(&self, name: &str) -> Result<Vec<(String, String)>, String> {
            if self.before.resolve(name).is_some() {
                return self.before.load(name);
            }
            if let Some((_, files)) = EMBEDDED.iter().find(|(n, _)| *n == name) {
                return Ok(files.iter().map(|(f, t)| ((*f).to_owned(), (*t).to_owned())).collect());
            }
            if self.after.resolve(name).is_some() {
                return self.after.load(name);
            }
            blue_lang_pkg::embedded::Standard
                .load(name)
                .map_err(|e| format!("{e}; arnes also searched its own bidamas, $ARNES_BIDAMAS and $BLUE_PATH"))
        }
    }

    fn call(path: &BidamaPath, package: &str, function: &str, extension: &str, args: &[&str], ctx: Context<'_>, state: Option<PathBuf>) -> Result<Outcome, ExtError> {
        let fail = |stage, message: String| ExtError {
            extension: extension.to_owned(),
            script: format!("{package}#{function}"),
            stage,
            message,
        };
        run_bidama(&fail, package, function, args, ctx, state, path)
    }

    pub(super) fn run_bidama(
        fail: &dyn Fn(Stage, String) -> ExtError,
        package: &str,
        function: &str,
        args: &[&str],
        ctx: Context<'_>,
        state: Option<PathBuf>,
        path: &BidamaPath,
    ) -> Result<Outcome, ExtError> {
        let entry = format!("use(\"{package}\")\n");
        let qualified = if function.contains("::") { function.to_owned() } else { format!("{package}::{function}") };
        super::run(fail, None, package, &entry, &qualified, args, ctx, state, path)
    }

    fn not_served(extension: String, what: &str) -> ExtError {
        ExtError { extension, script: String::new(), stage: Stage::Call, message: format!("no bidama serves this {what}") }
    }

}

/// `~/x` against `$HOME`; anything else unchanged.
#[must_use]
pub fn expand_home(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), std::env::var_os("HOME")) {
        (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => path.to_path_buf(),
    }
}

/// One model-callable tool implemented in blue.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// The input JSON Schema. `None` is an object with no declared properties.
    pub schema: Option<serde_json::Value>,
    /// Whether the tool changes nothing (the permission gate lets read-only
    /// tools run unasked in default mode). Unset is treated as side-effecting.
    pub read_only: bool,
    pub script: Script,
}

/// The configured blue tools, served beside the built-ins and MCP tools.
#[derive(Debug, Clone, Default)]
pub struct Tools {
    specs: Arc<Vec<ToolSpec>>,
}

impl Tools {
    #[must_use]
    pub fn new(specs: Vec<ToolSpec>) -> Self {
        Self { specs: Arc::new(specs) }
    }

    fn get(&self, name: &str) -> Option<&ToolSpec> {
        self.specs.iter().find(|s| s.name == name)
    }

    #[must_use]
    pub fn owns(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    #[must_use]
    pub fn read_only(&self, name: &str) -> Option<bool> {
        self.get(name).map(|s| s.read_only)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// The tool definitions sent to the model, in declaration order.
    #[must_use]
    pub fn tools(&self) -> Vec<Tool> {
        self.specs
            .iter()
            .map(|s| Tool {
                name: s.name.clone(),
                description: s.description.clone(),
                input_schema: s.schema.clone().map_or_else(ToolInputSchema::empty_object, ToolInputSchema),
            })
            .collect()
    }

    /// Run tool `name` with `input` (passed to blue as a JSON string). A tool
    /// runs off the async runtime, since a blue program is synchronous.
    ///
    /// # Errors
    ///
    /// The tool is not one of these, or [`ExtError`] as its text.
    pub async fn call(&self, name: &str, input: &serde_json::Value) -> Result<Outcome, ExtError> {
        let Some(spec) = self.get(name).cloned() else {
            return Err(ExtError {
                extension: format!("tool {name}"),
                script: String::new(),
                stage: Stage::Call,
                message: "no blue tool by that name".into(),
            });
        };
        let json = input.to_string();
        let extension = format!("tool {name}");
        tokio::task::spawn_blocking(move || invoke(&extension, &spec.script, &[&json], ""))
            .await
            .unwrap_or_else(|e| {
                Err(ExtError {
                    extension: format!("tool {name}"),
                    script: String::new(),
                    stage: Stage::Call,
                    message: format!("the blue task did not finish: {e}"),
                })
            })
    }
}

/// When a hook fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    /// Receives the prompt; returns the prompt to send, or `nil` for unchanged.
    BeforeSubmit,
    /// Receives nothing (reads `arnes_conversation()`); its value is ignored.
    AfterTurn,
    /// Receives the tool name and its input JSON; returns `"allow"`, or any
    /// other string as the reason to deny.
    BeforeTool,
}

impl HookEvent {
    pub const ALL: [(&'static str, Self); 3] = [
        ("before-submit", Self::BeforeSubmit),
        ("after-turn", Self::AfterTurn),
        ("before-tool", Self::BeforeTool),
    ];

    /// # Errors
    /// Names every event when `s` is none of them.
    pub fn parse(s: &str) -> Result<Self, String> {
        Self::ALL.iter().find(|(n, _)| *n == s).map(|(_, e)| *e).ok_or_else(|| {
            let legal: Vec<_> = Self::ALL.iter().map(|(n, _)| *n).collect();
            format!("unknown hook event {s:?}; expected one of {legal:?}")
        })
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        Self::ALL.iter().find(|(_, e)| *e == self).map_or("", |(n, _)| n)
    }
}

/// What the `before-tool` hooks decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolVerdict {
    Allow,
    Deny(String),
}

/// The configured hooks, in declaration order per event.
#[derive(Debug, Clone, Default)]
pub struct Hooks {
    hooks: Arc<Vec<(HookEvent, Script)>>,
}

impl Hooks {
    #[must_use]
    pub fn new(hooks: Vec<(HookEvent, Script)>) -> Self {
        Self { hooks: Arc::new(hooks) }
    }

    fn on(&self, event: HookEvent) -> impl Iterator<Item = &Script> {
        self.hooks.iter().filter(move |(e, _)| *e == event).map(|(_, s)| s)
    }

    #[must_use]
    pub fn any(&self, event: HookEvent) -> bool {
        self.on(event).next().is_some()
    }

    /// Run the `before-submit` hooks in order, each seeing the previous one's
    /// prompt. Returns the prompt to send and every effect, in order.
    ///
    /// # Errors
    /// The first hook that fails; nothing is sent then.
    pub fn before_submit(&self, prompt: &str, conversation: &str) -> Result<(String, Vec<Effect>), ExtError> {
        let mut prompt = prompt.to_owned();
        let mut effects = Vec::new();
        for s in self.on(HookEvent::BeforeSubmit) {
            let out = invoke("hook before-submit", s, &[&prompt], conversation)?;
            if let Returned::Text(t) = out.returned {
                prompt = t;
            }
            effects.extend(out.effects);
        }
        Ok((prompt, effects))
    }

    /// Run every `after-turn` hook; one failing does not stop the others.
    #[must_use]
    pub fn after_turn(&self, conversation: &str) -> Vec<Result<Outcome, ExtError>> {
        self.on(HookEvent::AfterTurn).map(|s| invoke("hook after-turn", s, &[], conversation)).collect()
    }

    /// Run the `before-tool` hooks for one call. The first deny wins, and a
    /// hook that fails denies (fail closed), naming itself. Returns the
    /// verdict and the effects the hooks asked for, for the caller to surface.
    pub async fn before_tool(&self, tool: &str, input: &serde_json::Value) -> (ToolVerdict, Vec<Effect>) {
        if !self.any(HookEvent::BeforeTool) {
            return (ToolVerdict::Allow, Vec::new());
        }
        let (hooks, tool, json) = (self.clone(), tool.to_owned(), input.to_string());
        tokio::task::spawn_blocking(move || {
            let mut effects = Vec::new();
            for s in hooks.on(HookEvent::BeforeTool) {
                let out = match invoke("hook before-tool", s, &[&tool, &json], "") {
                    Ok(out) => out,
                    Err(e) => return (ToolVerdict::Deny(format!("{e} (a failing before-tool hook denies)")), effects),
                };
                effects.extend(out.effects);
                match out.returned {
                    Returned::Text(t) if t == "allow" => {}
                    Returned::Text(reason) => return (ToolVerdict::Deny(format!("hook {s} denied {tool}: {reason}")), effects),
                    Returned::Nothing => {
                        let why = format!("hook {s} denied {tool}: it returned nil, not \"allow\"");
                        return (ToolVerdict::Deny(why), effects);
                    }
                }
            }
            (ToolVerdict::Allow, effects)
        })
        .await
        .unwrap_or_else(|e| (ToolVerdict::Deny(format!("before-tool hooks did not finish: {e}")), Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(dir: &tempfile::TempDir, src: &str, function: &str) -> Script {
        let path = dir.path().join("ext.b");
        std::fs::write(&path, src).unwrap();
        Script::new(path, function)
    }

    #[test]
    fn a_policy_is_the_first_text_answer_and_refuses_effects() {
        let budget = std::time::Duration::from_secs(5);
        assert!(policy("x", &[], "{}", budget).is_none());
        let nil = tempfile::tempdir().unwrap();
        let says = tempfile::tempdir().unwrap();
        let loud = tempfile::tempdir().unwrap();
        let nil = script(&nil, "def p(a)\n  nil\nend\n", "p");
        let says = script(&says, "def p(a)\n  json_get(json_parse(a), :n)\nend\n", "p");
        let loud = script(&loud, "def p(a)\n  arnes_toast(\"info\", \"hi\")\n  \"x\"\nend\n", "p");
        assert!(policy("x", std::slice::from_ref(&nil), "{}", budget).is_none());
        assert_eq!(policy("x", &[nil.clone(), says.clone()], r#"{"n":"base"}"#, budget).unwrap().unwrap(), "base");
        let e = policy("x", &[loud, says], r#"{"n":"base"}"#, budget).unwrap().unwrap_err();
        assert!(e.message.contains("may not ask for effects"), "{e}");
    }

    #[test]
    fn a_nil_return_with_no_effects_is_an_empty_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let s = script(&dir, "def run(args)\n  nil\nend\n", "run");
        let out = invoke("/noop", &s, &[""], "").unwrap();
        assert_eq!(out, Outcome { returned: Returned::Nothing, effects: vec![] });
        assert_eq!(out.text(), "");
    }

    #[test]
    fn the_host_api_reads_the_conversation_and_collects_effects_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let src = "def run(args)\n  c = arnes_conversation()\n  arnes_hint(\"saw #{length(c)} chars\")\n  arnes_toast(\"warn\", \"careful\")\n  arnes_insert(\"@#{args}\")\n  \"done\"\nend\n";
        let s = script(&dir, src, "run");
        let out = invoke("/probe", &s, &["notes.md"], "user: hello").unwrap();
        assert_eq!(out.returned, Returned::Text("done".into()));
        assert_eq!(
            out.effects,
            vec![
                Effect::Hint("saw 11 chars".into()),
                Effect::Toast(Level::Warn, "careful".into()),
                Effect::Insert("@notes.md".into()),
            ]
        );
    }

    #[test]
    fn a_tool_parses_its_json_input() {
        let dir = tempfile::tempdir().unwrap();
        let src = "def shout(input)\n  args = json_parse(input)\n  upcase(json_get(args, :text))\nend\n";
        let s = script(&dir, src, "shout");
        let tools = Tools::new(vec![ToolSpec {
            name: "shout".into(),
            description: "upper-case".into(),
            schema: None,
            read_only: true,
            script: s,
        }]);
        assert!(tools.owns("shout") && !tools.owns("Read"));
        assert_eq!(tools.read_only("shout"), Some(true));
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let out = rt.block_on(tools.call("shout", &serde_json::json!({"text": "héllo"}))).unwrap();
        assert_eq!(out.text(), "HÉLLO");
    }

    const APPEND: &str = "def run(args)\n  v = arnes_store_get(\"log\")\n  if v == nil\n    arnes_store_set(\"log\", args)\n  else\n    arnes_store_set(\"log\", \"#{v},#{args}\")\n  end\n  arnes_store_get(\"log\")\nend\n";

    #[test]
    fn store_starts_empty_and_survives_between_calls() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state/ext.json");
        let s = script(&dir, APPEND, "run");
        let first = invoke_with_state("/log", &s, &["a"], "", Some(state.clone())).unwrap();
        assert_eq!(first.text(), "a", "reads see this call's own write");
        let second = invoke_with_state("/log", &s, &["b"], "", Some(state.clone())).unwrap();
        assert_eq!(second.text(), "a,b");
        let on_disk: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
        assert_eq!(on_disk, serde_json::json!({"log": "a,b"}));
    }

    #[test]
    fn a_raising_call_saves_nothing_and_del_forgets() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("ext.json");
        let s = script(&dir, APPEND, "run");
        invoke_with_state("/log", &s, &["a"], "", Some(state.clone())).unwrap();
        let raising = script(&dir, "def run(a)\n  arnes_store_set(\"log\", \"lost\")\n  raise(:nope, \"nope\")\nend\n", "run");
        assert_eq!(invoke_with_state("/log", &raising, &[""], "", Some(state.clone())).unwrap_err().stage, Stage::Call);
        let del = script(&dir, "def run(a)\n  arnes_store_del(\"log\")\n  arnes_store_get(\"log\")\nend\n", "run");
        assert_eq!(invoke_with_state("/log", &del, &[""], "", Some(state.clone())).unwrap().returned, Returned::Nothing);
        assert_eq!(std::fs::read_to_string(&state).unwrap().trim(), "{}");
    }

    #[test]
    fn over_the_bound_is_a_store_failure_and_nothing_is_saved() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("ext.json");
        let big = "x".repeat(arnes_store::ext::MAX_BYTES + 1);
        let s = script(&dir, "def run(a)\n  arnes_store_set(\"k\", a)\n  \"ok\"\nend\n", "run");
        let e = invoke_with_state("/big", &s, &[&big], "", Some(state.clone())).unwrap_err();
        assert_eq!(e.stage, Stage::Store);
        assert!(!state.exists());
        let wrong = script(&dir, "def run(a)\n  arnes_store_set(\"k\", 1)\nend\n", "run");
        assert_eq!(invoke_with_state("/x", &wrong, &[""], "", None).unwrap_err().stage, Stage::Call);
    }

    #[test]
    fn every_failure_is_typed_and_names_its_stage() {
        let dir = tempfile::tempdir().unwrap();
        let missing = Script::new(dir.path().join("absent.b"), "run");
        assert_eq!(invoke("/x", &missing, &[], "").unwrap_err().stage, Stage::Read);

        let parse = script(&dir, "def run(\n", "run");
        assert_eq!(invoke("/x", &parse, &[], "").unwrap_err().stage, Stage::Load);

        let undefined = script(&dir, "def other()\n  nil\nend\n", "run");
        let e = invoke("/x", &undefined, &[], "").unwrap_err();
        assert_eq!(e.stage, Stage::Call);
        assert!(e.to_string().contains("no function named run"), "{e}");

        let raises = script(&dir, "def run(a)\n  raise(:nope, \"nope\")\nend\n", "run");
        assert_eq!(invoke("/x", &raises, &[""], "").unwrap_err().stage, Stage::Call);

        let number = script(&dir, "def run(a)\n  42\nend\n", "run");
        assert_eq!(invoke("/x", &number, &[""], "").unwrap_err().stage, Stage::Return);

        let bad_level = script(&dir, "def run(a)\n  arnes_toast(\"loud\", \"x\")\nend\n", "run");
        let e = invoke("/x", &bad_level, &[""], "").unwrap_err();
        assert!(e.to_string().contains("unknown level"), "{e}");
        assert!(e.to_string().starts_with("/x ("), "names the extension: {e}");
    }

    /// The example in docs/programmability.md, verbatim.
    #[test]
    fn the_documented_wc_command_runs() {
        let dir = tempfile::tempdir().unwrap();
        let src = "def run(args)\n  words = length(split(arnes_conversation(), \" \"))\n  arnes_toast(\"info\", \"#{words} words so far\")\n  \"#{words} words\"\nend\n";
        let s = script(&dir, src, "run");
        let out = invoke("/wc", &s, &[""], "user: hello there").unwrap();
        assert_eq!(out.text(), "3 words");
        assert_eq!(out.effects, vec![Effect::Toast(Level::Info, "3 words so far".into())]);
    }

    const HOOKS: &str = "def tidy(p)\n  trim(p)\nend\n\
def shout(p)\n  upcase(p)\nend\n\
def keep(p)\n  nil\nend\n\
def guard(tool, input)\n  if tool == \"Bash\"\n    \"no shell\"\n  else\n    \"allow\"\n  end\nend\n\
def log()\n  arnes_hint(\"turn done\")\nend\n\
def broken(p)\n  raise(:bad, \"bad hook\")\nend\n";

    fn hooks(dir: &tempfile::TempDir, list: &[(HookEvent, &str)]) -> Hooks {
        let path = dir.path().join("hooks.b");
        std::fs::write(&path, HOOKS).unwrap();
        Hooks::new(list.iter().map(|(e, f)| (*e, Script::new(&path, *f))).collect())
    }

    #[test]
    fn before_submit_chains_and_nil_keeps_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let none = hooks(&dir, &[]);
        assert_eq!(none.before_submit(" hi ", "").unwrap().0, " hi ", "no hooks: unchanged");
        let h = hooks(&dir, &[(HookEvent::BeforeSubmit, "tidy"), (HookEvent::BeforeSubmit, "keep"), (HookEvent::BeforeSubmit, "shout")]);
        assert_eq!(h.before_submit("  make it so  ", "").unwrap().0, "MAKE IT SO");
        let failing = hooks(&dir, &[(HookEvent::BeforeSubmit, "broken")]);
        assert_eq!(failing.before_submit("x", "").unwrap_err().stage, Stage::Call);
    }

    #[test]
    fn after_turn_hooks_run_independently() {
        let dir = tempfile::tempdir().unwrap();
        let h = hooks(&dir, &[(HookEvent::AfterTurn, "broken"), (HookEvent::AfterTurn, "log")]);
        let results = h.after_turn("user: hi");
        assert!(results[0].is_err());
        assert_eq!(results[1].as_ref().unwrap().effects, vec![Effect::Hint("turn done".into())]);
    }

    #[test]
    fn before_tool_allows_denies_and_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).build().unwrap();
        let guard = hooks(&dir, &[(HookEvent::BeforeTool, "guard")]);
        let input = serde_json::json!({"command": "ls"});
        assert_eq!(rt.block_on(guard.before_tool("Read", &input)).0, ToolVerdict::Allow);
        match rt.block_on(guard.before_tool("Bash", &input)).0 {
            ToolVerdict::Deny(why) => assert!(why.contains("no shell"), "{why}"),
            ToolVerdict::Allow => panic!("Bash must be denied"),
        }
        let broken = hooks(&dir, &[(HookEvent::BeforeTool, "broken")]);
        assert!(matches!(rt.block_on(broken.before_tool("Read", &input)).0, ToolVerdict::Deny(_)));
        assert!(HookEvent::parse("before-lunch").unwrap_err().contains("before-tool"));
    }

    #[test]
    fn effects_before_a_raise_are_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let s = script(&dir, "def run(a)\n  arnes_hint(\"half\")\n  raise(:stop, \"stop\")\nend\n", "run");
        assert!(invoke("/x", &s, &[""], "").is_err());
    }

    fn embedded() -> bidamas::BidamaPath {
        bidamas::BidamaPath::new(Vec::new(), blue_lang_pkg::load_path::LoadPath::default())
    }

    fn hosted() -> bidamas::Registry {
        let names: Vec<String> = bidamas::DEFAULT_HOSTED.iter().map(|s| (*s).to_owned()).collect();
        let (reg, refused) = bidamas::Registry::load(&embedded(), &names);
        assert!(refused.is_empty(), "{refused:?}");
        reg
    }

    #[test]
    fn the_default_bidamas_export_the_ported_features() {
        let reg = hosted();
        let names: Vec<_> = reg.commands().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["clear", "exit", "help", "mcp", "mode", "model", "notices", "profile", "resume", "vigy", "vim"]);
        assert_eq!(reg.commands()[0].1, "start a new conversation");
        assert!(reg.get(bidamas::Kind::Segment, "tools").is_some());
        assert!(!reg.owns_command("tools"));
    }

    #[test]
    fn bind_lines_keep_a_two_key_chord_whole() {
        let (ok, refused) = bidamas::parse_exports("k", "bind app:exit ctrl+d\nbind chat:externalEditor ctrl+x ctrl+e\nbind app:exit");
        assert_eq!(ok.iter().map(|e| (e.name.as_str(), e.function.as_str())).collect::<Vec<_>>(), [("ctrl+d", "app:exit"), ("ctrl+x ctrl+e", "chat:externalEditor")]);
        assert_eq!(refused.len(), 1);
        let reg = hosted();
        assert_eq!(reg.binds(&[]).first(), Some(&("ctrl+c".to_owned(), "app:interrupt".to_owned())));
        assert!(reg.binds(&["kenban".to_owned()]).is_empty());
    }

    #[test]
    fn a_bad_export_line_is_refused_alone() {
        let (ok, refused) = bidamas::parse_exports("p", "command a p_a does a\nwidget b p_b\ncommand c\n\nsegment d p_d");
        assert_eq!(ok.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["a", "d"]);
        assert_eq!(refused.len(), 2);
        assert!(bidamas::parse_exports("p", "").0.is_empty());
    }

    #[test]
    fn a_missing_bidama_is_refused_and_the_rest_are_hosted() {
        let names = vec!["nosuch".to_owned(), "meirei".to_owned()];
        let (reg, refused) = bidamas::Registry::load(&embedded(), &names);
        assert_eq!(refused.len(), 1);
        assert!(refused[0].contains("nosuch"));
        assert!(reg.owns_command("clear"));
    }

    #[test]
    fn a_segment_that_asks_for_an_effect_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("seg")).unwrap();
        std::fs::write(
            dir.path().join("seg/seg.b"),
            "def exports()\n  \"segment s s x\"\nend\ndef s(a)\n  arnes_clear()\n  \"x\"\nend\n",
        )
        .unwrap();
        let path = bidamas::BidamaPath::new(vec![dir.path().to_path_buf()], blue_lang_pkg::load_path::LoadPath::default());
        let (reg, refused) = bidamas::Registry::load(&path, &["seg".to_owned()]);
        assert!(refused.is_empty(), "{refused:?}");
        assert!(reg.segment(&path, "s", "").unwrap_err().message.contains("may not ask for effects"));
    }

    fn snapshot() -> Vec<(String, String)> {
        vec![("vim".into(), "off".into()), ("permission-mode".into(), "default".into())]
    }

    #[test]
    fn vim_mode_and_notices_are_meirei_commands_over_state_and_settings() {
        let st = snapshot();
        let ctx = Context { state: &st, notices: "warn: x\ninfo: y", ..Context::default() };
        let v = hosted().command(&embedded(), "vim", "", ctx).unwrap();
        assert_eq!(v.effects, vec![Effect::Set { key: "vim".into(), value: "on".into() }]);
        let on = vec![("vim".to_owned(), "on".to_owned())];
        let off = hosted().command(&embedded(), "vim", "", Context { state: &on, ..Context::default() }).unwrap();
        assert_eq!(off.effects, vec![Effect::Set { key: "vim".into(), value: "off".into() }]);
        assert_eq!(off.text(), "vim editing off");
        let m = hosted().command(&embedded(), "mode", "plan", ctx).unwrap();
        assert_eq!(m.effects, vec![Effect::Set { key: "permission-mode".into(), value: "plan".into() }]);
        let n = hosted().command(&embedded(), "notices", "", ctx).unwrap();
        assert_eq!(n.effects, vec![Effect::Panel { title: "notices".into(), text: "warn: x\ninfo: y".into() }]);
    }

    #[test]
    fn model_mcp_and_vigy_are_meirei_commands_over_the_kernel_views() {
        let ctx = Context::default();
        let c = |n: &str, a: &str| hosted().command(&embedded(), n, a, ctx).unwrap().effects;
        assert_eq!(c("model", ""), vec![arm("model", "")]);
        assert_eq!(c("model", "opus"), vec![arm("model", "opus")]);
        assert_eq!(c("mcp", ""), vec![arm("mcp", "")]);
        assert_eq!(c("vigy", "tick health"), vec![arm("vigy", "tick health")]);
        assert_ne!(c("model", "opus"), vec![arm("model", "")]);
    }

    #[test]
    fn pickers_and_views_outside_their_domain_are_typed_errors() {
        let dir = tempfile::tempdir().unwrap();
        let a = script(&dir, "def run(a)\n  arnes_pick(\"colour\")\nend\n", "run");
        assert!(run_with(&a, Context::default()).unwrap_err().message.contains("expected one of"));
        let b = script(&dir, "def run(a)\n  arnes_open(\"files\", \"\")\nend\n", "run");
        assert!(run_with(&b, Context::default()).unwrap_err().message.contains("\"mcp\""));
        let c = script(&dir, "def run(a)\n  arnes_route_switch(\" \")\nend\n", "run");
        assert!(run_with(&c, Context::default()).is_err());
    }

    #[test]
    fn unknown_state_keys_and_settings_are_typed_errors() {
        let dir = tempfile::tempdir().unwrap();
        let st = snapshot();
        let a = script(&dir, "def run(a)\n  arnes_state(\"nope\")\nend\n", "run");
        let e = run_with(&a, Context { state: &st, ..Context::default() }).unwrap_err();
        assert!(e.message.contains("unknown key") && e.message.contains("permission-mode"), "{e}");
        let b = script(&dir, "def run(a)\n  arnes_set(\"colour\", \"red\")\nend\n", "run");
        assert!(run_with(&b, Context::default()).unwrap_err().message.contains("unknown setting"));
    }

    fn run_with(s: &Script, ctx: Context<'_>) -> Result<Outcome, ExtError> {
        let fail = |stage, message: String| ExtError { extension: "t".into(), script: s.to_string(), stage, message };
        let text = std::fs::read_to_string(&s.path).unwrap();
        run(&fail, Some(&s.path), "t", &text, &s.function, &[""], ctx, None, &embedded())
    }

    #[test]
    fn a_call_within_budget_returns_its_outcome() {
        // The registry is loaded once, as the kernel does; the budget bounds the call.
        let reg = hosted();
        let r = reg.clone();
        let out = within_budget("/ok", DEFAULT_BUDGET, move || r.command(&embedded(), "clear", "", Context::default())).unwrap();
        assert_eq!(out.effects, vec![arm("clear", "")]);
        let err = within_budget("/bad", DEFAULT_BUDGET, move || reg.command(&embedded(), "nope", "", Context::default())).unwrap_err();
        assert_eq!(err.stage, Stage::Call);
    }

    #[test]
    fn arnes_http_reaches_only_through_a_reach_and_at_most_four_times() {
        let dir = tempfile::tempdir().unwrap();
        let budget = std::time::Duration::from_secs(5);
        let ask = script(&dir, "def run(a)\n  arnes_http(\"judge\", a)\nend\n", "run");
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        let reach = Reach(Arc::new(move |e: &str, b: &str| {
            log.lock().unwrap().push(format!("{e} {b}"));
            Ok(format!("{{\"status\":200,\"body\":\"{b}\"}}"))
        }));
        let out = policy_with("choose-route", std::slice::from_ref(&ask), "q", budget, Some(reach.clone())).unwrap().unwrap();
        assert_eq!(out, r#"{"status":200,"body":"q"}"#);
        assert_eq!(*seen.lock().unwrap(), ["judge q"]);
        let refused = policy("choose-route", std::slice::from_ref(&ask), "q", budget).unwrap().unwrap_err();
        assert!(refused.message.contains("only to choose-route hooks"), "{refused}");
        let greedy = script(&dir, "def run(a)\n  arnes_http(\"j\", a)\n  arnes_http(\"j\", a)\n  arnes_http(\"j\", a)\n  arnes_http(\"j\", a)\n  arnes_http(\"j\", a)\nend\n", "run");
        let e = policy_with("choose-route", std::slice::from_ref(&greedy), "q", budget, Some(reach)).unwrap().unwrap_err();
        assert!(e.message.contains("at most 4"), "{e}");
    }

    #[test]
    fn a_looping_program_is_cut_off_at_its_budget() {
        let dir = tempfile::tempdir().unwrap();
        let s = script(&dir, "def spin(a)\n  spin(a)\nend\ndef run(a)\n  arnes_hint(\"never\")\n  spin(a)\nend\n", "run");
        let started = std::time::Instant::now();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let err = within_budget("/spin", std::time::Duration::from_millis(100), move || {
            let r = invoke("/spin", &s, &[""], "");
            let _ = done_tx.send(r.clone());
            r
        })
        .unwrap_err();
        assert_eq!(err.stage, Stage::Budget);
        assert!(started.elapsed() < std::time::Duration::from_millis(1000));
        let ended = done_rx.recv_timeout(std::time::Duration::from_secs(2)).expect("the interrupted worker ends");
        assert!(ended.unwrap_err().message.contains("halted"));
    }

    #[test]
    fn std_clear_and_exit_ask_the_kernel_and_nothing_else() {
        let c = hosted().command(&embedded(), "clear", "", Context::default()).unwrap();
        assert_eq!(c.effects, vec![arm("clear", "")]);
        assert_eq!(c.text(), "new conversation");
        let e = hosted().command(&embedded(), "exit", "", Context::default()).unwrap();
        assert_eq!((e.effects, e.returned), (vec![arm("exit", "")], Returned::Nothing));
    }

    #[test]
    fn std_help_is_a_panel_of_exactly_the_bindings() {
        let ctx = Context { bindings: "Enter  send\nCtrl-C  interrupt", ..Context::default() };
        let h = hosted().command(&embedded(), "help", "", ctx).unwrap();
        assert_eq!(h.effects, vec![Effect::Panel { title: "help".into(), text: ctx.bindings.into() }]);
        let empty = hosted().command(&embedded(), "help", "", Context::default()).unwrap();
        assert_eq!(empty.effects, vec![Effect::Panel { title: "help".into(), text: String::new() }]);
    }

    #[test]
    fn std_tools_segment_matches_the_kernel_format() {
        for n in [0_usize, 1, 42] {
            assert_eq!(hosted().segment(&embedded(), "tools", &n.to_string()).unwrap(), format!(" \u{b7} {n} tools"));
        }
    }

    #[test]
    fn a_user_bidama_of_the_same_name_replaces_the_built_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("meirei")).unwrap();
        std::fs::write(dir.path().join("meirei/meirei.b"), "def clear(args)\n  \"mine\"\nend\n").unwrap();
        let empty = blue_lang_pkg::load_path::LoadPath::default();
        let mine = bidamas::BidamaPath::new(vec![dir.path().to_path_buf()], empty);
        let out = hosted().command(&mine, "clear", "", Context::default()).unwrap();
        assert_eq!(out.text(), "mine");
        assert!(out.effects.is_empty());
        let other = tempfile::tempdir().unwrap();
        let built = bidamas::BidamaPath::new(vec![other.path().to_path_buf()], blue_lang_pkg::load_path::LoadPath::default());
        assert_eq!(hosted().command(&built, "clear", "", Context::default()).unwrap().effects, vec![arm("clear", "")]);
    }

    #[test]
    fn hosted_bidamas_resolve_blues_standard_distribution_with_nothing_installed() {
        assert!(!blue_lang_pkg::embedded::Standard.is_empty());
        let path = bidamas::BidamaPath::embedded_only();
        assert!(blue_lang_runtime::uses::Loader::load(&path, "ronri").is_ok());
        assert!(blue_lang_runtime::uses::Loader::load(&path, "no-such-bidama").is_err());
        assert!(!hosted().commands().is_empty());
    }

    #[test]
    fn a_hosted_vigy_is_a_bidama_script_that_runs_by_name() {
        let (reg, refused) = bidamas::Registry::load(&embedded(), &["kenkou".to_owned()]);
        assert!(refused.is_empty(), "{refused:?}");
        let (v, bad) = reg.vigies(&[]);
        assert!(bad.is_empty());
        assert_eq!(v.len(), 1);
        assert_eq!((v[0].0.as_str(), v[0].2), ("mcp-health", std::time::Duration::from_secs(30)));
        let out = invoke("vigy", &v[0].1, &["[]"], "").unwrap();
        assert_eq!(out.text(), "no MCP servers");
        assert!(reg.vigies(&["mcp-health".to_owned()]).0.is_empty());
        let (_, slow) = bidamas::parse_exports("p", "vigy x p_x 5");
        let r = bidamas::Registry::default();
        assert!(r.vigies(&[]).0.is_empty() && slow.is_empty());
    }

    #[test]
    fn a_theme_bidama_exports_color_rows() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("umi")).unwrap();
        std::fs::write(
            dir.path().join("umi/umi.b"),
            "use(\"yushutsu\", [:render])\ndef exports()\n  render([[\"color\", \"accent\", \"#00aaff\"], [\"color\", \"border\", \"#333333\"]])\nend\n",
        )
        .unwrap();
        let path = bidamas::BidamaPath::new(vec![dir.path().to_path_buf()], blue_lang_pkg::load_path::LoadPath::default());
        let (reg, refused) = bidamas::Registry::load(&path, &["umi".to_owned()]);
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(reg.colors(&[]), [("accent".to_owned(), "#00aaff".to_owned()), ("border".to_owned(), "#333333".to_owned())]);
        assert!(reg.colors(&["umi".to_owned()]).is_empty());
    }

    #[test]
    fn the_host_surface_is_exactly_the_arnes_primitives_and_their_arities() {
        let mut surface: Vec<_> = <Host as blue_lang_host::Host>::SURFACE.iter().map(|f| (f.name, f.params.len())).collect();
        surface.sort_unstable();
        let mut registered = vec![
            ("arnes_bindings", 0),
            ("arnes_clear", 0),
            ("arnes_conversation", 0),
            ("arnes_exit", 0),
            ("arnes_hint", 1),
            ("arnes_http", 2),
            ("arnes_insert", 1),
            ("arnes_notices", 0),
            ("arnes_open", 2),
            ("arnes_panel", 2),
            ("arnes_pick", 1),
            ("arnes_profile", 1),
            ("arnes_read_file", 2),
            ("arnes_resume", 1),
            ("arnes_route_switch", 1),
            ("arnes_sessions", 0),
            ("arnes_set", 2),
            ("arnes_state", 1),
            ("arnes_store_del", 1),
            ("arnes_store_get", 1),
            ("arnes_store_set", 2),
            ("arnes_toast", 2),
        ];
        registered.sort_unstable();
        assert_eq!(surface, registered);
    }

    #[test]
    fn std_refuses_names_it_does_not_own() {
        assert!(!hosted().owns_command("nosuch"));
        assert_eq!(hosted().command(&embedded(), "nosuch", "", Context::default()).unwrap_err().stage, Stage::Call);
        assert!(hosted().segment(&embedded(), "spend", "").is_err());
        assert!(bidamas::EMBEDDED.iter().all(|(n, f)| f.iter().all(|(p, _)| p.contains(n))));
    }
}
