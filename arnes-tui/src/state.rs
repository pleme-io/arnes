use arnes_core::Conversation;

use crossterm::event::KeyEvent;
use egaku::{NavOutcome, Navigator};

use crate::input::InputState;
use crate::keymap::Keymap;
use crate::look::Surface;

/// A pending "may arnes run this tool?" question (docs/tui-parity.md §4.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalView {
    pub tool: String,
    /// The call's input, compact JSON.
    pub input: String,
    /// 0 = yes, 1 = yes and don't ask again for this tool, 2 = no.
    pub selected: usize,
    /// guardrail's warning, shown above the options.
    pub warning: Option<String>,
}

impl ApprovalView {
    pub const OPTIONS: [&'static str; 3] =
        ["Yes", "Yes, and don't ask again for this tool", "No, tell arnes what to do instead (Esc)"];
}

/// How loud a toast is. Each level has its own lifetime: an error stays
/// long enough to read, an info note gets out of the way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    Info,
    Warn,
    Error,
}

impl ToastLevel {
    #[must_use]
    pub const fn lifetime(self) -> std::time::Duration {
        std::time::Duration::from_secs(match self {
            Self::Info => 4,
            Self::Warn => 8,
            Self::Error => 12,
        })
    }

    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Info => "\u{2022}",
            Self::Warn => "\u{25b2}",
            Self::Error => "\u{2718}",
        }
    }
}

/// A fading notice: the one place arnes's warnings, errors and notes
/// appear. Nothing writes to the terminal behind the TUI — the log file
/// has the detail; a toast has the one line an operator needs.
#[derive(Debug, Clone)]
pub struct Toast {
    pub level: ToastLevel,
    pub text: String,
    /// When it (last) arrived; a repeat refreshes it.
    pub born: std::time::Instant,
    /// How many times it arrived in a row.
    pub count: u32,
    /// How long it stays in the lane (from the look spec).
    pub life: std::time::Duration,
}

impl Toast {
    /// 0.0 fresh → 1.0 expired. The last 30% of a life is drawn faded.
    #[must_use]
    pub fn age(&self, now: std::time::Instant) -> f32 {
        now.saturating_duration_since(self.born).as_secs_f32() / self.life.as_secs_f32().max(0.001)
    }
}

/// Most toasts shown at once; the oldest goes first.
pub const MAX_TOASTS: usize = 4;

/// The stateful UI model: the conversation, the prompt (with all of its
/// Claude Code semantics — see [`InputState`]) and the ephemeral view state.
#[derive(Debug, Clone)]
pub struct TuiState {
    pub conversation: Conversation,
    pub input: InputState,
    pub busy: bool,
    pub last_error: Option<String>,
    pub model: String,
    /// Reasoning effort shown in the status line (`Alt-T` cycles it).
    pub effort: String,
    /// `Ctrl-O`: show thinking and full tool results instead of summaries.
    pub show_transcript: bool,
    /// MCP tools available this session.
    pub tools: usize,
    /// Status segments as their hosted bidama drew them, by segment name
    /// (`model`, `effort`, `tools`, `permission-mode`; `docs/kernel.md`). A
    /// name that is absent draws the kernel's own text; an empty one hides it.
    pub hosted_segments: std::collections::HashMap<String, String>,
    /// Permission mode label (`Shift-Tab` cycles it).
    pub permission_mode: String,
    /// The active profile (docs/profiles.md); empty when none.
    pub profile: String,
    /// Extra status-line segments (`mcp 5/6` from the vigy facts), in order.
    pub segments: Vec<String>,
    pub approval: Option<ApprovalView>,
    /// A local, read-only panel (`/mcp` output…) — never sent to the model.
    pub panel: Option<(String, Vec<String>)>,
    /// `/mcp`: the server → tool → detail browser.
    pub mcp: Option<crate::mcp_browser::McpBrowser>,
    /// Session usage for the status line (the runtime copies it from the
    /// ledger's totals).
    pub usage: UsageView,
    /// Fading notices, oldest first.
    pub toasts: Vec<Toast>,
    /// The resolved theme at the terminal's colour depth.
    pub palette: egaku_term::theme::Palette,
    /// False draws every animation at its end state (reduced motion).
    pub motion: bool,
    /// When the running turn started (drives the spinner and shimmer).
    pub busy_since: Option<std::time::Instant>,
    /// The overlay in front, and when it came to the front.
    pub surface: (crate::look::Surface, std::time::Instant),
    /// A finished reply unrolling: messages from this index on are
    /// revealed row by row, starting at the instant.
    pub reveal: Option<(usize, std::time::Instant)>,
    /// The screen's declared knobs (region sizes, notice lifetimes).
    pub spec: crate::look::LookSpec,
    /// Micro-interactions: when each kind of change last happened.
    pub blips: crate::look::Blips,
    /// What is happening now, for the activity line.
    pub activity: crate::activity::Activity,
    /// Every notice this session, newest last (`/notices` shows them).
    pub notice_log: Vec<Toast>,
    /// What [`TuiState::observe`] saw last, to tell a change.
    seen: Seen,
    /// The navigator of the overlay in front (help, a local panel, the
    /// approval question, the scrollback). One grammar for all of them; a new
    /// overlay gets it by being a [`Surface`].
    pub nav: Navigator,
    /// The navigation keymap new list surfaces start from (`keys.nav.menu`).
    pub nav_menu: egaku::NavKeymap,
    /// Whether the conversation follows its live tail or is scrolled back.
    pub pane: Pane,
    /// Text a yank put here for the runtime to send to the clipboard (OSC 52).
    pub clipboard: Option<String>,
    /// The terminal size, `(cols, rows)`; the runtime keeps it current.
    pub viewport: (u16, u16),
}

/// Where the conversation pane is looking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    /// Following the newest output.
    #[default]
    Live,
    /// Scrolled back; the front navigator holds the offset.
    Scrollback,
}

/// What a key did to the overlay in front.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    /// No overlay is open: the key belongs to the prompt.
    None,
    /// The overlay used the key.
    Consumed,
    /// The overlay closed and the key should reach the prompt as well.
    Pass,
    /// The approval question was answered with this option (0 yes, 1 always, 2 no).
    Approval(usize),
}

/// The values whose change is acknowledged with a blip.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Seen {
    mode: String,
    permission: String,
    effort: String,
    model: String,
    hint: Option<String>,
    pick: Option<usize>,
    list: Option<usize>,
    sent: usize,
}

/// Notices kept for review.
pub const NOTICE_LOG: usize = 200;

/// What the status line shows about spend.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UsageView {
    pub calls: u32,
    /// Every prompt token: uncached + cache reads + cache writes.
    pub prompt_tokens: u64,
    pub output_tokens: u64,
    pub cache_hit_ratio: f64,
    pub cost_usd: f64,
    /// False when some call's price is unknown (cost is then a floor).
    pub priced: bool,
}

impl UsageView {
    /// `↑12.3k 81% cached ↓1.2k $0.0042` — or empty before the first call.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.calls == 0 {
            return String::new();
        }
        let k = |n: u64| if n >= 1000 { format!("{}.{}k", n / 1000, n % 1000 / 100) } else { n.to_string() };
        let cost = if self.priced { format!("${:.4}", self.cost_usd) } else { "$?".to_owned() };
        format!(
            "↑{} {:.0}% cached ↓{} {cost}",
            k(self.prompt_tokens),
            self.cache_hit_ratio * 100.0,
            k(self.output_tokens)
        )
    }
}

impl TuiState {
    #[must_use]
    pub fn new(conversation: Conversation, model: impl Into<String>) -> Self {
        Self {
            conversation,
            input: InputState::new(),
            busy: false,
            last_error: None,
            model: model.into(),
            effort: "default".into(),
            show_transcript: false,
            tools: 0,
            hosted_segments: std::collections::HashMap::new(),
            profile: String::new(),
            permission_mode: "default".into(),
            segments: Vec::new(),
            approval: None,
            panel: None,
            mcp: None,
            usage: UsageView::default(),
            toasts: Vec::new(),
            palette: egaku_term::theme::Palette::from_fleet(ishou_tokens::FleetTheme::prescribed_default()),
            motion: true,
            busy_since: None,
            surface: (crate::look::Surface::Conversation, std::time::Instant::now()),
            reveal: None,
            spec: crate::look::LookSpec::default(),
            blips: crate::look::Blips::default(),
            activity: crate::activity::Activity::default(),
            notice_log: Vec::new(),
            seen: Seen::default(),
            nav: Navigator::pager(),
            nav_menu: egaku::NavKeymap::menu(),
            pane: Pane::Live,
            clipboard: None,
            viewport: (80, 24),
        }
        .observed()
    }

    fn observed(mut self) -> Self {
        self.seen = self.snapshot();
        self
    }

    fn snapshot(&self) -> Seen {
        use crate::mcp_browser::RowKind;
        Seen {
            mode: self.input.vim.as_ref().map_or_else(String::new, |v| v.mode().as_str().to_owned()),
            permission: self.permission_mode.clone(),
            effort: self.effort.clone(),
            model: self.model.clone(),
            hint: self.input.hint.clone(),
            pick: self.input.picker.as_ref().map(|p| p.picker.view().selected),
            list: self.mcp.as_ref().and_then(|b| b.rows(usize::MAX).0.iter().position(|r| r.kind == RowKind::Selected)),
            sent: self.conversation.messages.iter().filter(|m| m.role == arnes_core::Role::User).count(),
        }
    }

    /// Compare against the last frame and fire a blip for each change:
    /// the acknowledgement every state change gets (docs/design.md).
    pub fn observe(&mut self, now: std::time::Instant) {
        use crate::look::Blip;
        let next = self.snapshot();
        let was = std::mem::replace(&mut self.seen, next.clone());
        let mut fire = |changed: bool, b| {
            if changed {
                self.blips.fire(b, now);
            }
        };
        fire(was.mode != next.mode, Blip::Mode);
        fire(was.permission != next.permission, Blip::Permission);
        fire(was.effort != next.effort, Blip::Effort);
        fire(was.model != next.model, Blip::Model);
        fire(was.hint != next.hint && next.hint.is_some(), Blip::Hint);
        fire(was.pick != next.pick && was.pick.is_some() && next.pick.is_some(), Blip::Pick);
        fire(was.list != next.list && was.list.is_some() && next.list.is_some(), Blip::Scroll);
        fire(next.sent > was.sent, Blip::Submit);
        self.note_surface(now);
    }

    /// Apply the look spec (from the config).
    pub fn set_spec(&mut self, spec: crate::look::LookSpec) {
        self.spec = spec;
        self.motion = spec.motion;
        self.activity.hold = spec.hold;
    }

    /// Apply everything a config says about the screen and the keys, in
    /// place: the look spec, the theme at `depth`, the navigation keymaps and
    /// the editor mode. The conversation, the prompt text, the scrollback,
    /// the open surface and the notices stay. Returns the prompt keymap (the
    /// runtime holds it) and one refusal per value that could not apply,
    /// each with its default kept.
    pub fn apply_config(&mut self, cfg: &arnes_config::ArnesConfig, depth: egaku_term::theme::ColorDepth) -> (crate::Keymap, Vec<String>) {
        let (spec, mut refused) = crate::look::LookSpec::from_config(cfg);
        self.set_spec(spec);
        let (palette, warn) = crate::look::palette(cfg.theme().as_deref(), depth);
        self.palette = palette;
        refused.extend(warn);
        let (menu, filter, r) = crate::nav::keymaps(cfg);
        self.set_nav_keymaps(&menu, &filter);
        refused.extend(r);
        let mode = cfg.editor_mode_name();
        let (mode, r) = arnes_config::sections::one_of("editor-mode", mode.as_deref(), &["normal", "vim"], "normal");
        refused.extend(r);
        match (mode, self.input.vim.is_some()) {
            ("vim", false) => self.input.enable_vim(),
            ("normal", true) => self.input.vim = None,
            _ => {}
        }
        let (history, r) = cfg.history_knobs();
        refused.extend(r);
        self.input.history_search = history.search;
        self.input.history_rank = wadachi_spec::usage::RankOptions {
            mode: if history.recency { wadachi_spec::usage::RankMode::Recency } else { wadachi_spec::usage::RankMode::Frecency },
            pin_most_recent: history.pin_recent,
        };
        let (keymap, r) = crate::Keymap::from_config(cfg);
        refused.extend(r);
        (keymap, refused)
    }

    /// Swap the navigation keymaps in place: the overlay in front, an open
    /// picker and an open `/mcp` browser read the new keys from the next
    /// press; nothing closes and no position is lost.
    pub fn set_nav_keymaps(&mut self, menu: &egaku::NavKeymap, filter: &egaku::NavKeymap) {
        *self.nav.keymap_mut() = menu.clone();
        if let Some(m) = self.mcp.as_mut() {
            m.set_keymap(menu);
        }
        self.nav_menu = menu.clone();
        self.input.nav_filter = Some(filter.clone());
        if let Some(p) = self.input.picker.as_mut() {
            p.nav = egaku::NavResolver::new(filter.clone());
        }
    }

    /// A tool event from the running turn.
    pub fn activity_event(&mut self, ev: crate::activity::Event, now: std::time::Instant) {
        if matches!(ev, crate::activity::Event::ToolEnd { .. }) {
            self.blips.fire(crate::look::Blip::Tick, now);
        }
        self.activity.apply(ev, now);
    }

    /// The approval question was answered.
    pub fn answered(&mut self, tool: &str, allowed: bool, now: std::time::Instant) {
        let text = if allowed { format!("allowed {tool}") } else { format!("denied {tool}") };
        self.activity.note(text, allowed, now);
        self.blips.fire(crate::look::Blip::Verdict, now);
    }

    /// Rows the notice lane wants: one per live notice, up to the spec.
    #[must_use]
    pub fn notice_rows(&self) -> u16 {
        u16::try_from(self.toasts.len()).unwrap_or(u16::MAX).min(self.spec.notice_rows)
    }

    /// The notice log as panel lines, newest first.
    #[must_use]
    pub fn notice_lines(&self) -> Vec<String> {
        if self.notice_log.is_empty() {
            return vec!["no notices this session".into()];
        }
        self.notice_log
            .iter()
            .rev()
            .map(|t| {
                let count = if t.count > 1 { format!(" \u{d7}{}", t.count) } else { String::new() };
                format!("{} {}{count}", t.level.glyph(), t.text)
            })
            .collect()
    }

    /// The overlay currently in front, derived from the view state.
    #[must_use]
    pub fn front(&self) -> Surface {
        if self.approval.is_some() {
            Surface::Approval
        } else if self.input.picker.is_some() {
            Surface::Picker
        } else if self.input.show_help {
            Surface::Help
        } else if self.mcp.is_some() {
            Surface::Mcp
        } else if self.panel.is_some() {
            Surface::Panel
        } else if self.is_scrolling() {
            Surface::Scrollback
        } else {
            Surface::Conversation
        }
    }

    /// Record a change of overlay so its opening animates from `now`.
    pub fn note_surface(&mut self, now: std::time::Instant) {
        let f = self.front();
        if f != self.surface.0 {
            self.surface = (f, now);
            self.reset_nav(f);
        }
    }

    /// A fresh navigator for a surface coming to the front. The scrollback
    /// sets its own on entry (it starts at the tail, not the top).
    fn reset_nav(&mut self, f: Surface) {
        let keymap = self.nav.keymap().clone();
        let fresh = |kind| Navigator::new(kind, egaku::SearchStyle::Jump, keymap.clone());
        match f {
            Surface::Help | Surface::Panel => self.nav = fresh(egaku::NavKind::Pager),
            Surface::Approval => self.nav = fresh(egaku::NavKind::Cursor),
            Surface::Scrollback | Surface::Conversation | Surface::Mcp | Surface::Picker => {}
        }
    }

    /// The conversation pane's inner size, `(width, height)`, from
    /// [`Self::viewport`] and the screen grid — the rows help, panels and
    /// the scrollback show. Read with every surface fully open.
    #[must_use]
    pub fn pane_inner(&self) -> (u16, u16) {
        let (cols, rows) = self.viewport;
        let settled = self.surface.1 + std::time::Duration::from_secs(3600);
        let r = crate::layout::layout(cols, rows, crate::chrome::demand(self, settled)).conversation;
        (r.w.saturating_sub(2), r.h.saturating_sub(2))
    }

    /// Rows a pager overlay shows: the pane less its footer row.
    fn pager_rows(&self) -> usize {
        usize::from(self.pane_inner().1.saturating_sub(1)).max(1)
    }

    /// Whether the conversation is scrolled back off the live tail.
    #[must_use]
    pub fn is_scrolling(&self) -> bool {
        self.pane == Pane::Scrollback
    }

    /// Leave the live tail and scroll back half a page. False when the
    /// conversation fits and there is nothing to scroll.
    pub fn enter_scrollback(&mut self) -> bool {
        let rows = crate::render::conversation_rows(self, self.pane_inner().0);
        let mut nav = Navigator::new(egaku::NavKind::Pager, egaku::SearchStyle::Jump, self.nav.keymap().clone());
        nav.set_viewport(self.pager_rows());
        if nav.at_end(rows.len()) && nav.offset() == 0 {
            return false;
        }
        nav.to_end(rows.len());
        nav.apply(egaku::NavAction::HalfPageUp, None, &rows);
        self.nav = nav;
        self.pane = Pane::Scrollback;
        true
    }

    /// One key to the overlay in front: the approval dialog, the MCP
    /// browser, help, a local panel or the scrollback, each through the same
    /// navigation grammar. Pickers belong to the prompt and are not here.
    pub fn overlay_key(&mut self, ke: &KeyEvent, keymap: &Keymap) -> Overlay {
        let open = self.approval.is_some() || self.mcp.is_some() || self.input.show_help || self.panel.is_some() || self.is_scrolling();
        if !open {
            return Overlay::None;
        }
        let Some(key) = crate::nav::nav_key(ke) else { return Overlay::Consumed };
        if self.approval.is_some() {
            return self.approval_key(key);
        }
        let list_rows = usize::from(self.pane_inner().1.saturating_sub(1)).max(1);
        if let Some(b) = self.mcp.as_mut() {
            b.page = list_rows;
            match b.nav_key(key) {
                crate::mcp_browser::Outcome::Close => self.mcp = None,
                crate::mcp_browser::Outcome::Yank(t) => self.clipboard = Some(t),
                crate::mcp_browser::Outcome::Stay => {}
            }
            return Overlay::Consumed;
        }
        let height = self.pager_rows();
        self.nav.set_viewport(height);
        if self.input.show_help {
            let rows = crate::render::help_lines(keymap, self.nav.keymap());
            return match self.nav.handle(key, &rows) {
                NavOutcome::Dismiss | NavOutcome::Close | NavOutcome::Back => {
                    self.input.show_help = false;
                    Overlay::Consumed
                }
                NavOutcome::Yank(i) => {
                    self.clipboard = rows.get(i).cloned();
                    Overlay::Consumed
                }
                // `?` toggles help off, as it toggled it on.
                NavOutcome::Unhandled if key == egaku::NavKey::Char('?') => {
                    self.input.show_help = false;
                    Overlay::Consumed
                }
                NavOutcome::Unhandled => {
                    self.input.show_help = false;
                    Overlay::Pass
                }
                _ => Overlay::Consumed,
            };
        }
        if let Some((_, lines)) = &self.panel {
            let lines = lines.clone();
            return match self.nav.handle(key, &lines) {
                NavOutcome::Dismiss | NavOutcome::Close | NavOutcome::Back => {
                    self.panel = None;
                    Overlay::Consumed
                }
                NavOutcome::Yank(i) => {
                    self.clipboard = lines.get(i).cloned();
                    Overlay::Consumed
                }
                // Typing goes on: the panel steps aside and the key lands.
                NavOutcome::Unhandled => {
                    self.panel = None;
                    Overlay::Pass
                }
                _ => Overlay::Consumed,
            };
        }
        // The scrollback.
        let rows = crate::render::conversation_rows(self, self.pane_inner().0);
        match self.nav.handle(key, &rows) {
            NavOutcome::Dismiss | NavOutcome::Close => self.pane = Pane::Live,
            NavOutcome::Moved if self.nav.at_end(rows.len()) => self.pane = Pane::Live,
            NavOutcome::Yank(i) => self.clipboard = rows.get(i).map(|r| r.trim().to_owned()),
            NavOutcome::Unhandled => {
                self.pane = Pane::Live;
                return Overlay::Pass;
            }
            _ => {}
        }
        Overlay::Consumed
    }

    /// The approval dialog's own keys first (Claude Code's `1`/`2`/`3` and
    /// `y`/`a`/`n`), then the navigation grammar: `j`/`k` move, Enter picks,
    /// Esc or `q` declines. An answer closes the dialog.
    fn approval_key(&mut self, key: egaku::NavKey) -> Overlay {
        use egaku::NavKey::Char;
        let own = match key {
            Char('1' | 'y') => Some(0),
            Char('2' | 'a') => Some(1),
            Char('3' | 'n') => Some(2),
            _ => None,
        };
        if let Some(answer) = own {
            self.approval = None;
            return Overlay::Approval(answer);
        }
        let Some(a) = self.approval.as_mut() else { return Overlay::None };
        let options = ApprovalView::OPTIONS;
        if self.nav.kind() != egaku::NavKind::Cursor {
            self.nav = Navigator::new(egaku::NavKind::Cursor, egaku::SearchStyle::Jump, self.nav.keymap().clone());
        }
        self.nav.set_viewport(options.len());
        self.nav.set_cursor(a.selected, options.len());
        let answer = match self.nav.handle(key, &options) {
            NavOutcome::Open(i) => i,
            NavOutcome::Dismiss | NavOutcome::Close => 2,
            _ => {
                a.selected = self.nav.cursor();
                return Overlay::Consumed;
            }
        };
        self.approval = None;
        Overlay::Approval(answer)
    }

    /// The footer under the overlay in front, rendered from the keymap.
    #[must_use]
    pub fn nav_footer(&self) -> String {
        let actions = match self.front() {
            Surface::Approval => crate::nav::CHOICE_FOOTER,
            Surface::Picker => crate::nav::PICKER_FOOTER,
            _ => crate::nav::PAGER_FOOTER,
        };
        self.nav.footer(actions)
    }

    /// A finished turn's conversation arrives: new messages unroll.
    pub fn receive_conversation(&mut self, conversation: Conversation, now: std::time::Instant) {
        let from = self.conversation.messages.len().min(conversation.messages.len());
        if self.motion && conversation.messages.len() > from {
            self.reveal = Some((from, now));
        }
        self.conversation = conversation;
    }

    /// Whether anything on screen is still moving at `now` — the event
    /// loop keeps its frame pacer running while this holds.
    #[must_use]
    pub fn animating(&self, now: std::time::Instant) -> bool {
        if !self.motion {
            return self.busy;
        }
        let recent = |t: std::time::Instant, d: std::time::Duration| now.saturating_duration_since(t) < d;
        self.blips.live(now, self.spec.hold)
            || self.activity.current_note(now).is_some()
            || self.busy
            || !self.toasts.is_empty()
            || self.reveal.is_some_and(|(_, t)| recent(t, self.spec.reveal(usize::MAX)))
            || recent(self.surface.1, crate::look::Timing::open())
    }

    /// Show a notice. The same notice arriving again folds into the last
    /// one (`×3`) and restarts its clock rather than stacking copies.
    pub fn toast(&mut self, level: ToastLevel, text: impl Into<String>) {
        self.toast_at(level, text, std::time::Instant::now());
    }

    pub fn toast_at(&mut self, level: ToastLevel, text: impl Into<String>, now: std::time::Instant) {
        let text = text.into();
        if let Some(l) = self.notice_log.last_mut().filter(|t| t.level == level && t.text == text) {
            l.count += 1;
            l.born = now;
        } else {
            self.notice_log.push(Toast { level, text: text.clone(), born: now, count: 1, life: self.spec.lifetime(level) });
            if self.notice_log.len() > NOTICE_LOG {
                self.notice_log.remove(0);
            }
        }
        if let Some(t) = self.toasts.iter_mut().find(|t| t.level == level && t.text == text) {
            t.count += 1;
            t.born = now;
            return;
        }
        self.toasts.push(Toast { level, text, born: now, count: 1, life: self.spec.lifetime(level) });
        if self.toasts.len() > MAX_TOASTS {
            self.toasts.remove(0);
        }
    }

    /// Drop expired toasts; true when any went (a repaint is due).
    pub fn expire_toasts(&mut self, now: std::time::Instant) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|t| t.age(now) < 1.0);
        before != self.toasts.len()
    }

    pub fn set_busy(&mut self, busy: bool) {
        self.set_busy_at(busy, std::time::Instant::now());
    }

    pub fn set_busy_at(&mut self, busy: bool, now: std::time::Instant) {
        if busy && !self.busy {
            self.busy_since = Some(now);
        } else if !busy {
            self.busy_since = None;
            self.activity.settle();
        }
        self.busy = busy;
    }

    /// An error is a toast (the one place notices appear); `last_error`
    /// keeps the latest for anything that asks.
    pub fn set_error(&mut self, err: impl Into<String>) {
        let err = err.into();
        self.toast(ToastLevel::Error, err.clone());
        self.last_error = Some(err);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_idle_with_an_empty_prompt() {
        let s = TuiState::new(Conversation::default(), "deepseek-flash");
        assert!(!s.busy && s.input.editor.is_empty() && !s.show_transcript);
    }

    #[test]
    fn toasts_fold_repeats_cap_and_expire() {
        let mut s = TuiState::new(Conversation::default(), "m");
        let t0 = std::time::Instant::now();
        s.toast_at(ToastLevel::Warn, "deepseek 402 → local", t0);
        s.toast_at(ToastLevel::Warn, "deepseek 402 → local", t0);
        assert_eq!(s.toasts.len(), 1, "a repeat folds into the last toast");
        assert_eq!(s.toasts[0].count, 2);
        for i in 0..6 {
            s.toast_at(ToastLevel::Info, format!("note {i}"), t0);
        }
        assert_eq!(s.toasts.len(), MAX_TOASTS);
        assert_eq!(s.toasts[MAX_TOASTS - 1].text, "note 5", "the newest is kept");
        // Info lives 4 s; at 5 s every info toast has gone.
        assert!(s.expire_toasts(t0 + std::time::Duration::from_secs(5)));
        assert!(s.toasts.is_empty());
    }

    #[test]
    fn an_error_outlives_a_warning_outlives_a_note() {
        assert!(ToastLevel::Error.lifetime() > ToastLevel::Warn.lifetime());
        assert!(ToastLevel::Warn.lifetime() > ToastLevel::Info.lifetime());
    }

    #[test]
    fn surfaces_and_reveal_drive_animation() {
        let mut s = TuiState::new(Conversation::default(), "m");
        let t0 = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let later = t0 + std::time::Duration::from_secs(5);
        s.surface.1 = t0.checked_sub(std::time::Duration::from_secs(5)).unwrap_or(t0);
        assert!(!s.animating(t0), "idle and settled");
        s.input.show_help = true;
        s.note_surface(t0);
        assert_eq!(s.surface, (crate::look::Surface::Help, t0));
        assert!(s.animating(t0) && !s.animating(later));
        let mut c = Conversation::default();
        c.push(arnes_core::Message::assistant_text("hi"));
        s.receive_conversation(c, later);
        assert_eq!(s.reveal, Some((0, later)));
        assert!(s.animating(later));
        s.set_busy_at(true, t0);
        assert_eq!(s.busy_since, Some(t0));
        s.set_busy_at(false, t0);
        assert_eq!(s.busy_since, None);
        s.motion = false;
        s.receive_conversation(Conversation::default(), later);
        assert!(!s.animating(later), "reduced motion: nothing moves when idle");
    }

    #[test]
    fn usage_summary_formats_and_marks_unknown_prices() {
        assert_eq!(UsageView::default().summary(), "", "nothing before the first call");
        let v = UsageView { calls: 2, prompt_tokens: 12_345, output_tokens: 1_200, cache_hit_ratio: 0.81, cost_usd: 0.0042, priced: true };
        assert_eq!(v.summary(), "↑12.3k 81% cached ↓1.2k $0.0042");
        let u = UsageView { priced: false, ..v };
        assert!(u.summary().ends_with("$?"));
    }
}
