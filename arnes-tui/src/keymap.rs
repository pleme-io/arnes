//! Keys → actions, as data.
//!
//! The default bindings are Claude Code's (docs/tui-parity.md §4.1–4.2),
//! written in the same `ctrl+x ctrl+e` chord syntax its keybindings.json
//! uses and parsed by `awase` — so a user keymap (T5) reuses this parser
//! and a binding is a row of data, never a `match` arm.
//!
//! Typed text does not go through the keymap: a key with no Ctrl/Alt that
//! produces a character is inserted verbatim (case preserved). Only
//! commands are bound.

use awase::Hotkey;

/// Every command the input layer understands. `name()` is the Claude Code
/// action id (`namespace:action`), the vocabulary user keymaps bind to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    // ── app ──
    /// `Ctrl-C`: interrupt a turn / clear input / exit on second press.
    Interrupt,
    /// `Ctrl-D`: delete forward, or exit on second press when empty.
    Exit,
    /// `Esc`: interrupt a turn; double-press clears input to history.
    Cancel,
    Redraw,
    ToggleTranscript,
    CycleEffort,
    /// `Shift-Tab`: cycle the permission mode.
    CycleMode,
    /// `PageUp`: scroll back through the conversation with the navigation
    /// keys; `G` returns to the live tail.
    Scrollback,
    // ── chat ──
    Submit,
    Newline,
    ExternalEditor,
    Stash,
    // ── history ──
    HistoryPrevious,
    HistoryNext,
    HistorySearch,
    // ── editing (readline; egaku::TextArea) ──
    Left,
    Right,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    DeleteBack,
    DeleteForward,
    KillToEnd,
    KillToStart,
    KillWordBack,
    KillWordForward,
    Yank,
    YankPop,
    Undo,
}

impl Action {
    pub const ALL: [Self; 30] = [
        Self::Interrupt,
        Self::Exit,
        Self::Cancel,
        Self::Redraw,
        Self::ToggleTranscript,
        Self::CycleEffort,
        Self::CycleMode,
        Self::Scrollback,
        Self::Submit,
        Self::Newline,
        Self::ExternalEditor,
        Self::Stash,
        Self::HistoryPrevious,
        Self::HistoryNext,
        Self::HistorySearch,
        Self::Left,
        Self::Right,
        Self::WordLeft,
        Self::WordRight,
        Self::LineStart,
        Self::LineEnd,
        Self::DeleteBack,
        Self::DeleteForward,
        Self::KillToEnd,
        Self::KillToStart,
        Self::KillWordBack,
        Self::KillWordForward,
        Self::Yank,
        Self::YankPop,
        Self::Undo,
    ];

    /// The action whose id is `name` (`chat:submit`).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.name() == name)
    }

    /// Claude Code action id where one exists; `arnes:` for arnes-only ones.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Interrupt => "app:interrupt",
            Self::Exit => "app:exit",
            Self::Cancel => "chat:cancel",
            Self::Redraw => "app:redraw",
            Self::ToggleTranscript => "app:toggleTranscript",
            Self::CycleEffort => "chat:thinkingToggle",
            Self::CycleMode => "app:cycleMode",
            Self::Scrollback => "app:scrollback",
            Self::Submit => "chat:submit",
            Self::Newline => "chat:newline",
            Self::ExternalEditor => "chat:externalEditor",
            Self::Stash => "chat:stash",
            Self::HistoryPrevious => "history:previous",
            Self::HistoryNext => "history:next",
            Self::HistorySearch => "history:search",
            Self::Left => "edit:left",
            Self::Right => "edit:right",
            Self::WordLeft => "edit:wordLeft",
            Self::WordRight => "edit:wordRight",
            Self::LineStart => "edit:lineStart",
            Self::LineEnd => "edit:lineEnd",
            Self::DeleteBack => "edit:deleteBack",
            Self::DeleteForward => "edit:deleteForward",
            Self::KillToEnd => "edit:killToEnd",
            Self::KillToStart => "edit:killToStart",
            Self::KillWordBack => "edit:killWordBack",
            Self::KillWordForward => "edit:killWordForward",
            Self::Yank => "edit:yank",
            Self::YankPop => "edit:yankPop",
            Self::Undo => "edit:undo",
        }
    }
}

/// Claude Code's default bindings (interactive-mode.md, 2026-09-26).
/// Several keys per action where terminals differ (e.g. `Ctrl-_` arrives
/// as `ctrl+7` on legacy terminals; `Shift-Enter` needs the Kitty keyboard
/// protocol, so `alt+enter` and `ctrl+j` are its portable fallbacks).
pub const DEFAULT_BINDINGS: &[(&str, Action)] = &[
    ("ctrl+c", Action::Interrupt),
    ("ctrl+d", Action::Exit),
    ("escape", Action::Cancel),
    ("ctrl+l", Action::Redraw),
    ("ctrl+o", Action::ToggleTranscript),
    ("alt+t", Action::CycleEffort),
    ("shift+tab", Action::CycleMode),
    ("pageup", Action::Scrollback),
    ("shift+up", Action::Scrollback),
    ("enter", Action::Submit),
    ("shift+enter", Action::Newline),
    ("alt+enter", Action::Newline),
    ("ctrl+j", Action::Newline),
    ("ctrl+g", Action::ExternalEditor),
    ("ctrl+x ctrl+e", Action::ExternalEditor),
    ("ctrl+s", Action::Stash),
    ("up", Action::HistoryPrevious),
    ("ctrl+p", Action::HistoryPrevious),
    ("down", Action::HistoryNext),
    ("ctrl+n", Action::HistoryNext),
    ("ctrl+r", Action::HistorySearch),
    ("left", Action::Left),
    ("ctrl+b", Action::Left),
    ("right", Action::Right),
    ("ctrl+f", Action::Right),
    ("alt+b", Action::WordLeft),
    ("alt+left", Action::WordLeft),
    ("alt+f", Action::WordRight),
    ("alt+right", Action::WordRight),
    ("ctrl+a", Action::LineStart),
    ("home", Action::LineStart),
    ("ctrl+e", Action::LineEnd),
    ("end", Action::LineEnd),
    ("backspace", Action::DeleteBack),
    ("ctrl+h", Action::DeleteBack),
    ("delete", Action::DeleteForward),
    ("ctrl+k", Action::KillToEnd),
    ("ctrl+u", Action::KillToStart),
    ("ctrl+w", Action::KillWordBack),
    ("alt+backspace", Action::KillWordBack),
    ("alt+d", Action::KillWordForward),
    ("ctrl+y", Action::Yank),
    ("alt+y", Action::YankPop),
    ("ctrl+_", Action::Undo),
    ("ctrl+7", Action::Undo),
];

/// What a key press resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Action(Action),
    /// First key of a chord — wait for the next.
    Pending,
    Unbound,
}

/// A parsed keymap with chord state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    bindings: Vec<(Vec<Hotkey>, Action)>,
    pending: Option<Hotkey>,
}

impl Keymap {
    /// Parse `(chord, action)` rows.
    ///
    /// # Errors
    /// Names the first chord awase cannot parse.
    pub fn from_rows(rows: &[(&str, Action)]) -> Result<Self, String> {
        let mut bindings = Vec::with_capacity(rows.len());
        for (chord, action) in rows {
            let keys = chord
                .split_whitespace()
                .map(|k| Hotkey::parse(k).map_err(|e| format!("binding {chord:?}: {e}")))
                .collect::<Result<Vec<_>, _>>()?;
            bindings.push((keys, *action));
        }
        Ok(Self { bindings, pending: None })
    }

    fn chord(chord: &str) -> Result<Vec<Hotkey>, String> {
        chord.split_whitespace().map(|k| Hotkey::parse(k).map_err(|e| format!("{chord:?}: {e}"))).collect()
    }

    /// A keymap from `(chord, action id)` rows, as a keymap bidama exports
    /// them (`docs/kernel.md`). A row with an unknown action or a chord that
    /// does not parse is refused alone, named; the rest are bound.
    #[must_use]
    pub fn from_named(rows: &[(String, String)]) -> (Self, Vec<String>) {
        let mut bindings = Vec::with_capacity(rows.len());
        let mut refused = Vec::new();
        for (chord, action) in rows {
            let Some(a) = Action::from_name(action) else {
                refused.push(format!("bind {chord:?}: unknown action {action:?}"));
                continue;
            };
            match Self::chord(chord) {
                Ok(keys) if keys.is_empty() || keys.len() > 2 => refused.push(format!("bind {chord:?}: one or two keys")),
                Ok(keys) => bindings.push((keys, a)),
                Err(e) => refused.push(format!("bind {e}")),
            }
        }
        (Self { bindings, pending: None }, refused)
    }

    /// Apply `keys.prompt`: each unbind removes a chord, then each bind
    /// points a chord at an action (replacing what it meant). Each entry is
    /// refused alone, named, and the rest apply.
    #[must_use]
    pub fn with_edits(mut self, edits: &arnes_config::KeyEdits) -> (Self, Vec<String>) {
        let mut refused = Vec::new();
        for chord in &edits.unbind {
            match Self::chord(chord) {
                Ok(keys) => {
                    let before = self.bindings.len();
                    self.bindings.retain(|(k, _)| *k != keys);
                    if before == self.bindings.len() {
                        refused.push(format!("keys.prompt.unbind {chord:?}: not bound"));
                    }
                }
                Err(e) => refused.push(format!("keys.prompt.unbind {e}")),
            }
        }
        for (chord, action) in &edits.bind {
            let Some(a) = Action::from_name(action) else {
                refused.push(format!("keys.prompt.bind {chord:?}: unknown action {action:?}"));
                continue;
            };
            match Self::chord(chord) {
                Ok(keys) if keys.len() > 2 => refused.push(format!("keys.prompt.bind {chord:?}: at most two keys")),
                Ok(keys) => {
                    self.bindings.retain(|(k, _)| *k != keys);
                    self.bindings.push((keys, a));
                }
                Err(e) => refused.push(format!("keys.prompt.bind {e}")),
            }
        }
        (self, refused)
    }

    /// Claude Code's defaults with the config's `keys.prompt` applied.
    #[must_use]
    pub fn from_config(cfg: &arnes_config::ArnesConfig) -> (Self, Vec<String>) {
        Self::claude_code().with_edits(&cfg.keys.prompt)
    }

    /// Claude Code's defaults. Infallible: a test parses every row.
    #[must_use]
    pub fn claude_code() -> Self {
        Self::from_rows(DEFAULT_BINDINGS).expect("DEFAULT_BINDINGS parse (tested)")
    }

    /// Resolve one key press, advancing chord state.
    pub fn resolve(&mut self, key: Hotkey) -> Resolution {
        if let Some(leader) = self.pending.take() {
            return self
                .bindings
                .iter()
                .find(|(ks, _)| ks.len() == 2 && ks[0] == leader && ks[1] == key)
                .map_or(Resolution::Unbound, |(_, a)| Resolution::Action(*a));
        }
        if let Some((_, a)) = self.bindings.iter().find(|(ks, _)| ks.len() == 1 && ks[0] == key) {
            return Resolution::Action(*a);
        }
        if self.bindings.iter().any(|(ks, _)| ks.len() == 2 && ks[0] == key) {
            self.pending = Some(key);
            return Resolution::Pending;
        }
        Resolution::Unbound
    }

    /// True while the first key of a chord waits for its second.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Abandon a half-typed chord.
    pub fn cancel_pending(&mut self) {
        self.pending = None;
    }

    /// Every chord bound to `action`, rendered — for the help panel.
    #[must_use]
    pub fn chords_for(&self, action: Action) -> Vec<String> {
        self.bindings
            .iter()
            .filter(|(_, a)| *a == action)
            .map(|(ks, _)| ks.iter().map(Hotkey::display).collect::<Vec<_>>().join(" "))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hk(s: &str) -> Hotkey {
        Hotkey::parse(s).unwrap()
    }

    #[test]
    fn edits_rebind_and_unbind_and_refuse_alone() {
        let edits = arnes_config::KeyEdits {
            bind: vec![
                ("ctrl+q".into(), "app:exit".into()),
                ("ctrl+z".into(), "no:such".into()),
                ("enter".into(), "chat:newline".into()),
                ("ctrl+nope".into(), "app:exit".into()),
            ],
            unbind: vec!["ctrl+d".into(), "f13".into()],
        };
        let (mut k, refused) = Keymap::claude_code().with_edits(&edits);
        assert_eq!(k.resolve(hk("ctrl+q")), Resolution::Action(Action::Exit));
        assert_eq!(k.resolve(hk("enter")), Resolution::Action(Action::Newline), "a bind replaces the chord's meaning");
        assert_eq!(k.resolve(hk("ctrl+d")), Resolution::Unbound);
        assert_eq!(k.resolve(hk("ctrl+c")), Resolution::Action(Action::Interrupt), "the rest are untouched");
        assert_eq!(refused.len(), 3, "{refused:?}");
    }

    #[test]
    fn every_default_binding_parses() {
        Keymap::from_rows(DEFAULT_BINDINGS).unwrap();
    }

    #[test]
    fn every_action_has_a_default_binding() {
        // The parity matrix's floor: an Action nobody can reach is a bug.
        for a in Action::ALL {
            assert!(
                DEFAULT_BINDINGS.iter().any(|(_, b)| *b == a),
                "{} has no default binding",
                a.name()
            );
        }
    }

    #[test]
    fn action_names_are_unique() {
        let mut names: Vec<_> = Action::ALL.iter().map(|a| a.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Action::ALL.len());
    }

    #[test]
    fn claude_code_core_keys() {
        let mut k = Keymap::claude_code();
        for (key, action) in [
            ("ctrl+c", Action::Interrupt),
            ("ctrl+d", Action::Exit),
            ("escape", Action::Cancel),
            ("enter", Action::Submit),
            ("shift+enter", Action::Newline),
            ("ctrl+j", Action::Newline),
            ("ctrl+w", Action::KillWordBack),
            ("ctrl+r", Action::HistorySearch),
        ] {
            assert_eq!(k.resolve(hk(key)), Resolution::Action(action), "{key}");
        }
    }

    #[test]
    fn chords_wait_for_their_second_key() {
        let mut k = Keymap::claude_code();
        assert_eq!(k.resolve(hk("ctrl+x")), Resolution::Pending);
        assert_eq!(k.resolve(hk("ctrl+e")), Resolution::Action(Action::ExternalEditor));
        assert_eq!(k.resolve(hk("ctrl+x")), Resolution::Pending);
        assert_eq!(k.resolve(hk("q")), Resolution::Unbound, "a broken chord resolves to nothing");
        assert_eq!(k.resolve(hk("ctrl+e")), Resolution::Action(Action::LineEnd), "and state resets");
    }
}
