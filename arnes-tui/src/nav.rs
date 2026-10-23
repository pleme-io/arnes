//! Navigation keys for every arnes surface, through `egaku::nav`.
//!
//! The grammar (motions, counts, search, open/back/dismiss/close, yank) and
//! the default chords live in egaku, next to the widgets, so arnes and every
//! other egaku-term program read `j`, `gg`, `5j`, `Ctrl-d`, `/`, `n`, `q`,
//! `Esc` and `y` the same way. This module only converts crossterm key
//! events and names which actions each kind of surface advertises in its
//! footer.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use egaku::{NavAction, NavKey};

/// A crossterm key as navigation reads it: an unmodified character keeps its
/// case (`G`, `N`), anything else becomes an awase chord.
#[must_use]
pub fn nav_key(ke: &KeyEvent) -> Option<NavKey> {
    if let KeyCode::Char(c) = ke.code
        && c != ' '
        && !ke.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        return Some(NavKey::Char(c));
    }
    egaku_term::event::to_hotkey(ke).map(NavKey::Chord)
}

/// A navigation keymap with a config section's edits: each unbind removes a
/// chord, then each bind points one at a `nav:*` action. Each entry is refused
/// alone, named by `section` (`keys.nav.menu`), and the rest apply.
#[must_use]
pub fn with_edits(mut keymap: egaku::NavKeymap, edits: &arnes_config::KeyEdits, section: &str) -> (egaku::NavKeymap, Vec<String>) {
    let mut refused = Vec::new();
    for chord in &edits.unbind {
        if !keymap.unbind(chord) {
            refused.push(format!("{section}.unbind {chord:?}: not bound"));
        }
    }
    for (chord, action) in &edits.bind {
        if let Err(e) = keymap.bind_named(chord, action) {
            refused.push(format!("{section}.bind {chord:?}: {e}"));
        }
    }
    (keymap, refused)
}

/// The two navigation keymaps the config describes: `menu` (lists, pagers,
/// dialogs, the scrollback) and `filter` (pickers).
#[must_use]
pub fn keymaps(cfg: &arnes_config::ArnesConfig) -> (egaku::NavKeymap, egaku::NavKeymap, Vec<String>) {
    let (menu, mut refused) = with_edits(egaku::NavKeymap::menu(), &cfg.keys.nav.menu, "keys.nav.menu");
    let (filter, r) = with_edits(egaku::NavKeymap::filter(), &cfg.keys.nav.filter, "keys.nav.filter");
    refused.extend(r);
    (menu, filter, refused)
}

/// Footer actions for a list you open things from.
pub const LIST_FOOTER: &[NavAction] = &[
    NavAction::Down,
    NavAction::Up,
    NavAction::Top,
    NavAction::Bottom,
    NavAction::Open,
    NavAction::Back,
    NavAction::Search,
    NavAction::Yank,
    NavAction::Close,
];

/// Footer actions for a list at the top level (nothing to go back to).
pub const ROOT_LIST_FOOTER: &[NavAction] = &[
    NavAction::Down,
    NavAction::Up,
    NavAction::Top,
    NavAction::Bottom,
    NavAction::Open,
    NavAction::Search,
    NavAction::Yank,
    NavAction::Close,
];

/// Footer actions for scrolling text.
pub const PAGER_FOOTER: &[NavAction] = &[
    NavAction::Down,
    NavAction::Up,
    NavAction::Top,
    NavAction::Bottom,
    NavAction::HalfPageDown,
    NavAction::HalfPageUp,
    NavAction::Search,
    NavAction::SearchNext,
    NavAction::SearchPrev,
    NavAction::Yank,
    NavAction::Close,
];

/// Footer actions for scrolling text with a level above it.
pub const DETAIL_FOOTER: &[NavAction] = &[
    NavAction::Down,
    NavAction::Up,
    NavAction::Top,
    NavAction::Bottom,
    NavAction::Search,
    NavAction::Back,
    NavAction::Yank,
    NavAction::Close,
];

/// Footer actions for a choice dialog.
pub const CHOICE_FOOTER: &[NavAction] = &[NavAction::Down, NavAction::Up, NavAction::Open, NavAction::Dismiss];

/// Footer actions for a picker whose typed letters filter.
pub const PICKER_FOOTER: &[NavAction] = &[NavAction::Down, NavAction::Up, NavAction::Open, NavAction::Dismiss];

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A chord spelled the way binding rows spell it, as the key event a
    /// terminal delivers for it.
    pub(crate) fn event(spelling: &str) -> KeyEvent {
        let mut mods = KeyModifiers::NONE;
        let mut key = spelling;
        if spelling.chars().count() > 1 {
            for part in spelling.split('+') {
                match part {
                    "ctrl" => mods |= KeyModifiers::CONTROL,
                    "alt" => mods |= KeyModifiers::ALT,
                    "shift" => mods |= KeyModifiers::SHIFT,
                    k => key = k,
                }
            }
        }
        let code = match key {
            "escape" | "esc" => KeyCode::Esc,
            "enter" | "return" => KeyCode::Enter,
            "tab" if mods.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
            "tab" => KeyCode::Tab,
            "backspace" => KeyCode::Backspace,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            k => {
                let c = k.chars().next().expect("a key");
                if c.is_uppercase() {
                    mods |= KeyModifiers::SHIFT;
                }
                KeyCode::Char(c)
            }
        };
        KeyEvent::new(code, mods)
    }

    /// A space-separated key sequence as events (`"5 j"`, `"g g"`).
    pub(crate) fn events(seq: &str) -> Vec<KeyEvent> {
        seq.split_whitespace().map(event).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::event;
    use super::*;
    use egaku::{MENU_BINDINGS, FILTER_BINDINGS};

    #[test]
    fn every_default_chord_is_deliverable_from_a_real_key_event() {
        // A chord no key event can produce is a dead key: it shows in the
        // footer and never fires. Drive each default through the converter
        // the runtime uses and check it comes out as the bound key.
        for (chord, action) in MENU_BINDINGS.iter().chain(FILTER_BINDINGS) {
            for part in chord.split_whitespace() {
                let got = nav_key(&event(part));
                assert_eq!(got, NavKey::parse(part).ok(), "{chord} ({}) is dead", action.name());
            }
        }
    }

    #[test]
    fn case_survives_and_space_is_a_chord() {
        assert_eq!(nav_key(&event("G")), Some(NavKey::Char('G')));
        assert_eq!(nav_key(&event("g")), Some(NavKey::Char('g')));
        assert_eq!(nav_key(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)), Some(NavKey::chord("space")));
        assert_eq!(nav_key(&event("ctrl+d")), Some(NavKey::chord("ctrl+d")));
    }
}

/// One chord, one meaning, on every surface: each surface is driven through
/// the key path the runtime uses (`TuiState::overlay_key`, or the picker's
/// `picker_nav_key`) and read back through its own position.
#[cfg(test)]
mod surfaces {
    use super::testing::events;
    use crate::keymap::Keymap;
    use crate::state::{ApprovalView, Overlay, TuiState};
    use arnes_core::{Conversation, Message};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum S {
        Mcp,
        Help,
        Panel,
        Approval,
        Scrollback,
    }

    const ALL: [S; 5] = [S::Mcp, S::Help, S::Panel, S::Approval, S::Scrollback];

    fn state(s: S) -> TuiState {
        let mut c = Conversation::default();
        for i in 0..200 {
            c.push(Message::user_text(format!("message {i}")));
        }
        let mut st = TuiState::new(c, "m");
        st.viewport = (80, 30);
        // Surfaces drawn fully open, not mid-animation.
        st.motion = false;
        match s {
            S::Mcp => {
                let servers = (0..40)
                    .map(|i| crate::ServerEntry {
                        name: format!("server-{i}"),
                        status: crate::ServerStatus::Connected,
                        command: "x".into(),
                        log: None,
                        tools: vec![],
                        health: None,
                    })
                    .collect();
                st.mcp = Some(crate::McpBrowser::new(servers, "built-in".into()));
            }
            S::Help => st.input.show_help = true,
            S::Panel => st.panel = Some(("panel".into(), (0..200).map(|i| format!("line {i}")).collect())),
            S::Approval => {
                st.approval = Some(ApprovalView { tool: "Bash".into(), input: "{}".into(), selected: 0, warning: None });
            }
            S::Scrollback => {
                st.note_surface(std::time::Instant::now());
                assert!(st.enter_scrollback());
                // Start from the top so every surface starts at row 0.
                press(&mut st, "g g");
            }
        }
        st.note_surface(std::time::Instant::now());
        st
    }

    fn press(st: &mut TuiState, keys: &str) -> Overlay {
        let km = Keymap::claude_code();
        let mut last = Overlay::None;
        for ke in events(keys) {
            last = st.overlay_key(&ke, &km);
        }
        last
    }

    /// Where the surface's cursor is (a selected row, or a pager's top row).
    fn pos(st: &TuiState, s: S) -> usize {
        match s {
            S::Mcp => st.mcp.as_ref().map_or(usize::MAX, crate::McpBrowser::server_sel),
            S::Approval => st.approval.as_ref().map_or(usize::MAX, |a| a.selected),
            _ => st.nav.cursor(),
        }
    }

    fn is_open(st: &TuiState, s: S) -> bool {
        match s {
            S::Mcp => st.mcp.is_some(),
            S::Help => st.input.show_help,
            S::Panel => st.panel.is_some(),
            S::Approval => st.approval.is_some(),
            S::Scrollback => st.is_scrolling(),
        }
    }

    #[test]
    fn j_and_k_move_one_row_everywhere() {
        for s in ALL {
            let mut st = state(s);
            press(&mut st, "j");
            assert_eq!(pos(&st, s), 1, "{s:?}: j");
            press(&mut st, "k");
            assert_eq!(pos(&st, s), 0, "{s:?}: k");
            press(&mut st, "down");
            assert_eq!(pos(&st, s), 1, "{s:?}: down");
            press(&mut st, "ctrl+p");
            assert_eq!(pos(&st, s), 0, "{s:?}: ctrl+p");
        }
    }

    #[test]
    fn counts_and_jumps_mean_the_same_everywhere() {
        // Not the approval dialog: its digits answer it (its own keys come first).
        for s in [S::Mcp, S::Help, S::Panel, S::Scrollback] {
            let mut st = state(s);
            press(&mut st, "2 j");
            assert_eq!(pos(&st, s), 2, "{s:?}: 2j");
            press(&mut st, "g g");
            assert_eq!(pos(&st, s), 0, "{s:?}: gg");
        }
        // G goes to the end; on the scrollback the end is the live tail.
        for s in [S::Mcp, S::Help, S::Panel, S::Approval] {
            let mut st = state(s);
            press(&mut st, "G");
            assert!(pos(&st, s) > 1, "{s:?}: G");
        }
        let mut st = state(S::Scrollback);
        press(&mut st, "G");
        assert!(!st.is_scrolling(), "G on the scrollback returns to the live tail");
    }

    #[test]
    fn half_pages_move_everywhere_there_is_a_page() {
        for s in [S::Mcp, S::Help, S::Panel, S::Scrollback] {
            let mut st = state(s);
            press(&mut st, "ctrl+d");
            let half = pos(&st, s);
            assert!(half > 1, "{s:?}: ctrl+d moved {half}");
            press(&mut st, "ctrl+u");
            assert_eq!(pos(&st, s), 0, "{s:?}: ctrl+u");
        }
    }

    #[test]
    fn q_closes_everywhere() {
        for s in ALL {
            let mut st = state(s);
            press(&mut st, "q");
            assert!(!is_open(&st, s), "{s:?}: q");
        }
    }

    /// A search each text surface's rows contain.
    fn query(s: S) -> &'static str {
        if s == S::Help { "/ c t r l enter" } else { "/ 1 9 enter" }
    }

    #[test]
    fn escape_backs_out_a_search_then_closes() {
        for s in [S::Help, S::Panel, S::Scrollback] {
            let mut st = state(s);
            press(&mut st, query(s));
            assert!(st.nav.query().is_some(), "{s:?}: / committed a search");
            press(&mut st, "escape");
            assert!(is_open(&st, s), "{s:?}: first esc clears the search");
            press(&mut st, "escape");
            assert!(!is_open(&st, s), "{s:?}: second esc closes");
        }
        for s in [S::Mcp, S::Approval] {
            let mut st = state(s);
            press(&mut st, "escape");
            assert!(!is_open(&st, s), "{s:?}: esc at the top closes");
        }
    }

    #[test]
    fn n_finds_the_next_match_on_every_text_surface() {
        for s in [S::Help, S::Panel, S::Scrollback] {
            let mut st = state(s);
            press(&mut st, query(s));
            let first = pos(&st, s);
            press(&mut st, "n");
            assert!(pos(&st, s) > first, "{s:?}: n");
            press(&mut st, "N");
            assert_eq!(pos(&st, s), first, "{s:?}: N");
        }
    }

    #[test]
    fn y_yanks_everywhere_but_the_approval_dialog() {
        for s in [S::Mcp, S::Help, S::Panel, S::Scrollback] {
            let mut st = state(s);
            press(&mut st, "y");
            assert!(st.clipboard.as_deref().is_some_and(|t| !t.is_empty()), "{s:?}: y");
        }
        // There `y` is Claude Code's "yes", which comes first.
        let mut st = state(S::Approval);
        assert_eq!(press(&mut st, "y"), Overlay::Approval(0));
    }

    #[test]
    fn enter_picks_the_selected_approval_option() {
        let mut st = state(S::Approval);
        assert_eq!(press(&mut st, "j j enter"), Overlay::Approval(2));
        let mut st = state(S::Approval);
        assert_eq!(press(&mut st, "j l"), Overlay::Approval(1));
        let mut st = state(S::Approval);
        assert_eq!(press(&mut st, "2"), Overlay::Approval(1), "the number keys still answer");
    }

    #[test]
    fn a_typed_letter_leaves_a_panel_and_reaches_the_prompt() {
        let mut st = state(S::Panel);
        assert_eq!(press(&mut st, "x"), Overlay::Pass);
        assert!(st.panel.is_none());
        let mut st = state(S::Scrollback);
        assert_eq!(press(&mut st, "x"), Overlay::Pass);
        assert!(!st.is_scrolling());
    }

    #[test]
    fn pickers_move_with_ctrl_j_k_and_type_letters() {
        use crate::nav::nav_key;
        let mut st = TuiState::new(Conversation::default(), "m");
        st.input.open_model_picker((0..20).map(|i| (format!("m{i}"), format!("model {i}"))).collect());
        let sel = |st: &TuiState| st.input.picker.as_ref().unwrap().picker.view().selected;
        for (key, want) in [("ctrl+j", 1), ("ctrl+n", 2), ("tab", 3), ("down", 4), ("ctrl+k", 3), ("ctrl+p", 2), ("up", 1), ("shift+tab", 0)] {
            let k = nav_key(&super::testing::event(key)).unwrap();
            assert!(st.input.picker_nav_key(k).is_some(), "{key} is a picker key");
            assert_eq!(sel(&st), want, "{key}");
        }
        for c in ["j", "k", "q", "G", "5"] {
            let k = nav_key(&super::testing::event(c)).unwrap();
            assert!(st.input.picker_nav_key(k).is_none(), "{c} is typed, not a motion");
        }
        let k = nav_key(&super::testing::event("end")).unwrap();
        st.input.picker_nav_key(k);
        assert_eq!(sel(&st), 19, "end clamps to the last row");
        let k = nav_key(&super::testing::event("escape")).unwrap();
        st.input.picker_nav_key(k);
        assert!(st.input.picker.is_none(), "esc dismisses");
    }

    #[test]
    fn a_reload_rebinds_keys_and_retheme_in_place() {
        use egaku_term::theme::ColorDepth;
        let mut st = state(S::Help);
        st.input.set_text("half-typed prompt");
        let before = st.conversation.messages.len();
        press(&mut st, "3 j");
        let cfg = arnes_config::ArnesConfig {
            ui: arnes_config::UiSection { theme: Some("vellum".into()), motion: Some("off".into()), ..Default::default() },
            keys: arnes_config::KeysSection {
                nav: arnes_config::NavKeysSection {
                    menu: arnes_config::KeyEdits { bind: vec![("x".into(), "nav:close".into())], unbind: vec!["q".into()] },
                    filter: arnes_config::KeyEdits::default(),
                },
                prompt: arnes_config::KeyEdits { bind: vec![("ctrl+q".into(), "app:exit".into())], unbind: vec![] },
            },
            ..arnes_config::ArnesConfig::default()
        };
        let old_palette = st.palette;
        let (mut km, refused) = st.apply_config(&cfg, ColorDepth::TrueColor);
        assert!(refused.is_empty(), "{refused:?}");
        assert!(st.input.show_help && pos(&st, S::Help) == 3, "the open surface and its position stay");
        assert_eq!(st.input.editor.text(), "half-typed prompt");
        assert_eq!(st.conversation.messages.len(), before);
        assert_ne!((st.palette.background, st.palette.accent), (old_palette.background, old_palette.accent), "the theme changed");
        assert!(!st.motion);
        let close = st.nav.keymap().chords_for(egaku::NavAction::Close);
        assert!(close.contains(&"x".to_owned()) && !close.contains(&"q".to_owned()), "{close:?}");
        press(&mut st, "x");
        assert!(!st.input.show_help, "x closes now");
        assert_eq!(km.resolve(awase::Hotkey::parse("ctrl+q").unwrap()), crate::Resolution::Action(crate::Action::Exit));
        // A picker opened after the reload reads the filter keymap too.
        let cfg = arnes_config::ArnesConfig {
            keys: arnes_config::KeysSection {
                nav: arnes_config::NavKeysSection { filter: arnes_config::KeyEdits { bind: vec![], unbind: vec!["tab".into()] }, ..Default::default() },
                ..Default::default()
            },
            ..arnes_config::ArnesConfig::default()
        };
        let _ = st.apply_config(&cfg, ColorDepth::TrueColor);
        st.input.open_model_picker(vec![("a".into(), "a".into()), ("b".into(), "b".into())]);
        let tab = crate::nav::nav_key(&super::testing::event("tab")).unwrap();
        assert!(st.input.picker_nav_key(tab).is_none(), "tab no longer moves the picker");
    }

    #[test]
    fn a_bad_binding_is_refused_alone() {
        let cfg = arnes_config::ArnesConfig {
            keys: arnes_config::KeysSection {
                nav: arnes_config::NavKeysSection {
                    menu: arnes_config::KeyEdits { bind: vec![("x".into(), "nav:nothing".into()), ("z".into(), "nav:top".into())], unbind: vec![] },
                    ..Default::default()
                },
                ..Default::default()
            },
            ui: arnes_config::UiSection { theme: Some("solarized".into()), ..Default::default() },
            ..arnes_config::ArnesConfig::default()
        };
        let mut st = state(S::Help);
        let (_, refused) = st.apply_config(&cfg, egaku_term::theme::ColorDepth::TrueColor);
        assert_eq!(refused.len(), 2, "{refused:?}");
        press(&mut st, "5 j");
        press(&mut st, "z");
        assert_eq!(pos(&st, S::Help), 0, "the good binding applied beside the bad one");
    }

    mod footers {
        use super::*;
        use egaku_term::Buffer;

        fn screen(st: &TuiState) -> String {
            let (w, h) = st.viewport;
            let mut buf = Buffer::empty(w, h);
            crate::draw_frame(&mut buf, st, &Keymap::claude_code());
            let mut out = String::new();
            for y in 0..buf.height() {
                for x in 0..buf.width() {
                    out.push_str(buf.get(x, y).map_or(" ", |c| c.symbol()));
                }
                out.push('\n');
            }
            out
        }

        #[test]
        fn every_surface_shows_the_same_footer_from_the_keymap() {
            for s in ALL {
                let st = state(s);
                let shown = screen(&st);
                assert!(shown.contains("j/k move"), "{s:?} footer missing:\n{shown}");
            }
        }

        #[test]
        fn a_rebound_key_shows_in_the_footer() {
            let mut st = state(S::Help);
            st.nav.keymap_mut().unbind("j");
            let shown = screen(&st);
            assert!(shown.contains("down/k move"), "{shown}");
        }

        #[test]
        fn the_search_prompt_replaces_the_footer_while_typing() {
            let mut st = state(S::Panel);
            press(&mut st, "/ l i n");
            assert!(screen(&st).contains("/lin"), "{}", screen(&st));
        }

        #[test]
        fn help_lists_the_navigation_chords_and_scrolls() {
            let mut st = state(S::Help);
            press(&mut st, "G");
            let shown = screen(&st);
            assert!(shown.contains("nav:yank"), "the last rows are the navigation grammar:\n{shown}");
        }

        #[test]
        fn a_picker_title_comes_from_its_keymap() {
            let mut st = TuiState::new(Conversation::default(), "m");
            st.viewport = (80, 30);
            st.motion = false;
            st.input.open_model_picker(vec![("a".into(), "a".into())]);
            assert!(screen(&st).contains("down/up move"), "{}", screen(&st));
        }
    }
}
