//! The input reducer — Claude Code's prompt semantics as a pure state machine.
//!
//! `apply(action, busy, now)` holds every behaviour in docs/tui-parity.md
//! §4.1–4.3 and performs no I/O: it edits the buffer and returns the
//! [`Effect`]s the runtime must carry out (submit, interrupt, exit, …).
//! Time is a parameter, so the double-press windows are testable without
//! sleeping, and every row of the parity matrix is a plain unit test.

use std::time::{Duration, Instant};

use arnes_config::HistorySearchStyle;
use egaku::{Feed, FuzzyPicker, NavAction, NavKey, NavKeymap, NavResolver, PickerEffect, PickerEvent, PickerItem, TextArea};
use wadachi_spec::usage::RankOptions;

use crate::keymap::Action;
use crate::vim::{VimInput, VimOutcome};

/// Second `Ctrl-C` / `Ctrl-D` / `Esc` within this window counts as a
/// double press (Claude Code documents 800 ms for `Ctrl-D`).
pub const DOUBLE_PRESS: Duration = Duration::from_millis(800);

/// A paste larger than this collapses into a `[Pasted text #N …]` chip.
/// Claude Code documents "~1 KB"; the exact byte count is unconfirmed.
pub const PASTE_CHIP_BYTES: usize = 1024;

/// The built-in slash commands, `(name, what it does)` — the `/` picker's
/// catalogue and the single source the runtime dispatches on.
pub const COMMANDS: &[(&str, &str)] = &[
    ("clear", "start a new conversation"),
    ("exit", "quit arnes"),
    ("help", "show keyboard shortcuts"),
    ("mcp", "browse MCP servers, tools and schemas"),
    ("model", "switch model or provider route"),
    ("mode", "permission mode: default | plan | bypass"),
    ("notices", "review this session's notices"),
    ("profile", "switch profile: MCP servers, skills, context, access"),
    ("resume", "list saved sessions; /resume <n|id> switches"),
    ("vigy", "each vigy's last result; /vigy tick <name>"),
    ("vim", "toggle vim editing"),
];

/// Commands that take an argument: picking one fills the prompt instead
/// of running it.
const COMMANDS_WITH_ARGS: &[&str] = &["mode"];

/// Which picker is open over the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    /// `@` — files under the working directory.
    Mention,
    /// `/` — slash commands.
    Command,
    /// `/model` — the provider's models and the configured routes.
    Model,
    /// A list whose accepted key runs a command with it (`/profile`).
    Choice(&'static str),
    /// `Ctrl-R` — the prompt history, ranked; accepting one replaces the
    /// prompt with it for review.
    History,
}

/// A picker open over the prompt: egaku's `FuzzyPicker` (filtering and
/// selection) plus what accepting means here.
#[derive(Debug, Clone)]
pub struct PromptPicker {
    pub kind: PickerKind,
    pub picker: FuzzyPicker<String>,
    /// The picker's navigation keys: egaku's filter set, so typed letters
    /// filter and `Ctrl-j`/`Ctrl-k`, `Ctrl-n`/`Ctrl-p`, arrows and Tab move.
    pub nav: NavResolver,
}

impl PromptPicker {
    fn new(kind: PickerKind, picker: FuzzyPicker<String>, keymap: Option<&NavKeymap>) -> Self {
        Self { kind, picker, nav: NavResolver::new(keymap.cloned().unwrap_or_else(NavKeymap::filter)) }
    }
}

/// What the runtime must do after an input step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Send this prompt (paste chips already expanded).
    Submit(String),
    /// Stop the running turn, keeping what was already received.
    Interrupt,
    Exit,
    /// Force a full repaint.
    Redraw,
    /// Edit the current prompt in `$VISUAL` / `$EDITOR`.
    OpenEditor,
    ToggleTranscript,
    CycleEffort,
    CycleMode,
    /// `@` was typed: the runtime lists files and calls
    /// [`InputState::open_mention_picker`] (I/O stays out of the reducer).
    OpenMentionPicker,
    /// A slash command: `/mode plan` → `{ name: "mode", args: "plan" }`.
    Command { name: String, args: String },
    /// Leave the live tail and scroll back through the conversation.
    Scrollback,
}

/// `Ctrl-R` incremental reverse search over history.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistorySearch {
    pub query: String,
    /// Index into history of the current match.
    pub hit: Option<usize>,
}

/// One use of a prompt, as the `Ctrl-R` picker ranks it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Use {
    pub text: String,
    /// Unix seconds, when known.
    pub at: Option<i64>,
    /// In the first tier: this directory's under `project-then-global`,
    /// every use otherwise.
    pub near: bool,
}

/// The `Ctrl-R` feed, best first: the fleet's history ranking
/// (`wadachi_spec::usage`, the one skim-tab's shell `Ctrl-R` uses) over the
/// near tier, then over the rest, each distinct prompt once.
#[must_use]
pub fn history_feed(uses: &[Use], opts: RankOptions, now_unix: i64) -> Vec<String> {
    use wadachi_spec::usage::{accumulate, rank};
    let tier = |near: bool| rank(accumulate(uses.iter().filter(|u| u.near == near).map(|u| (u.at, u.text.as_str()))), opts, now_unix);
    let mut feed = tier(true);
    let mut seen: std::collections::HashSet<String> = feed.iter().cloned().collect();
    feed.extend(tier(false).into_iter().filter(|t| seen.insert(t.clone())));
    feed
}

/// A prompt as one picker row: its lines joined by ` ⏎ `.
fn one_line(text: &str) -> String {
    text.split('\n').collect::<Vec<_>>().join(" \u{23ce} ")
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The prompt: an editable buffer plus Claude Code's surrounding state.
#[derive(Debug, Clone, Default)]
pub struct InputState {
    pub editor: TextArea,
    /// What `Up`/`Down` walk, oldest first (consecutive repeats once).
    history: Vec<String>,
    /// Every use, oldest first, with its time: what `Ctrl-R` ranks.
    uses: Vec<Use>,
    /// Prompts added since the runtime last took them, to be written to disk.
    recorded: Vec<String>,
    /// What `Ctrl-R` opens (`history.search`).
    pub history_search: HistorySearchStyle,
    /// How the `Ctrl-R` picker orders the feed (`history.ranking`, `history.pin-recent`).
    pub history_rank: RankOptions,
    history_pos: Option<usize>,
    /// What was being typed before walking history, restored on the way back.
    draft: Option<String>,
    stash: Option<String>,
    /// Paste bodies, indexed by chip number − 1.
    pastes: Vec<String>,
    armed_exit: Option<(Action, Instant)>,
    last_escape: Option<Instant>,
    /// Prompts typed while a turn ran; sent in order as turns finish.
    pub queued: Vec<String>,
    /// A one-line hint for the status area ("Press Ctrl-C again to exit").
    pub hint: Option<String>,
    pub search: Option<HistorySearch>,
    pub show_help: bool,
    /// `editor-mode = "vim"`: escriba's vim over this prompt.
    pub vim: Option<VimInput>,
    /// The `@` / `/` picker, when open.
    pub picker: Option<PromptPicker>,
    /// Commands declared in the config and implemented in blue, `(name,
    /// description)`, listed after the built-ins and dispatched the same way.
    pub user_commands: Vec<(String, String)>,
    /// The pickers' navigation keymap (`keys.nav.filter`); unset, egaku's.
    pub nav_filter: Option<NavKeymap>,
}

impl InputState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Replace the history with prompts read from disk, oldest first (the
    /// last one is what `Up` recalls first). `tiered`: the `Ctrl-R` picker
    /// ranks this directory's prompts above the rest.
    pub fn load_history(&mut self, recalled: Vec<arnes_store::history::Recalled>, tiered: bool) {
        self.history.clear();
        self.uses.clear();
        for r in recalled {
            if self.history.last() != Some(&r.entry.text) {
                self.history.push(r.entry.text.clone());
            }
            self.uses.push(Use { text: r.entry.text, at: Some(r.entry.at), near: !tiered || r.here });
        }
        self.history_pos = None;
        self.draft = None;
    }

    /// Prompts added to the history since the last call, oldest first: the
    /// runtime appends them to the history file (the reducer does no I/O).
    pub fn take_recorded(&mut self) -> Vec<String> {
        std::mem::take(&mut self.recorded)
    }

    /// Open the `Ctrl-R` picker over the ranked history, the typed prompt as
    /// its query when it is one line. An empty history says so instead.
    pub fn open_history_picker(&mut self, now_unix: i64) {
        let feed = history_feed(&self.uses, self.history_rank, now_unix);
        if feed.is_empty() {
            self.hint = Some("no prompt history yet".into());
            return;
        }
        let n = feed.len();
        #[allow(clippy::cast_precision_loss)]
        let items = feed.into_iter().enumerate().map(|(i, t)| PickerItem::with_score(t.clone(), one_line(&t), (n - i) as f64)).collect();
        let mut picker = FuzzyPicker::new(items);
        picker.on_event(PickerEvent::Open);
        if self.editor.row_count() == 1 {
            for c in self.editor.text().chars() {
                picker.on_event(PickerEvent::Type(c));
            }
        }
        self.picker = Some(PromptPicker::new(PickerKind::History, picker, self.nav_filter.as_ref()));
    }

    /// Apply one bound action.
    pub fn apply(&mut self, action: Action, busy: bool, now: Instant) -> Vec<Effect> {
        if !matches!(action, Action::Interrupt | Action::Exit) {
            self.armed_exit = None;
        }
        if action != Action::Cancel {
            self.last_escape = None;
        }
        if action != Action::Interrupt && action != Action::Exit {
            self.hint = None;
        }
        if self.search.is_some()
            && let Some(effects) = self.apply_in_search(action) {
                return effects;
            }
        if self.picker.is_some()
            && let Some(effects) = self.apply_in_picker(action) {
                return effects;
            }
        match action {
            Action::Interrupt => self.interrupt(busy, now),
            Action::Exit => {
                if self.editor.is_empty() {
                    self.double_press(Action::Exit, now, "Press Ctrl-D again to exit")
                } else {
                    self.editor.delete_forward();
                    vec![]
                }
            }
            Action::Cancel => self.cancel(busy, now),
            Action::Submit => self.submit(busy),
            Action::Newline => {
                self.editor.insert_newline();
                vec![]
            }
            Action::HistoryPrevious => {
                self.history_previous();
                vec![]
            }
            Action::HistoryNext => {
                self.history_next();
                vec![]
            }
            Action::HistorySearch => {
                match self.history_search {
                    HistorySearchStyle::Picker => self.open_history_picker(unix_now()),
                    HistorySearchStyle::Incremental => self.search = Some(HistorySearch::default()),
                }
                vec![]
            }
            Action::Stash => {
                if !self.editor.is_empty() {
                    self.stash = Some(self.editor.text());
                    self.editor.clear();
                    self.hint = Some("Stashed — Ctrl-S restores it".into());
                } else if let Some(s) = self.stash.take() {
                    self.editor.set_text(&s);
                }
                vec![]
            }
            Action::Redraw => vec![Effect::Redraw],
            Action::ExternalEditor => vec![Effect::OpenEditor],
            Action::ToggleTranscript => vec![Effect::ToggleTranscript],
            Action::CycleEffort => vec![Effect::CycleEffort],
            Action::CycleMode => vec![Effect::CycleMode],
            Action::Scrollback => vec![Effect::Scrollback],
            edit => {
                self.edit(edit);
                vec![]
            }
        }
    }

    /// Turn vim editing on, starting in Insert (as Claude Code's vim mode does).
    pub fn enable_vim(&mut self) {
        let mut v = VimInput::new();
        v.host.modal.enter_insert();
        self.vim = Some(v);
    }

    #[must_use]
    pub fn vim_mode(&self) -> Option<escriba_core::Mode> {
        self.vim.as_ref().map(VimInput::mode)
    }

    /// One key in a vim Normal / Command context, through escriba's
    /// pipeline; its outcomes become the same effects the Claude Code keys
    /// produce, so submit / history / exit behave identically either way.
    pub fn vim_key(&mut self, key: &escriba_keymap::Key, busy: bool) -> Vec<Effect> {
        self.hint = None;
        let Some(vim) = self.vim.as_mut() else { return vec![] };
        let outcomes = vim.key(&mut self.editor, key);
        let mut effects = Vec::new();
        for o in outcomes {
            match o {
                VimOutcome::None => {}
                VimOutcome::HistoryPrevious => self.history_previous(),
                VimOutcome::HistoryNext => self.history_next(),
                VimOutcome::Submit => {
                    effects.extend(self.submit(busy));
                    if let Some(v) = self.vim.as_mut() {
                        v.host.modal.enter_insert();
                    }
                }
                VimOutcome::Quit => effects.push(Effect::Exit),
                VimOutcome::OpenEditor => effects.push(Effect::OpenEditor),
                VimOutcome::Unsupported(m) => self.hint = Some(m),
            }
        }
        effects
    }

    /// Open the `@` picker over `files` (paths relative to the cwd).
    pub fn open_mention_picker(&mut self, files: Vec<String>) {
        let items = files.into_iter().map(|f| PickerItem::new(f.clone(), f)).collect();
        let mut picker = FuzzyPicker::new(items);
        picker.on_event(PickerEvent::Open);
        self.picker = Some(PromptPicker::new(PickerKind::Mention, picker, self.nav_filter.as_ref()));
    }

    /// Open the `/model` picker over `(key, label)` pairs; accepting one
    /// runs `/model <key>`.
    pub fn open_model_picker(&mut self, items: Vec<(String, String)>) {
        let items = items.into_iter().map(|(k, l)| PickerItem::new(k, l)).collect();
        let mut picker = FuzzyPicker::new(items);
        picker.on_event(PickerEvent::Open);
        self.picker = Some(PromptPicker::new(PickerKind::Model, picker, self.nav_filter.as_ref()));
    }

    /// A picker over `items` (`(key, line)`); accepting one runs `/command
    /// <key>` (`/profile`), on the same navigation as every other list.
    pub fn open_choice_picker(&mut self, command: &'static str, items: Vec<(String, String)>) {
        let items = items.into_iter().map(|(k, l)| PickerItem::new(k, l)).collect();
        let mut picker = FuzzyPicker::new(items);
        picker.on_event(PickerEvent::Open);
        self.picker = Some(PromptPicker::new(PickerKind::Choice(command), picker, self.nav_filter.as_ref()));
    }

    fn open_command_picker(&mut self) {
        // A hosted or user command of a built-in's name replaces its row.
        let builtins = COMMANDS
            .iter()
            .filter(|(n, _)| !self.user_commands.iter().any(|(u, _)| u == n))
            .map(|(n, w)| ((*n).to_owned(), (*w).to_owned()));
        let items = builtins
            .chain(self.user_commands.iter().cloned())
            .map(|(name, what)| PickerItem::new(name.clone(), format!("/{name:<8} {what}")))
            .collect();
        let mut picker = FuzzyPicker::new(items);
        picker.on_event(PickerEvent::Open);
        self.picker = Some(PromptPicker::new(PickerKind::Command, picker, self.nav_filter.as_ref()));
    }

    /// A key while a picker is open, read by the picker's navigation keys
    /// first. `None` when it is not one (typed text, Backspace): the caller
    /// carries on with the prompt's own handling.
    pub fn picker_nav_key(&mut self, key: NavKey) -> Option<Vec<Effect>> {
        let pp = self.picker.as_mut()?;
        let (action, count) = match pp.nav.feed(key) {
            Feed::Unbound => return None,
            Feed::Pending => return Some(vec![]),
            Feed::Command { action, count } => (action, count.unwrap_or(1)),
        };
        let view = pp.picker.view();
        let (len, at) = (view.rows.len(), view.selected);
        let page = 5;
        // The picker wraps one row at a time; longer moves clamp at the ends.
        let (event, times) = match action {
            NavAction::Down | NavAction::SearchNext => (PickerEvent::NavDown, count),
            NavAction::Up | NavAction::SearchPrev => (PickerEvent::NavUp, count),
            NavAction::HalfPageDown | NavAction::PageDown => (PickerEvent::NavDown, (page * count).min(len.saturating_sub(1).saturating_sub(at))),
            NavAction::HalfPageUp | NavAction::PageUp => (PickerEvent::NavUp, (page * count).min(at)),
            NavAction::Top => (PickerEvent::NavUp, at),
            NavAction::Bottom => (PickerEvent::NavDown, len.saturating_sub(1).saturating_sub(at)),
            NavAction::Open => return Some(self.apply_in_picker(Action::Submit).unwrap_or_default()),
            NavAction::Dismiss | NavAction::Close | NavAction::Back => {
                self.picker = None;
                return Some(vec![]);
            }
            _ => return Some(vec![]),
        };
        for _ in 0..times {
            pp.picker.on_event(event.clone());
        }
        Some(vec![])
    }

    /// Keys while a picker is open. `None` = not a picker key: close it and
    /// let the key act normally.
    fn apply_in_picker(&mut self, action: Action) -> Option<Vec<Effect>> {
        let pp = self.picker.as_mut()?;
        let event = match action {
            Action::HistoryPrevious => PickerEvent::NavUp,
            Action::HistoryNext => PickerEvent::NavDown,
            // `Ctrl-R` again inside the history picker: the next match.
            Action::HistorySearch if pp.kind == PickerKind::History => PickerEvent::NavDown,
            Action::Submit => PickerEvent::Accept,
            Action::Cancel | Action::Interrupt => PickerEvent::Cancel,
            Action::DeleteBack if pp.picker.query().is_empty() => PickerEvent::Cancel,
            Action::DeleteBack => PickerEvent::Backspace,
            _ => {
                self.picker = None;
                return None;
            }
        };
        let kind = pp.kind;
        // Text typed after the name (`/mode plan`) is the command's
        // argument; the fuzzy filter still matched on the whole query.
        let args = pp.picker.query().split_once(char::is_whitespace).map(|(_, a)| a.trim().to_owned());
        let effects = pp.picker.on_event(event);
        let mut out = Vec::new();
        for e in effects {
            match e {
                PickerEffect::Accepted { key } => {
                    self.picker = None;
                    out.extend(self.accept_pick(kind, &key, args.as_deref().unwrap_or("")));
                }
                PickerEffect::Cancelled => self.picker = None,
                _ => {}
            }
        }
        Some(out)
    }

    fn accept_pick(&mut self, kind: PickerKind, key: &str, args: &str) -> Vec<Effect> {
        match kind {
            PickerKind::Mention => {
                // Replace the `@` that opened the picker with `@path `.
                self.editor.delete_back();
                self.editor.insert_str(&format!("@{key} "));
                vec![]
            }
            PickerKind::Model => {
                self.editor.clear();
                vec![Effect::Command { name: "model".to_owned(), args: key.to_owned() }]
            }
            PickerKind::Choice(command) => {
                self.editor.clear();
                vec![Effect::Command { name: command.to_owned(), args: key.to_owned() }]
            }
            PickerKind::History => {
                // Replaces the prompt for review; Enter sends it.
                self.editor.set_text(key);
                self.history_pos = None;
                self.draft = None;
                vec![]
            }
            PickerKind::Command => {
                self.editor.clear();
                if COMMANDS_WITH_ARGS.contains(&key) && args.is_empty() {
                    self.editor.insert_str(&format!("/{key} "));
                    vec![]
                } else {
                    let line = if args.is_empty() { format!("/{key}") } else { format!("/{key} {args}") };
                    self.push_history(line);
                    vec![Effect::Command { name: key.to_owned(), args: args.to_owned() }]
                }
            }
        }
    }

    /// `/name args` → a command effect, when `name` is a built-in or a
    /// declared blue command.
    fn as_command(&self, text: &str) -> Option<Effect> {
        let rest = text.strip_prefix('/')?;
        let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let known = COMMANDS.iter().any(|(n, _)| *n == name) || self.user_commands.iter().any(|(n, _)| n == name);
        known.then(|| Effect::Command { name: name.to_owned(), args: args.trim().to_owned() })
    }

    /// Insert a typed character (no Ctrl/Alt). `?` on an empty prompt
    /// toggles the shortcut help, as in Claude Code.
    pub fn type_char(&mut self, c: char) -> Vec<Effect> {
        if let Some(pp) = self.picker.as_mut() {
            pp.picker.on_event(PickerEvent::Type(c));
            return vec![];
        }
        self.armed_exit = None;
        self.last_escape = None;
        self.hint = None;
        if let Some(s) = &mut self.search {
            s.query.push(c);
            let hit = self.find_older(None);
            if let Some(s) = &mut self.search {
                s.hit = hit;
            }
            return vec![];
        }
        if c == '?' && self.editor.is_empty() {
            self.show_help = !self.show_help;
            return vec![];
        }
        self.show_help = false;
        if c == '/' && self.editor.is_empty() {
            self.editor.insert_char(c);
            self.open_command_picker();
            return vec![];
        }
        // `@` at a word start opens the file picker.
        let at_word_start = {
            let row = self.editor.current_row();
            let caret = self.editor.caret_byte();
            row[..caret].chars().next_back().is_none_or(char::is_whitespace)
        };
        self.editor.insert_char(c);
        if c == '@' && at_word_start {
            return vec![Effect::OpenMentionPicker];
        }
        vec![]
    }

    /// A bracketed paste. Large pastes become a chip; the real text is
    /// what gets submitted.
    pub fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if text.len() > PASTE_CHIP_BYTES {
            self.pastes.push(text.clone());
            let lines = text.lines().count();
            let chip = format!("[Pasted text #{} +{lines} lines]", self.pastes.len());
            self.editor.insert_str(&chip);
        } else {
            self.editor.insert_str(&text);
        }
    }

    /// The next queued prompt, for the runtime to send when a turn ends.
    pub fn take_queued(&mut self) -> Option<String> {
        (!self.queued.is_empty()).then(|| self.queued.remove(0))
    }

    /// Replace the prompt with text from an external editor.
    pub fn set_text(&mut self, text: &str) {
        self.editor.set_text(text.trim_end_matches('\n'));
    }

    // ── behaviours ─────────────────────────────────────────────────

    fn interrupt(&mut self, busy: bool, now: Instant) -> Vec<Effect> {
        if busy {
            self.armed_exit = Some((Action::Interrupt, now));
            self.hint = Some("Interrupted — Ctrl-C again to exit".into());
            return vec![Effect::Interrupt];
        }
        if !self.editor.is_empty() {
            self.editor.clear();
            self.pastes.clear();
            self.history_pos = None;
            return vec![];
        }
        self.double_press(Action::Interrupt, now, "Press Ctrl-C again to exit")
    }

    fn double_press(&mut self, key: Action, now: Instant, hint: &str) -> Vec<Effect> {
        if let Some((k, t)) = self.armed_exit
            && k == key && now.duration_since(t) <= DOUBLE_PRESS {
                return vec![Effect::Exit];
            }
        self.armed_exit = Some((key, now));
        self.hint = Some(hint.into());
        vec![]
    }

    fn cancel(&mut self, busy: bool, now: Instant) -> Vec<Effect> {
        if self.show_help {
            self.show_help = false;
            return vec![];
        }
        if busy {
            return vec![Effect::Interrupt];
        }
        let double = self.last_escape.is_some_and(|t| now.duration_since(t) <= DOUBLE_PRESS);
        self.last_escape = if double { None } else { Some(now) };
        if double {
            if self.editor.is_empty() {
                self.hint = Some("Rewind arrives with sessions (docs/tui-parity.md T2)".into());
            } else {
                // Claude Code: Esc Esc clears the prompt and keeps it as a
                // history entry, so nothing typed is lost.
                self.push_history(self.editor.text());
                self.editor.clear();
            }
        }
        vec![]
    }

    fn submit(&mut self, busy: bool) -> Vec<Effect> {
        let text = self.editor.text();
        // `\` + Enter is the portable newline.
        if text.ends_with('\\') && self.editor.cursor().1 > 0 && self.editor.on_last_row() {
            self.editor.move_to_end();
            self.editor.delete_back();
            self.editor.insert_newline();
            return vec![];
        }
        if text.trim().is_empty() {
            return vec![];
        }
        let expanded = self.expand(&text);
        self.push_history(expanded.clone());
        self.editor.clear();
        self.pastes.clear();
        if let Some(cmd) = self.as_command(expanded.trim()) {
            return vec![cmd];
        }
        if busy {
            self.queued.push(expanded);
            vec![]
        } else {
            vec![Effect::Submit(expanded)]
        }
    }

    fn history_previous(&mut self) {
        if !self.editor.on_first_row() {
            self.editor.move_up();
            return;
        }
        // Claude Code: Up from the first row takes back a queued prompt first.
        if self.editor.is_empty()
            && let Some(q) = self.queued.pop() {
                self.editor.set_text(&q);
                return;
            }
        if self.history.is_empty() {
            return;
        }
        let pos = match self.history_pos {
            None => {
                self.draft = Some(self.editor.text());
                self.history.len() - 1
            }
            Some(p) => p.saturating_sub(1),
        };
        self.history_pos = Some(pos);
        self.editor.set_text(&self.history[pos]);
        // On the first row, so the next `Up` walks on instead of climbing
        // a recalled multi-line prompt.
        self.editor.move_to_start();
        self.editor.move_to_row_end();
    }

    fn history_next(&mut self) {
        if !self.editor.on_last_row() {
            self.editor.move_down();
            return;
        }
        let Some(p) = self.history_pos else { return };
        if p + 1 < self.history.len() {
            self.history_pos = Some(p + 1);
            self.editor.set_text(&self.history[p + 1]);
        } else {
            self.history_pos = None;
            let draft = self.draft.take().unwrap_or_default();
            self.editor.set_text(&draft);
        }
    }

    /// Search-mode handling. `None` means "not a search key": accept the
    /// match and fall through to normal handling.
    fn apply_in_search(&mut self, action: Action) -> Option<Vec<Effect>> {
        match action {
            Action::HistorySearch => {
                let from = self.search.as_ref().and_then(|s| s.hit);
                let hit = self.find_older(from);
                if let Some(s) = &mut self.search
                    && hit.is_some() {
                        s.hit = hit;
                    }
                Some(vec![])
            }
            Action::DeleteBack => {
                if let Some(s) = &mut self.search {
                    s.query.pop();
                }
                let hit = self.find_older(None);
                if let Some(s) = &mut self.search {
                    s.hit = hit;
                }
                Some(vec![])
            }
            Action::Cancel | Action::Interrupt => {
                self.search = None;
                Some(vec![])
            }
            Action::Submit => {
                self.accept_search();
                Some(vec![])
            }
            _ => {
                self.accept_search();
                None
            }
        }
    }

    fn accept_search(&mut self) {
        if let Some(HistorySearch { hit: Some(i), .. }) = self.search.take() {
            self.editor.set_text(&self.history[i]);
            self.history_pos = Some(i);
        }
        self.search = None;
    }

    /// Newest history entry older than `before` containing the query.
    fn find_older(&self, before: Option<usize>) -> Option<usize> {
        let q = &self.search.as_ref()?.query;
        if q.is_empty() {
            return None;
        }
        let end = before.unwrap_or(self.history.len());
        (0..end).rev().find(|&i| self.history[i].contains(q.as_str()))
    }

    fn edit(&mut self, action: Action) {
        let e = &mut self.editor;
        match action {
            Action::Left => e.move_left(),
            Action::Right => e.move_right(),
            Action::WordLeft => e.move_word_left(),
            Action::WordRight => e.move_word_right(),
            Action::LineStart => e.move_to_row_start(),
            Action::LineEnd => e.move_to_row_end(),
            Action::DeleteBack => e.delete_back(),
            Action::DeleteForward => e.delete_forward(),
            Action::KillToEnd => e.kill_to_row_end(),
            Action::KillToStart => e.kill_to_row_start(),
            Action::KillWordBack => e.kill_word_back(),
            Action::KillWordForward => e.kill_word_forward(),
            Action::Yank => e.yank(),
            Action::YankPop => e.yank_pop(),
            Action::Undo => {
                e.undo();
            }
            // Non-editing actions are dispatched in `apply`; listing them
            // keeps this match exhaustive so a new Action must be placed.
            Action::Interrupt
            | Action::Exit
            | Action::Cancel
            | Action::Redraw
            | Action::ToggleTranscript
            | Action::CycleEffort
            | Action::CycleMode
            | Action::Scrollback
            | Action::Submit
            | Action::Newline
            | Action::ExternalEditor
            | Action::Stash
            | Action::HistoryPrevious
            | Action::HistoryNext
            | Action::HistorySearch => {}
        }
    }

    fn push_history(&mut self, text: String) {
        self.uses.push(Use { text: text.clone(), at: Some(unix_now()), near: true });
        self.recorded.push(text.clone());
        if self.history.last() != Some(&text) {
            self.history.push(text);
        }
        self.history_pos = None;
        self.draft = None;
    }

    fn expand(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for (i, body) in self.pastes.iter().enumerate() {
            let lines = body.lines().count();
            out = out.replace(&format!("[Pasted text #{} +{lines} lines]", i + 1), body);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> InputState {
        let mut i = InputState::new();
        for c in s.chars() {
            i.type_char(c);
        }
        i
    }

    fn t0() -> Instant {
        Instant::now()
    }

    // ── Ctrl-C ──

    #[test]
    fn ctrl_c_with_text_clears_it() {
        let mut i = typed("hello");
        assert!(i.apply(Action::Interrupt, false, t0()).is_empty());
        assert!(i.editor.is_empty());
    }

    #[test]
    fn ctrl_c_twice_on_empty_exits_within_the_window_only() {
        let now = t0();
        let mut i = InputState::new();
        assert!(i.apply(Action::Interrupt, false, now).is_empty());
        assert!(i.hint.as_deref().unwrap().contains("Ctrl-C again"));
        assert_eq!(i.apply(Action::Interrupt, false, now + Duration::from_millis(300)), vec![Effect::Exit]);

        let mut late = InputState::new();
        late.apply(Action::Interrupt, false, now);
        assert!(
            late.apply(Action::Interrupt, false, now + DOUBLE_PRESS + Duration::from_millis(1)).is_empty(),
            "outside the window a second press only re-arms"
        );
    }

    #[test]
    fn ctrl_c_during_a_turn_interrupts_then_exits() {
        let now = t0();
        let mut i = InputState::new();
        assert_eq!(i.apply(Action::Interrupt, true, now), vec![Effect::Interrupt]);
        assert_eq!(i.apply(Action::Interrupt, false, now + Duration::from_millis(100)), vec![Effect::Exit]);
    }

    #[test]
    fn another_key_disarms_exit() {
        let now = t0();
        let mut i = InputState::new();
        i.apply(Action::Interrupt, false, now);
        i.apply(Action::Left, false, now);
        assert!(i.apply(Action::Interrupt, false, now).is_empty());
    }

    // ── Ctrl-D ──

    #[test]
    fn ctrl_d_deletes_forward_with_text_and_exits_twice_when_empty() {
        let now = t0();
        let mut i = typed("ab");
        i.apply(Action::LineStart, false, now);
        i.apply(Action::Exit, false, now);
        assert_eq!(i.editor.text(), "b");
        let mut e = InputState::new();
        assert!(e.apply(Action::Exit, false, now).is_empty());
        assert_eq!(e.apply(Action::Exit, false, now), vec![Effect::Exit]);
    }

    #[test]
    fn ctrl_c_then_ctrl_d_does_not_exit() {
        let now = t0();
        let mut i = InputState::new();
        i.apply(Action::Interrupt, false, now);
        assert!(i.apply(Action::Exit, false, now).is_empty(), "the double press is per key");
    }

    // ── Esc ──

    #[test]
    fn esc_interrupts_a_running_turn() {
        let mut i = InputState::new();
        assert_eq!(i.apply(Action::Cancel, true, t0()), vec![Effect::Interrupt]);
    }

    #[test]
    fn esc_esc_clears_the_prompt_into_history() {
        let now = t0();
        let mut i = typed("draft idea");
        i.apply(Action::Cancel, false, now);
        assert_eq!(i.editor.text(), "draft idea", "a single Esc keeps the text");
        i.apply(Action::Cancel, false, now + Duration::from_millis(200));
        assert!(i.editor.is_empty());
        assert_eq!(i.history(), ["draft idea"]);
    }

    #[test]
    fn esc_never_exits() {
        let now = t0();
        let mut i = InputState::new();
        for k in 0..4 {
            assert!(!i.apply(Action::Cancel, false, now + Duration::from_millis(k * 100)).contains(&Effect::Exit));
        }
    }

    // ── Enter / newlines ──

    #[test]
    fn enter_submits_and_records_history() {
        let mut i = typed("fix the build");
        assert_eq!(i.apply(Action::Submit, false, t0()), vec![Effect::Submit("fix the build".into())]);
        assert!(i.editor.is_empty());
        assert_eq!(i.history(), ["fix the build"]);
    }

    #[test]
    fn enter_on_blank_does_nothing() {
        let mut i = typed("   ");
        assert!(i.apply(Action::Submit, false, t0()).is_empty());
    }

    #[test]
    fn backslash_enter_inserts_a_newline() {
        let mut i = typed("line one\\");
        assert!(i.apply(Action::Submit, false, t0()).is_empty());
        assert_eq!(i.editor.text(), "line one\n");
    }

    #[test]
    fn shift_enter_is_a_newline_not_a_submit() {
        let mut i = typed("a");
        assert!(i.apply(Action::Newline, false, t0()).is_empty());
        i.type_char('b');
        assert_eq!(i.apply(Action::Submit, false, t0()), vec![Effect::Submit("a\nb".into())]);
    }

    #[test]
    fn enter_during_a_turn_queues_and_up_takes_it_back() {
        let mut i = typed("next task");
        assert!(i.apply(Action::Submit, true, t0()).is_empty());
        assert_eq!(i.queued, ["next task"]);
        i.apply(Action::HistoryPrevious, true, t0());
        assert_eq!(i.editor.text(), "next task");
        assert!(i.queued.is_empty());
    }

    #[test]
    fn queued_prompts_drain_in_order() {
        let mut i = InputState::new();
        for p in ["one", "two"] {
            for c in p.chars() {
                i.type_char(c);
            }
            i.apply(Action::Submit, true, t0());
        }
        assert_eq!(i.take_queued().as_deref(), Some("one"));
        assert_eq!(i.take_queued().as_deref(), Some("two"));
        assert_eq!(i.take_queued(), None);
    }

    // ── history ──

    #[test]
    fn up_down_walk_history_and_restore_the_draft() {
        let now = t0();
        let mut i = InputState::new();
        for p in ["first", "second"] {
            for c in p.chars() {
                i.type_char(c);
            }
            i.apply(Action::Submit, false, now);
        }
        for c in "wip".chars() {
            i.type_char(c);
        }
        i.apply(Action::HistoryPrevious, false, now);
        assert_eq!(i.editor.text(), "second");
        i.apply(Action::HistoryPrevious, false, now);
        assert_eq!(i.editor.text(), "first");
        i.apply(Action::HistoryPrevious, false, now);
        assert_eq!(i.editor.text(), "first", "stops at the oldest");
        i.apply(Action::HistoryNext, false, now);
        i.apply(Action::HistoryNext, false, now);
        assert_eq!(i.editor.text(), "wip", "the draft comes back");
    }

    #[test]
    fn up_inside_multiline_moves_the_cursor_not_history() {
        let now = t0();
        let mut i = typed("old");
        i.apply(Action::Submit, false, now);
        for c in "a".chars() {
            i.type_char(c);
        }
        i.apply(Action::Newline, false, now);
        i.type_char('b');
        i.apply(Action::HistoryPrevious, false, now);
        assert_eq!(i.editor.text(), "a\nb");
        assert_eq!(i.editor.cursor().0, 0);
    }

    #[test]
    fn ctrl_r_finds_older_matches_and_enter_accepts() {
        let now = t0();
        let mut i = InputState { history_search: HistorySearchStyle::Incremental, ..InputState::new() };
        for p in ["cargo test", "git status", "cargo build"] {
            for c in p.chars() {
                i.type_char(c);
            }
            i.apply(Action::Submit, false, now);
        }
        i.apply(Action::HistorySearch, false, now);
        for c in "cargo".chars() {
            i.type_char(c);
        }
        assert_eq!(i.search.as_ref().unwrap().hit, Some(2));
        i.apply(Action::HistorySearch, false, now);
        assert_eq!(i.search.as_ref().unwrap().hit, Some(0), "Ctrl-R again goes older");
        i.apply(Action::Submit, false, now);
        assert!(i.search.is_none());
        assert_eq!(i.editor.text(), "cargo test");
    }

    #[test]
    fn esc_cancels_search_without_touching_the_prompt() {
        let now = t0();
        let mut i = typed("keep");
        i.history_search = HistorySearchStyle::Incremental;
        i.apply(Action::HistorySearch, false, now);
        i.type_char('x');
        i.apply(Action::Cancel, false, now);
        assert!(i.search.is_none());
        assert_eq!(i.editor.text(), "keep");
    }

    fn recalled(texts: &[(&str, &str)]) -> Vec<arnes_store::history::Recalled> {
        texts
            .iter()
            .enumerate()
            .map(|(n, (cwd, text))| arnes_store::history::Recalled {
                entry: arnes_store::history::Entry { at: 1_000 + i64::try_from(n).unwrap(), cwd: (*cwd).into(), text: (*text).into() },
                here: *cwd == "/here",
            })
            .collect()
    }

    fn loaded(texts: &[&str]) -> InputState {
        let mut i = InputState::new();
        i.load_history(recalled(&texts.iter().map(|t| ("/here", *t)).collect::<Vec<_>>()), true);
        i
    }

    #[test]
    fn walking_stops_at_both_edges_and_an_empty_history_leaves_the_prompt() {
        let now = t0();
        let mut i = typed("draft");
        i.apply(Action::HistoryPrevious, false, now);
        i.apply(Action::HistoryNext, false, now);
        assert_eq!(i.editor.text(), "draft", "nothing to walk, nothing changes");
        let mut i = loaded(&["one", "two"]);
        i.apply(Action::HistoryNext, false, now);
        assert_eq!(i.editor.text(), "", "Down before any Up stays on the draft");
        for _ in 0..5 {
            i.apply(Action::HistoryPrevious, false, now);
        }
        assert_eq!(i.editor.text(), "one", "Up stops at the oldest");
        for _ in 0..5 {
            i.apply(Action::HistoryNext, false, now);
        }
        assert_eq!(i.editor.text(), "", "past the newest, the (empty) draft");
    }

    #[test]
    fn a_recalled_multiline_prompt_is_walked_past_not_climbed() {
        let now = t0();
        let mut i = loaded(&["one", "a\nb\nc", "last"]);
        i.type_char('w');
        i.apply(Action::HistoryPrevious, false, now);
        assert_eq!(i.editor.text(), "last");
        i.apply(Action::HistoryPrevious, false, now);
        assert_eq!((i.editor.text().as_str(), i.editor.cursor().0), ("a\nb\nc", 0), "Up lands on the first row");
        i.apply(Action::HistoryPrevious, false, now);
        assert_eq!(i.editor.text(), "one", "so the next Up keeps walking");
        i.apply(Action::HistoryNext, false, now);
        assert_eq!((i.editor.text().as_str(), i.editor.cursor().0), ("a\nb\nc", 2), "Down lands on the last row");
        i.apply(Action::HistoryPrevious, false, now);
        assert_eq!((i.editor.text().as_str(), i.editor.cursor().0), ("a\nb\nc", 1), "inside it, Up moves the cursor");
        i.apply(Action::HistoryNext, false, now);
        i.apply(Action::HistoryNext, false, now);
        assert_eq!(i.editor.text(), "last", "Down leaves only from the bottom row");
        i.apply(Action::HistoryNext, false, now);
        assert_eq!(i.editor.text(), "w", "and past the newest the draft comes back");
    }

    #[test]
    fn loaded_history_walks_before_this_sessions_and_only_new_prompts_are_recorded() {
        let now = t0();
        let mut i = loaded(&["from yesterday"]);
        assert!(i.take_recorded().is_empty(), "what was loaded is not written again");
        for c in "today".chars() {
            i.type_char(c);
        }
        i.apply(Action::Submit, false, now);
        i.set_text("kept by esc esc");
        i.apply(Action::Cancel, false, now);
        i.apply(Action::Cancel, false, now);
        assert_eq!(i.take_recorded(), ["today", "kept by esc esc"]);
        assert!(i.take_recorded().is_empty());
        i.apply(Action::HistoryPrevious, false, now);
        i.apply(Action::HistoryPrevious, false, now);
        i.apply(Action::HistoryPrevious, false, now);
        assert_eq!(i.editor.text(), "from yesterday");
    }

    #[test]
    fn the_feed_ranks_this_directory_first_pins_the_newest_and_lists_each_prompt_once() {
        let u = |text: &str, at: i64, near: bool| Use { text: text.into(), at: Some(at), near };
        let now = 10 * 86_400;
        let uses = vec![
            u("elsewhere often", now - 3_600, false),
            u("elsewhere often", now - 3_600, false),
            u("elsewhere often", now - 3_600, false),
            u("here often", now - 86_400, true),
            u("here often", now - 86_400, true),
            u("both", now - 7_200, false),
            u("here once", now - 5 * 86_400, true),
            u("both", now - 60, true),
            u("just now", now, true),
        ];
        assert_eq!(history_feed(&uses, RankOptions::default(), now), ["just now", "here often", "both", "here once", "elsewhere often"]);
        let recency = RankOptions { mode: wadachi_spec::usage::RankMode::Recency, pin_most_recent: false };
        assert_eq!(history_feed(&uses, recency, now), ["just now", "both", "here once", "here often", "elsewhere often"]);
        assert!(history_feed(&[], RankOptions::default(), now).is_empty());
    }

    #[test]
    fn ctrl_r_opens_the_ranked_picker_and_accepting_replaces_the_prompt() {
        let now = t0();
        let mut i = InputState::new();
        for p in ["cargo test", "git status", "cargo build"] {
            for c in p.chars() {
                i.type_char(c);
            }
            i.apply(Action::Submit, false, now);
        }
        i.apply(Action::HistorySearch, false, now);
        let pp = i.picker.as_ref().expect("Ctrl-R opens the history picker");
        assert_eq!(pp.kind, PickerKind::History);
        assert_eq!(pp.picker.selected_key().map(String::as_str), Some("cargo build"), "the newest first");
        for c in "cargo".chars() {
            i.type_char(c);
        }
        assert_eq!(i.picker.as_ref().unwrap().picker.visible_count(), 2);
        i.apply(Action::HistorySearch, false, now);
        let second = i.picker.as_ref().unwrap().picker.selected_key().cloned().unwrap();
        assert_ne!(second, "cargo build", "Ctrl-R again moves to the next match");
        assert!(second.starts_with("cargo"));
        let fx = i.apply(Action::Submit, false, now);
        assert!(fx.is_empty(), "accepting only fills the prompt; Enter sends it");
        assert!(i.picker.is_none());
        assert_eq!(i.editor.text(), second);
    }

    #[test]
    fn ctrl_r_seeds_its_query_from_the_typed_line_and_esc_keeps_the_prompt() {
        let now = t0();
        let mut i = loaded(&["deploy staging", "run tests", "deploy prod"]);
        for c in "stag".chars() {
            i.type_char(c);
        }
        i.apply(Action::HistorySearch, false, now);
        let p = &i.picker.as_ref().unwrap().picker;
        assert_eq!((p.query(), p.visible_count(), p.selected_key().map(String::as_str)), ("stag", 1, Some("deploy staging")));
        i.apply(Action::Cancel, false, now);
        assert!(i.picker.is_none());
        assert_eq!(i.editor.text(), "stag");
    }

    #[test]
    fn ctrl_r_on_an_empty_history_says_so_and_opens_nothing() {
        let mut i = InputState::new();
        i.apply(Action::HistorySearch, false, t0());
        assert!(i.picker.is_none() && i.search.is_none());
        assert_eq!(i.hint.as_deref(), Some("no prompt history yet"));
    }

    #[test]
    fn a_multiline_prompt_is_one_picker_row() {
        let mut i = loaded(&["first line\nsecond line"]);
        i.open_history_picker(0);
        let view = i.picker.as_ref().unwrap().picker.view();
        assert_eq!(view.rows.len(), 1);
        assert!(!view.rows[0].label.contains('\n'));
    }

    // ── paste / stash / help ──

    #[test]
    fn large_paste_becomes_a_chip_and_submits_the_real_text() {
        let body = "x".repeat(PASTE_CHIP_BYTES + 1) + "\nsecond line";
        let mut i = typed("review: ");
        i.paste(&body);
        assert!(i.editor.text().contains("[Pasted text #1 +2 lines]"));
        assert_eq!(i.apply(Action::Submit, false, t0()), vec![Effect::Submit(format!("review: {body}"))]);
    }

    #[test]
    fn small_paste_is_inserted_inline_with_crlf_normalised() {
        let mut i = InputState::new();
        i.paste("a\r\nb");
        assert_eq!(i.editor.text(), "a\nb");
    }

    #[test]
    fn ctrl_s_stashes_and_restores() {
        let now = t0();
        let mut i = typed("half-written");
        i.apply(Action::Stash, false, now);
        assert!(i.editor.is_empty());
        i.apply(Action::Stash, false, now);
        assert_eq!(i.editor.text(), "half-written");
    }

    #[test]
    fn question_mark_toggles_help_only_on_an_empty_prompt() {
        let mut i = InputState::new();
        i.type_char('?');
        assert!(i.show_help && i.editor.is_empty());
        i.type_char('?');
        assert!(!i.show_help);
        let mut t = typed("why");
        t.type_char('?');
        assert_eq!(t.editor.text(), "why?");
    }

    // ── readline passthrough ──

    #[test]
    fn readline_actions_reach_the_editor() {
        let now = t0();
        let mut i = typed("cat src/main.rs");
        i.apply(Action::KillWordBack, false, now);
        assert_eq!(i.editor.text(), "cat ");
        i.apply(Action::Yank, false, now);
        assert_eq!(i.editor.text(), "cat src/main.rs");
        i.apply(Action::Undo, false, now);
        assert_eq!(i.editor.text(), "cat ");
    }

    #[test]
    fn ui_actions_become_effects() {
        let now = t0();
        let mut i = InputState::new();
        assert_eq!(i.apply(Action::Redraw, false, now), vec![Effect::Redraw]);
        assert_eq!(i.apply(Action::ExternalEditor, false, now), vec![Effect::OpenEditor]);
        assert_eq!(i.apply(Action::ToggleTranscript, false, now), vec![Effect::ToggleTranscript]);
        assert_eq!(i.apply(Action::CycleEffort, false, now), vec![Effect::CycleEffort]);
    }

    // ── vim ──

    fn vim_input() -> InputState {
        let mut i = InputState::new();
        i.enable_vim();
        i
    }

    #[test]
    fn vim_starts_in_insert_and_esc_goes_normal() {
        let mut i = vim_input();
        assert_eq!(i.vim_mode(), Some(escriba_core::Mode::Insert));
        i.type_char('h');
        i.vim_key(&escriba_keymap::Key::Esc, false);
        assert_eq!(i.vim_mode(), Some(escriba_core::Mode::Normal));
    }

    #[test]
    fn colon_w_submits_through_the_same_path_and_returns_to_insert() {
        let mut i = vim_input();
        for c in "ship it".chars() {
            i.type_char(c);
        }
        i.vim_key(&escriba_keymap::Key::Esc, false);
        let mut fx = Vec::new();
        for k in [':', 'w'] {
            fx.extend(i.vim_key(&escriba_keymap::Key::Char(k), false));
        }
        fx.extend(i.vim_key(&escriba_keymap::Key::Enter, false));
        assert_eq!(fx, vec![Effect::Submit("ship it".into())]);
        assert_eq!(i.history(), ["ship it"]);
        assert_eq!(i.vim_mode(), Some(escriba_core::Mode::Insert));
    }

    #[test]
    fn colon_q_exits() {
        let mut i = vim_input();
        i.vim_key(&escriba_keymap::Key::Esc, false);
        let mut fx = Vec::new();
        for k in [':', 'q'] {
            fx.extend(i.vim_key(&escriba_keymap::Key::Char(k), false));
        }
        fx.extend(i.vim_key(&escriba_keymap::Key::Enter, false));
        assert_eq!(fx, vec![Effect::Exit]);
    }

    #[test]
    fn vim_k_on_the_first_row_walks_history() {
        let mut i = vim_input();
        for c in "earlier".chars() {
            i.type_char(c);
        }
        i.apply(Action::Submit, false, t0());
        i.vim_key(&escriba_keymap::Key::Esc, false);
        i.vim_key(&escriba_keymap::Key::Char('k'), false);
        assert_eq!(i.editor.text(), "earlier");
    }

    #[test]
    fn visual_mode_is_refused_with_a_hint_not_entered() {
        let mut i = vim_input();
        i.vim_key(&escriba_keymap::Key::Esc, false);
        i.vim_key(&escriba_keymap::Key::Char('v'), false);
        assert_eq!(i.vim_mode(), Some(escriba_core::Mode::Normal));
        assert!(i.hint.as_deref().unwrap_or("").contains("visual"));
    }

    // ── pickers ──

    #[test]
    fn slash_on_an_empty_prompt_opens_the_command_picker() {
        let mut i = InputState::new();
        i.type_char('/');
        assert_eq!(i.picker.as_ref().map(|p| p.kind), Some(PickerKind::Command));
        for c in "cle".chars() {
            i.type_char(c);
        }
        assert_eq!(i.apply(Action::Submit, false, t0()), vec![Effect::Command { name: "clear".into(), args: String::new() }]);
        assert!(i.picker.is_none() && i.editor.is_empty());
    }

    #[test]
    fn a_declared_blue_command_is_in_the_picker_and_dispatches_typed() {
        let mut i = InputState::new();
        i.user_commands = vec![("wordcount".into(), "count words".into())];
        i.type_char('/');
        for c in "wordc".chars() {
            i.type_char(c);
        }
        assert_eq!(i.apply(Action::Submit, false, t0()), vec![Effect::Command { name: "wordcount".into(), args: String::new() }]);
        // Typed in full with an argument, bypassing the picker.
        i.editor.set_text("/wordcount the rest");
        assert_eq!(i.apply(Action::Submit, false, t0()), vec![Effect::Command { name: "wordcount".into(), args: "the rest".into() }]);
        // Control: an undeclared name is a prompt, not a command.
        i.editor.set_text("/nosuch x");
        assert_eq!(i.apply(Action::Submit, false, t0()), vec![Effect::Submit("/nosuch x".into())]);
    }

    #[test]
    fn a_command_with_args_fills_the_prompt_then_enter_runs_it() {
        let mut i = InputState::new();
        i.type_char('/');
        for c in "mode".chars() {
            i.type_char(c);
        }
        assert!(i.apply(Action::Submit, false, t0()).is_empty());
        assert_eq!(i.editor.text(), "/mode ");
        for c in "plan".chars() {
            i.type_char(c);
        }
        assert_eq!(i.apply(Action::Submit, false, t0()), vec![Effect::Command { name: "mode".into(), args: "plan".into() }]);
    }

    #[test]
    fn at_word_start_asks_for_files_and_accepting_inserts_the_path() {
        let mut i = typed("look at ");
        assert_eq!(i.type_char('@'), vec![Effect::OpenMentionPicker]);
        i.open_mention_picker(vec!["Cargo.toml".into(), "src/main.rs".into()]);
        for c in "main".chars() {
            i.type_char(c);
        }
        i.apply(Action::Submit, false, t0());
        assert_eq!(i.editor.text(), "look at @src/main.rs ");
    }

    #[test]
    fn at_inside_a_word_is_just_a_character() {
        let mut i = typed("user");
        assert!(i.type_char('@').is_empty());
        assert_eq!(i.editor.text(), "user@");
    }

    #[test]
    fn esc_cancels_the_picker_and_keeps_the_text() {
        let mut i = typed("see ");
        i.type_char('@');
        i.open_mention_picker(vec!["a.rs".into()]);
        i.apply(Action::Cancel, false, t0());
        assert!(i.picker.is_none());
        assert_eq!(i.editor.text(), "see @");
    }

    #[test]
    fn typed_slash_command_without_the_picker_still_runs() {
        let mut i = InputState::new();
        i.editor.insert_str("/exit");
        assert_eq!(i.apply(Action::Submit, false, t0()), vec![Effect::Command { name: "exit".into(), args: String::new() }]);
    }

    #[test]
    fn unknown_slash_text_is_an_ordinary_prompt() {
        let mut i = InputState::new();
        i.editor.insert_str("/usr/bin is a path");
        assert_eq!(i.apply(Action::Submit, false, t0()), vec![Effect::Submit("/usr/bin is a path".into())]);
    }

    #[test]
    fn typing_a_command_and_its_argument_in_one_go_keeps_the_argument() {
        let mut i = InputState::new();
        i.type_char('/');
        for c in "mode plan".chars() {
            i.type_char(c);
        }
        assert_eq!(i.apply(Action::Submit, false, t0()), vec![Effect::Command { name: "mode".into(), args: "plan".into() }]);
        assert!(i.editor.is_empty() && i.picker.is_none());
    }
}
