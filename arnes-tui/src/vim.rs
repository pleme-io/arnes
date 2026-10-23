//! Vim over the prompt — executes escriba's composed actions on an
//! `egaku::TextArea`, via escriba's shared vim crates.
//!
//! Nothing here re-derives vim. The vocabulary (`Action`, `Motion`,
//! `Operator`, `TextObject`, `Mode`, `CursorShape`) is `escriba-core`; the
//! single-line resolver with its eight tested vim rules is `unsoku`; the key
//! layer that turns keystrokes into these actions is escriba's shared key
//! pipeline. This module is only the **host**: it applies each action to
//! the prompt buffer and reports what the TUI must do (history, submit,
//! exit, `$EDITOR`).
//!
//! Rows vs line: unsoku works on one line (the cursor's row, adapted by
//! [`RowTarget`]); motions with no single-line reading (`j` `k` `gg` `G`) and
//! linewise operators (`dd`, `dj`, `yy`, `p` of a linewise yank) are row
//! operations on the `TextArea`, decided by escriba's own
//! `Motion::is_linewise` so the linewise rule matches escriba exactly.

use std::ops::Range;

use egaku::TextArea;
use escriba_core::{Action, CursorShape, InsertAt, Mode, Motion, Operator, TextObject};
use escriba_keymap::{Key, KeyPipeline};
use escriba_mode::ModalState;
use unsoku::{Register, Stance, TextTarget};

/// unsoku's `TextTarget` over the `TextArea`'s current row.
pub struct RowTarget<'a>(pub &'a mut TextArea);

impl TextTarget for RowTarget<'_> {
    fn text(&self) -> &str {
        self.0.current_row()
    }
    fn caret(&self) -> usize {
        self.0.caret_byte()
    }
    fn set_caret(&mut self, at: usize) {
        self.0.set_caret_byte(at);
    }
    fn replace(&mut self, range: Range<usize>, with: &str) {
        self.0.replace_in_row(range, with);
    }
}

/// What the TUI must do after a vim action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VimOutcome {
    None,
    /// `k` on the first row / `j` on the last walks prompt history
    /// (blzsh vicmd convention).
    HistoryPrevious,
    HistoryNext,
    /// `:w` / Enter in Normal: send the prompt.
    Submit,
    /// `:q` / `:q!` / `ZZ`-style quit.
    Quit,
    /// `v` in Normal opens `$EDITOR` (blzsh's `edit-command-line`).
    OpenEditor,
    /// An action with no meaning in a one-prompt buffer (splits, buffers…).
    Unsupported(String),
}

/// The last yank or delete — charwise (unsoku's register) or whole rows.
#[derive(Debug, Clone, Default)]
enum Yanked {
    #[default]
    Empty,
    Chars,
    Rows(Vec<String>),
}

/// Modal state plus registers for one prompt.
#[derive(Debug, Clone, Default)]
pub struct VimHost {
    pub modal: ModalState,
    chars: Register,
    yanked: Yanked,
}

impl VimHost {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn mode(&self) -> Mode {
        self.modal.mode()
    }

    /// Cursor shape for the mode — steady block in Normal, bar in Insert,
    /// matching blzsh and frost-zle (escriba's `Mode::cursor_shape`).
    #[must_use]
    pub fn cursor_shape(&self) -> CursorShape {
        self.modal.mode().cursor_shape()
    }

    /// Apply one composed `(action, count)` step.
    pub fn execute(&mut self, editor: &mut TextArea, action: &Action, count: u32) -> VimOutcome {
        let n = count.max(1) as usize;
        match action {
            Action::Move(m) => self.motion(editor, *m, n),
            Action::ApplyOperator { op, motion } => {
                self.operate_motion(editor, *op, *motion, n);
                VimOutcome::None
            }
            Action::ApplyOperatorObject { op, object } => {
                self.operate_object(editor, *op, *object);
                VimOutcome::None
            }
            Action::ChangeMode(Mode::Visual | Mode::VisualLine) => VimOutcome::Unsupported(
                "visual mode arrives with multi-row unsoku (docs/tui-parity.md §4.8.1)".into(),
            ),
            Action::ChangeMode(mode) => {
                self.modal.enter(*mode);
                if *mode == Mode::Normal {
                    self.clamp(editor);
                }
                VimOutcome::None
            }
            Action::EnterInsert(at) => {
                self.enter_insert(editor, *at);
                VimOutcome::None
            }
            Action::InsertChar(c) => {
                editor.insert_char(*c);
                VimOutcome::None
            }
            Action::Backspace => {
                editor.delete_back();
                VimOutcome::None
            }
            Action::DeleteForward => {
                editor.delete_forward();
                VimOutcome::None
            }
            Action::DeleteWordBefore => {
                editor.kill_word_back();
                VimOutcome::None
            }
            Action::DeleteToLineStart => {
                editor.kill_to_row_start();
                VimOutcome::None
            }
            Action::Put { before } => {
                self.put(editor, *before, n);
                VimOutcome::None
            }
            Action::ReplaceChar(c) => {
                self.replace_chars(editor, *c, n);
                VimOutcome::None
            }
            Action::Undo => {
                for _ in 0..n {
                    editor.undo();
                }
                self.clamp(editor);
                VimOutcome::None
            }
            Action::Save => VimOutcome::Submit,
            Action::Quit => VimOutcome::Quit,
            Action::Pending => VimOutcome::None,
            Action::Redo => VimOutcome::Unsupported("redo is not available in the prompt".into()),
            _ => VimOutcome::Unsupported("not available in the prompt".into()),
        }
    }

    // ── motions ────────────────────────────────────────────────────

    fn motion(&mut self, editor: &mut TextArea, m: Motion, n: usize) -> VimOutcome {
        match m {
            Motion::Up => {
                if editor.on_first_row() {
                    return VimOutcome::HistoryPrevious;
                }
                for _ in 0..n {
                    editor.move_up();
                }
            }
            Motion::Down => {
                if editor.on_last_row() {
                    return VimOutcome::HistoryNext;
                }
                for _ in 0..n {
                    editor.move_down();
                }
            }
            Motion::DocStart => editor.move_to_start(),
            Motion::DocEnd => {
                editor.move_to_end();
                editor.move_to_row_start();
            }
            other => {
                let text = editor.current_row().to_owned();
                if let Some(at) = unsoku::resolve(&text, editor.caret_byte(), other, n) {
                    editor.set_caret_byte(at);
                }
            }
        }
        self.clamp(editor);
        VimOutcome::None
    }

    /// Normal mode cannot rest past the last character (unsoku rule 7).
    fn clamp(&self, editor: &mut TextArea) {
        if self.modal.mode() == Mode::Insert {
            return;
        }
        let text = editor.current_row().to_owned();
        let at = unsoku::clamp(&text, editor.caret_byte(), Stance::Normal);
        editor.set_caret_byte(at);
    }

    // ── operators ──────────────────────────────────────────────────

    fn operate_motion(&mut self, editor: &mut TextArea, op: Operator, motion: Motion, n: usize) {
        if motion.is_linewise() {
            self.operate_rows(editor, op, motion, n);
            return;
        }
        let text = editor.current_row().to_owned();
        let Some(span) = unsoku::operated_span(&text, editor.caret_byte(), op, motion, n) else {
            return;
        };
        self.operate_span(editor, op, span);
    }

    fn operate_object(&mut self, editor: &mut TextArea, op: Operator, object: TextObject) {
        let text = editor.current_row().to_owned();
        if let Some(span) = unsoku::object_span(&text, editor.caret_byte(), object) {
            self.operate_span(editor, op, span);
        }
    }

    fn operate_span(&mut self, editor: &mut TextArea, op: Operator, span: Range<usize>) {
        let mut target = RowTarget(editor);
        match op {
            Operator::Yank => unsoku::yank(&mut target, span, &mut self.chars),
            Operator::Change => {
                unsoku::take(&mut target, span, &mut self.chars);
                self.modal.enter_insert();
            }
            // Delete, and any operator a prompt has no special reading for
            // (indent, format…), removes the span — the text still lands in
            // the register, so nothing typed is lost.
            _ => {
                unsoku::take(&mut target, span, &mut self.chars);
            }
        }
        self.yanked = Yanked::Chars;
        self.clamp(editor);
    }

    /// Rows covered by a linewise motion from the cursor.
    fn row_span(editor: &TextArea, motion: Motion, n: usize) -> Range<usize> {
        let row = editor.cursor().0;
        let last = editor.row_count() - 1;
        match motion {
            Motion::Up => row.saturating_sub(n)..row + 1,
            Motion::Down => row..(row + n).min(last) + 1,
            Motion::DocStart => 0..row + 1,
            Motion::DocEnd => row..last + 1,
            // `dd` is `d` + LinewiseDown with count n: n rows from here.
            _ => row..(row + n).min(last + 1),
        }
    }

    fn operate_rows(&mut self, editor: &mut TextArea, op: Operator, motion: Motion, n: usize) {
        let span = Self::row_span(editor, motion, n);
        let rows: Vec<String> = editor.rows()[span.clone()].to_vec();
        self.yanked = Yanked::Rows(rows);
        if op == Operator::Yank {
            return;
        }
        // Park the cursor on the first row of the span, then remove it.
        while editor.cursor().0 > span.start {
            editor.move_up();
        }
        for _ in span.clone() {
            editor.delete_row();
        }
        if op == Operator::Change {
            if span.start >= editor.row_count() || editor.row_count() == 1 && !editor.is_empty() {
                editor.insert_row_below("");
            } else {
                editor.insert_row_above("");
            }
            self.modal.enter_insert();
        }
        self.clamp(editor);
    }

    fn put(&mut self, editor: &mut TextArea, before: bool, n: usize) {
        match &self.yanked {
            Yanked::Empty => {}
            Yanked::Chars => {
                let mut target = RowTarget(editor);
                unsoku::paste(&mut target, &self.chars, !before, n);
            }
            Yanked::Rows(rows) => {
                let rows = rows.clone();
                for _ in 0..n {
                    for (i, r) in rows.iter().enumerate() {
                        if before && i == 0 {
                            editor.insert_row_above(r);
                        } else {
                            editor.insert_row_below(r);
                        }
                    }
                }
            }
        }
    }

    fn replace_chars(&mut self, editor: &mut TextArea, c: char, n: usize) {
        let text = editor.current_row().to_owned();
        let start = editor.caret_byte();
        let mut end = start;
        for _ in 0..n {
            if end >= text.len() {
                return; // vim: `3rx` on 2 chars left does nothing
            }
            end = unsoku::next_char(&text, end);
        }
        let with: String = std::iter::repeat_n(c, n).collect();
        editor.replace_in_row(start..end, &with);
        editor.set_caret_byte(start + with.len() - c.len_utf8());
    }

    fn enter_insert(&mut self, editor: &mut TextArea, at: InsertAt) {
        match at {
            InsertAt::Caret => {}
            InsertAt::AfterCaret => {
                if !editor.current_row().is_empty() {
                    editor.move_right();
                }
            }
            InsertAt::FirstNonBlank => {
                let fnb = unsoku::first_non_blank(editor.current_row());
                editor.set_caret_byte(fnb);
            }
            InsertAt::LineEnd => editor.move_to_row_end(),
            InsertAt::OpenBelow => editor.insert_row_below(""),
            InsertAt::OpenAbove => editor.insert_row_above(""),
        }
        self.modal.enter_insert();
    }
}

/// Vim for the prompt, end to end: escriba's shared key pipeline turns
/// keystrokes into composed actions (counts, `gg`/`iw` sequences, pending
/// `f`/`t`/`r`), and [`VimHost`] executes them. Command mode (`:`) collects
/// a line that escriba-command's ex parser resolves — `:w`, `:q`, `:q!`…
#[derive(Debug, Clone, Default)]
pub struct VimInput {
    pub host: VimHost,
    pipeline: KeyPipeline,
}

impl VimInput {
    #[must_use]
    pub fn new() -> Self {
        Self { host: VimHost::new(), pipeline: KeyPipeline::default_vim() }
    }

    #[must_use]
    pub fn mode(&self) -> Mode {
        self.host.mode()
    }

    /// The `:` command line being typed.
    #[must_use]
    pub fn command_line(&self) -> &str {
        self.host.modal.minibuffer()
    }

    /// A half-typed operator / sequence / object — for a showcmd hint.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.pipeline.is_pending() || self.host.modal.pending_count().is_some()
    }

    /// Feed one key; return what the TUI must do (empty when handled).
    pub fn key(&mut self, editor: &mut TextArea, key: &Key) -> Vec<VimOutcome> {
        let steps = self.pipeline.feed(&mut self.host.modal, key);
        steps
            .into_iter()
            .map(|(action, n)| self.step(editor, &action, n))
            .filter(|o| *o != VimOutcome::None)
            .collect()
    }

    fn step(&mut self, editor: &mut TextArea, action: &Action, n: u32) -> VimOutcome {
        let modal = &mut self.host.modal;
        if modal.mode() == Mode::Command {
            match action {
                Action::InsertChar(c) => {
                    modal.push_minibuffer(*c);
                    return VimOutcome::None;
                }
                Action::Backspace => {
                    if modal.pop_minibuffer().is_none() {
                        modal.enter(Mode::Normal);
                    }
                    return VimOutcome::None;
                }
                Action::SubmitCommand => {
                    let line = modal.minibuffer().to_owned();
                    modal.clear_minibuffer();
                    modal.enter(Mode::Normal);
                    return self.ex(editor, &line);
                }
                _ => {}
            }
        }
        if matches!(action, Action::ChangeMode(Mode::Command)) {
            modal.clear_minibuffer();
            modal.enter(Mode::Command);
            return VimOutcome::None;
        }
        self.host.execute(editor, action, n)
    }

    /// An ex line, resolved by escriba's verb table (abbreviations and `!`
    /// included). For a prompt, "write" means send: `:w` submits; `:wq` /
    /// `:x` also submit rather than exit, since leaving before the reply
    /// arrives has no use — a named divergence (docs/tui-parity.md §4.9).
    fn ex(&mut self, editor: &mut TextArea, line: &str) -> VimOutcome {
        let Some(inv) = escriba_command::ex::parse(line) else { return VimOutcome::None };
        match inv.command.as_str() {
            "save" | "write-quit" | "exit-write" | "write-quit-all" => VimOutcome::Submit,
            "quit" | "quit!" | "quit-all" | "quit-all!" => VimOutcome::Quit,
            "undo" => self.host.execute(editor, &Action::Undo, 1),
            other => VimOutcome::Unsupported(format!(":{other} is not available in the prompt")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, col: usize) -> TextArea {
        let mut t = TextArea::with_text(text);
        t.move_to_row_start();
        for _ in 0..col {
            t.move_right();
        }
        t
    }

    fn run(v: &mut VimHost, t: &mut TextArea, steps: &[(Action, u32)]) {
        for (a, n) in steps {
            v.execute(t, a, *n);
        }
    }

    #[test]
    fn dw_de_and_counts_follow_unsokus_rules() {
        let mut v = VimHost::new();
        let mut t = at("one two three", 0);
        v.execute(&mut t, &Action::ApplyOperator { op: Operator::Delete, motion: Motion::WordStartNext }, 1);
        assert_eq!(t.text(), "two three");
        v.execute(&mut t, &Action::ApplyOperator { op: Operator::Delete, motion: Motion::WordEndNext }, 1);
        assert_eq!(t.text(), " three", "de is inclusive");
        let mut t = at("a b c d", 0);
        v.execute(&mut t, &Action::ApplyOperator { op: Operator::Delete, motion: Motion::WordStartNext }, 3);
        assert_eq!(t.text(), "d");
    }

    #[test]
    fn ciw_changes_the_word_and_enters_insert() {
        let mut v = VimHost::new();
        let mut t = at("fix the bug", 5);
        v.execute(&mut t, &Action::ApplyOperatorObject { op: Operator::Change, object: TextObject::Word { around: false } }, 1);
        assert_eq!(t.text(), "fix  bug");
        assert_eq!(v.mode(), Mode::Insert);
        assert_eq!(v.cursor_shape(), CursorShape::Bar);
        v.execute(&mut t, &Action::InsertChar('a'), 1);
        assert_eq!(t.text(), "fix a bug");
    }

    #[test]
    fn x_then_p_swaps_characters() {
        let mut v = VimHost::new();
        let mut t = at("ab", 0);
        v.execute(&mut t, &Action::ApplyOperator { op: Operator::Delete, motion: Motion::Right }, 1);
        v.execute(&mut t, &Action::Put { before: false }, 1);
        assert_eq!(t.text(), "ba", "xp — unsoku rule 8");
    }

    #[test]
    fn dd_yy_p_are_linewise() {
        let mut v = VimHost::new();
        let mut t = TextArea::with_text("one\ntwo\nthree");
        t.move_to_start();
        v.execute(&mut t, &Action::ApplyOperator { op: Operator::Delete, motion: Motion::LinewiseDown }, 1);
        assert_eq!(t.rows(), ["two", "three"]);
        v.execute(&mut t, &Action::Put { before: false }, 1);
        assert_eq!(t.rows(), ["two", "one", "three"]);
        v.execute(&mut t, &Action::ApplyOperator { op: Operator::Yank, motion: Motion::LinewiseDown }, 2);
        v.execute(&mut t, &Action::Put { before: true }, 1);
        // yy with count 2 on "one" yanks ["one", "three"]; P puts them above.
        assert_eq!(t.rows(), ["two", "one", "three", "one", "three"]);
    }

    #[test]
    fn dj_deletes_two_rows() {
        let mut v = VimHost::new();
        let mut t = TextArea::with_text("a\nb\nc");
        t.move_to_start();
        v.execute(&mut t, &Action::ApplyOperator { op: Operator::Delete, motion: Motion::Down }, 1);
        assert_eq!(t.rows(), ["c"]);
    }

    #[test]
    fn k_on_the_first_row_and_j_on_the_last_walk_history() {
        let mut v = VimHost::new();
        let mut t = TextArea::with_text("only");
        assert_eq!(v.execute(&mut t, &Action::Move(Motion::Up), 1), VimOutcome::HistoryPrevious);
        assert_eq!(v.execute(&mut t, &Action::Move(Motion::Down), 1), VimOutcome::HistoryNext);
        let mut m = TextArea::with_text("a\nb");
        assert_eq!(v.execute(&mut m, &Action::Move(Motion::Up), 1), VimOutcome::None);
        assert_eq!(m.cursor().0, 0);
    }

    #[test]
    fn normal_mode_clamps_to_the_last_character() {
        let mut v = VimHost::new();
        let mut t = TextArea::with_text("abc"); // cursor after 'c' (insert-style)
        v.execute(&mut t, &Action::ChangeMode(Mode::Normal), 1);
        assert_eq!(t.cursor(), (0, 2));
        assert_eq!(v.cursor_shape(), CursorShape::Block);
    }

    #[test]
    fn insert_positions_and_open_lines() {
        let mut v = VimHost::new();
        let mut t = at("  code", 4);
        v.execute(&mut t, &Action::EnterInsert(InsertAt::FirstNonBlank), 1);
        assert_eq!(t.cursor(), (0, 2));
        v.execute(&mut t, &Action::ChangeMode(Mode::Normal), 1);
        v.execute(&mut t, &Action::EnterInsert(InsertAt::OpenBelow), 1);
        assert_eq!(t.rows(), ["  code", ""]);
        assert_eq!(t.cursor(), (1, 0));
    }

    #[test]
    fn r_replaces_count_characters() {
        let mut v = VimHost::new();
        let mut t = at("abcd", 1);
        v.execute(&mut t, &Action::ReplaceChar('x'), 2);
        assert_eq!(t.text(), "axxd");
        v.execute(&mut t, &Action::ReplaceChar('y'), 9);
        assert_eq!(t.text(), "axxd", "overrun does nothing, as in vim");
    }

    #[test]
    fn save_and_quit_become_outcomes() {
        let mut v = VimHost::new();
        let mut t = TextArea::new();
        assert_eq!(v.execute(&mut t, &Action::Save, 1), VimOutcome::Submit);
        assert_eq!(v.execute(&mut t, &Action::Quit, 1), VimOutcome::Quit);
    }

    #[test]
    fn undo_restores_after_an_operator() {
        let mut v = VimHost::new();
        let mut t = at("keep this", 0);
        run(&mut v, &mut t, &[(Action::ApplyOperator { op: Operator::Delete, motion: Motion::LineEnd }, 1), (Action::Undo, 1)]);
        assert_eq!(t.text(), "keep this");
    }

    // ── keys through escriba's pipeline ──

    fn keys(v: &mut VimInput, t: &mut TextArea, ks: &str) -> Vec<VimOutcome> {
        let mut out = Vec::new();
        for c in ks.chars() {
            let k = match c {
                '\x1b' => Key::Esc,
                '\r' => Key::Enter,
                c => Key::Char(c),
            };
            out.extend(v.key(t, &k));
        }
        out
    }

    fn normal(text: &str) -> (VimInput, TextArea) {
        let mut v = VimInput::new();
        let mut t = TextArea::with_text(text);
        v.host.execute(&mut t, &Action::ChangeMode(Mode::Normal), 1);
        t.move_to_row_start();
        (v, t)
    }

    #[test]
    fn dw_and_counts_via_the_pipeline() {
        let (mut v, mut t) = normal("one two three four");
        keys(&mut v, &mut t, "dw");
        assert_eq!(t.text(), "two three four");
        keys(&mut v, &mut t, "2dw");
        assert_eq!(t.text(), "four");
    }

    #[test]
    fn ciw_then_typing_then_esc() {
        let (mut v, mut t) = normal("fix the bug");
        keys(&mut v, &mut t, "wciw");
        assert_eq!(v.mode(), Mode::Insert);
        // Insert-mode text goes through the Claude Code path in the TUI;
        // here the host inserts directly.
        v.host.execute(&mut t, &Action::InsertChar('a'), 1);
        keys(&mut v, &mut t, "\x1b");
        assert_eq!(t.text(), "fix a bug");
        assert_eq!(v.mode(), Mode::Normal);
    }

    #[test]
    fn colon_q_quits_and_colon_w_submits() {
        let (mut v, mut t) = normal("hello");
        assert_eq!(keys(&mut v, &mut t, ":q\r"), [VimOutcome::Quit]);
        assert_eq!(keys(&mut v, &mut t, ":w\r"), [VimOutcome::Submit]);
        assert_eq!(keys(&mut v, &mut t, ":q!\r"), [VimOutcome::Quit]);
        assert_eq!(v.mode(), Mode::Normal);
        assert_eq!(t.text(), "hello", "command-line typing never touches the prompt");
    }

    #[test]
    fn unknown_ex_command_is_named() {
        let (mut v, mut t) = normal("x");
        let out = keys(&mut v, &mut t, ":vsplit\r");
        assert!(matches!(&out[..], [VimOutcome::Unsupported(m)] if m.contains("vsplit")), "{out:?}");
    }

    #[test]
    fn command_line_is_visible_while_typing_and_backspace_leaves_it() {
        let (mut v, mut t) = normal("x");
        keys(&mut v, &mut t, ":wq");
        assert_eq!(v.mode(), Mode::Command);
        assert_eq!(v.command_line(), "wq");
        for _ in 0..3 {
            v.key(&mut t, &Key::Backspace);
        }
        assert_eq!(v.mode(), Mode::Normal, "backspace past the colon leaves command mode");
    }

    #[test]
    fn gg_and_k_history_handoff() {
        let (mut v, mut t) = normal("a\nb\nc");
        keys(&mut v, &mut t, "G");
        assert_eq!(t.cursor().0, 2);
        keys(&mut v, &mut t, "gg");
        assert_eq!(t.cursor().0, 0);
        assert_eq!(keys(&mut v, &mut t, "k"), [VimOutcome::HistoryPrevious]);
    }

    #[test]
    fn esc_cancels_a_half_typed_operator() {
        let (mut v, mut t) = normal("keep these words");
        keys(&mut v, &mut t, "d\x1bw");
        assert_eq!(t.text(), "keep these words", "d<Esc>w must not delete (escriba c7c3f29)");
    }
}
