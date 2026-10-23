//! Terminal rendering for arnes-tui — conversation, status line, prompt.
//!
//! Renders the [`TuiState`] into an [`egaku_term::Buffer`] using the shared
//! `egaku-term` drawers plus typed cell writes. The owner of the terminal
//! drives the buffer lifecycle (reset → draw → diff → flush); this module
//! only paints cells, so it is infallible.
//!
//! The conversation is anchored to its **tail**: lines are laid out, then
//! the newest that fit are shown — new output never falls off the bottom.

use arnes_core::{ContentBlock, Role};
use egaku::Rect;
use egaku_term::crossterm::style::Color;
use egaku_term::anim;
use egaku_term::{Buffer, Style, draw, theme::Palette};
use std::time::Instant;

use crate::state::TuiState;

pub use crate::chrome::{draw_frame, draw_frame_at};


/// One laid-out line of the conversation: styled runs at an indent.
struct Line {
    spans: Vec<(String, Style)>,
    indent: u16,
}

impl Line {
    fn plain(text: impl Into<String>, style: Style, indent: u16) -> Self {
        Self { spans: vec![(text.into(), style)], indent }
    }
}

/// Assistant markdown → styled, wrapped rows. The whole document pass —
/// block structure, spacing, hanging indents, quote gutters, code frames,
/// table wrapping, aligned math — is `mojiban::render_markdown`; this only
/// maps its style table onto the palette through `egaku_term::markdown`.
fn markdown_lines(text: &str, width: u16, indent: u16, palette: &Palette) -> Vec<Line> {
    let doc = mojiban::render_markdown(
        text,
        // One column of right margin so frames and prose never touch the border.
        usize::from(width.saturating_sub(indent + 1).max(1)),
        &mojiban::Theme::default(),
    );
    let styles: Vec<Style> = egaku_term::markdown::styles(&doc, palette);
    doc.rows
        .iter()
        .map(|row| Line {
            spans: row
                .iter()
                .map(|sp| (sp.text().to_owned(), styles.get(usize::from(sp.style())).copied().unwrap_or_default()))
                .collect(),
            indent,
        })
        .collect()
}

fn layout_conversation(state: &TuiState, width: u16, palette: &Palette) -> Vec<Line> {
    let body = usize::from(width.saturating_sub(2));
    let mut out = Vec::new();
    for msg in &state.conversation.messages {
        let (label, color) = role_glyph(msg.role, palette);
        out.push(Line::plain(label, Style::default().fg(color), 0));
        for block in &msg.content {
            match block {
                ContentBlock::Text { text } if msg.role == Role::Assistant => {
                    out.extend(markdown_lines(text, width, 2, palette));
                }
                ContentBlock::Text { text } => {
                    for l in draw::wrap_text(text, width.saturating_sub(2)) {
                        out.push(Line::plain(l, Style::default(), 2));
                    }
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    let summary = serde_json::to_string(input).unwrap_or_default();
                    out.push(Line::plain(
                        format!("● {name} {}", truncate(&summary, body.saturating_sub(4 + name.len()))),
                        Style::default().fg(palette.warning),
                        2,
                    ));
                }
                ContentBlock::ToolResult { content, is_error, .. } if !*is_error && content.contains("\n@@ -") => {
                    // An Edit/Write result carries a unified diff: show it
                    // coloured, like Claude Code — capped when collapsed.
                    let mut lines = content.lines();
                    let head = lines.next().unwrap_or("");
                    out.push(Line::plain(format!("⎿ {head}"), Style::default().fg(palette.success), 2));
                    let rest: Vec<&str> = lines.collect();
                    let shown = if state.show_transcript { rest.len() } else { rest.len().min(12) };
                    for l in &rest[..shown] {
                        let color = match l.chars().next() {
                            Some('+') => palette.success,
                            Some('-') => palette.error,
                            Some('@') => palette.accent,
                            _ => palette.muted,
                        };
                        out.push(Line::plain(*l, Style::default().fg(color), 4));
                    }
                    if shown < rest.len() {
                        out.push(Line::plain(format!("… {} more diff lines (Ctrl-O)", rest.len() - shown), Style::default().fg(palette.muted), 4));
                    }
                }
                ContentBlock::ToolResult { content, is_error, .. } => {
                    let color = if *is_error { palette.error } else { palette.success };
                    let marker = if *is_error { "✘" } else { "⎿" };
                    if state.show_transcript {
                        for l in content.lines() {
                            out.push(Line::plain(format!("{marker} {l}"), Style::default().fg(color), 2));
                        }
                    } else {
                        let n = content.lines().count();
                        let first = content.lines().next().unwrap_or("");
                        out.push(Line::plain(
                            format!("{marker} {} ({n} lines, Ctrl-O to expand)", truncate(first, body.saturating_sub(30))),
                            Style::default().fg(color),
                            2,
                        ));
                    }
                }
                ContentBlock::Thinking { thinking, .. } => {
                    if state.show_transcript {
                        for l in draw::wrap_text(thinking, width.saturating_sub(4)) {
                            out.push(Line::plain(l, Style::default().fg(palette.muted), 4));
                        }
                    } else {
                        out.push(Line::plain("∴ thinking… (Ctrl-O to show)", Style::default().fg(palette.muted), 2));
                    }
                }
                ContentBlock::RedactedThinking { .. } => out.push(Line::plain("∴ [redacted thinking]", Style::default().fg(palette.muted), 2)),
            }
        }
        out.push(Line::plain(String::new(), Style::default(), 0));
    }
    out
}

pub(crate) fn draw_conversation(buf: &mut Buffer, r: Rect, state: &TuiState, palette: &Palette, now: Instant) {
    let mode = if state.show_transcript { " · transcript" } else { "" };
    let title = format!(" arnes · {} message(s){mode} ", state.conversation.len());
    draw::bordered_block_with(buf, r, &title, false, palette);
    let (ix, iy, iw, ih) = inner_cells(draw::block_inner(r));
    if iw == 0 || ih == 0 {
        return;
    }
    let mut lines = layout_conversation(state, iw, palette);
    if let Some((from, since)) = state.reveal {
        lines.truncate(revealed_rows(state, from, since, now, lines.len(), iw, palette));
    }
    // Scrolled back: the navigator's offset, and its footer on the last row.
    let (ih, skip) = if state.is_scrolling() && ih > 1 {
        let body = ih - 1;
        let footer = format!("scrollback · {} · G returns to live", state.nav_footer());
        buf.set_stringn(ix + 1, iy + body, &footer, iw.saturating_sub(1), Style::default().fg(palette.muted));
        (body, state.nav.offset().min(lines.len().saturating_sub(usize::from(body))))
    } else {
        (ih, lines.len().saturating_sub(usize::from(ih)))
    };
    for (row, line) in lines.iter().skip(skip).take(usize::from(ih)).enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let y = iy + row as u16;
        let mut x = ix + line.indent;
        for (text, style) in &line.spans {
            let room = (ix + iw).saturating_sub(x);
            if room == 0 {
                break;
            }
            x = buf.set_stringn(x, y, text, room, *style);
        }
    }
}

/// The shortcut help, as rows: the prompt's bindings, then the navigation
/// grammar every list, menu and pane shares — both read from their keymaps.
#[must_use]
pub fn help_lines(keymap: &crate::keymap::Keymap, nav: &egaku::NavKeymap) -> Vec<String> {
    let mut out: Vec<String> = crate::keymap::Action::ALL
        .iter()
        .map(|&a| format!("{:<28} {}", keymap.chords_for(a).join(", "), a.name()))
        .collect();
    out.push(String::new());
    out.push("lists, menus and panes".to_owned());
    for a in egaku::NavAction::ALL {
        out.push(format!("{:<28} {}", nav.chords_for(a).join(", "), a.name()));
    }
    out
}

/// The conversation as the plain text rows the pane lays out at `width`
/// (the pane's inner width) — what the scrollback searches and yanks.
#[must_use]
pub fn conversation_rows(state: &TuiState, width: u16) -> Vec<String> {
    layout_conversation(state, width, &state.palette)
        .into_iter()
        .map(|l| {
            let mut t = " ".repeat(usize::from(l.indent));
            for (text, _) in &l.spans {
                t.push_str(text);
            }
            t
        })
        .collect()
}

/// A scrollable, searchable text overlay (help, local panels): the rows at
/// the front navigator's offset, matches in the accent, and the footer
/// rendered from the navigation keymap.
pub(crate) fn draw_pager(buf: &mut Buffer, r: Rect, title: &str, lines: &[String], state: &TuiState, palette: &Palette) {
    draw::bordered_block_with(buf, r, title, true, palette);
    let (ix, iy, iw, ih) = inner_cells(draw::block_inner(r));
    if ih < 2 {
        return;
    }
    let body = ih - 1;
    let offset = state.nav.offset().min(lines.len().saturating_sub(usize::from(body)));
    for (i, line) in (0..body).zip(lines.iter().skip(offset)) {
        let hit = state.nav.query().is_some() && state.nav.matches(line);
        let st = if hit { Style::default().fg(palette.accent).bold() } else { Style::default().fg(palette.foreground) };
        buf.set_stringn(ix + 1, iy + i, line, iw.saturating_sub(1), st);
    }
    if lines.len() > usize::from(body) {
        let pos = format!(" {}/{} ", (offset + usize::from(body)).min(lines.len()), lines.len());
        let w = u16::try_from(pos.chars().count()).unwrap_or(0);
        buf.set_stringn(ix + iw.saturating_sub(w + 1), iy + body, &pos, w, Style::default().fg(palette.muted));
    }
    buf.set_stringn(ix + 1, iy + body, &state.nav_footer(), iw.saturating_sub(12), Style::default().fg(palette.muted));
}

/// How many laid-out rows show while messages from `from` on unroll: the
/// rows before them, plus an eased share of theirs.
fn revealed_rows(state: &TuiState, from: usize, since: Instant, now: Instant, total: usize, width: u16, palette: &Palette) -> usize {
    let mut head = state.clone();
    head.conversation.messages.truncate(from);
    head.reveal = None;
    let before = layout_conversation(&head, width, palette).len().min(total);
    let new = total - before;
    let shown = anim::reveal_count(new, now.saturating_duration_since(since), state.spec.reveal(new), anim::Curve::named(anim::EasingKind::Decelerate));
    before + shown
}

pub(crate) fn text_grapheme(s: &str, col: usize) -> Option<String> {
    use unicode_segmentation::UnicodeSegmentation;
    s.graphemes(true).nth(col).map(str::to_owned)
}

pub(crate) fn display_width_until(s: &str, col: usize) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    s.graphemes(true).take(col).count()
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn inner_cells(r: Rect) -> (u16, u16, u16, u16) {
    let to_u16 = |f: f32| f.max(0.0).round().min(f32::from(u16::MAX)) as u16;
    (to_u16(r.x), to_u16(r.y), to_u16(r.width), to_u16(r.height))
}

fn role_glyph(role: Role, palette: &Palette) -> (&'static str, Color) {
    match role {
        Role::User => ("you", palette.accent),
        Role::Assistant => ("arnes", palette.success),
        Role::System => ("sys", palette.muted),
    }
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arnes_core::{Conversation, Message};
    use crate::keymap::Keymap;

    fn screen(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.height() {
            for x in 0..buf.width() {
                out.push_str(buf.get(x, y).map_or(" ", |c| c.symbol()));
            }
            out.push('\n');
        }
        out
    }

    fn state_with(n: usize) -> TuiState {
        let mut c = Conversation::default();
        for i in 0..n {
            c.push(Message::user_text(format!("message number {i}")));
        }
        TuiState::new(c, "deepseek-flash")
    }

    #[test]
    fn newest_messages_stay_on_screen() {
        let mut buf = Buffer::empty(60, 16);
        draw_frame(&mut buf, &state_with(40), &Keymap::claude_code());
        let s = screen(&buf);
        assert!(s.contains("message number 39"), "tail must be visible:\n{s}");
        assert!(!s.contains("message number 0 "), "head scrolls off");
    }

    #[test]
    fn status_line_shows_model_effort_and_help_hint() {
        let mut buf = Buffer::empty(80, 12);
        draw_frame(&mut buf, &state_with(1), &Keymap::claude_code());
        let s = screen(&buf);
        assert!(s.contains("deepseek-flash") && s.contains("effort default") && s.contains("? for shortcuts"));
    }

    #[test]
    fn segments_follow_the_mode_on_the_status_line() {
        let mut st = state_with(0);
        let mut buf = Buffer::empty(100, 12);
        draw_frame(&mut buf, &st, &Keymap::claude_code());
        assert!(!screen(&buf).contains("mcp 1/2"), "no segments, nothing drawn");
        st.segments = vec!["mcp 1/2".into()];
        draw_frame(&mut buf, &st, &Keymap::claude_code());
        let s = screen(&buf);
        assert!(s.lines().any(|l| l.contains("default mode \u{b7} mcp 1/2")), "{s}");
    }

    #[test]
    fn an_error_takes_its_own_lane_row_under_the_conversation() {
        let mut st = state_with(0);
        st.set_error("MCP unavailable: postgres");
        let mut buf = Buffer::empty(80, 12);
        // A second in: it has slid in and not begun to fade.
        let now = st.toasts[0].born + std::time::Duration::from_secs(1);
        draw_frame_at(&mut buf, &st, &Keymap::claude_code(), now);
        let s = screen(&buf);
        let lines: Vec<&str> = s.lines().collect();
        let row = lines.iter().position(|l| l.contains("MCP unavailable: postgres")).expect("in the lane");
        assert!(lines[row - 1].starts_with('\u{2514}'), "directly under the conversation's frame:\n{s}");
        assert!(lines[row].starts_with('\u{258e}'), "behind its severity edge:\n{s}");
        let status = lines.iter().find(|l| l.contains("effort default")).unwrap();
        assert!(!status.contains("postgres"), "notices are not status text: {status}");
    }

    #[test]
    fn help_panel_lists_bindings_from_the_keymap() {
        let mut st = state_with(0);
        st.input.type_char('?');
        let mut buf = Buffer::empty(80, 40);
        draw_frame(&mut buf, &st, &Keymap::claude_code());
        let s = screen(&buf);
        assert!(s.contains("chat:submit") && s.contains("app:interrupt"), "{s}");
    }

    #[test]
    fn multiline_prompt_grows() {
        let mut st = state_with(0);
        st.input.paste("one\ntwo\nthree");
        let mut buf = Buffer::empty(40, 20);
        draw_frame(&mut buf, &st, &Keymap::claude_code());
        let s = screen(&buf);
        assert!(s.contains("one") && s.contains("two") && s.contains("three"));
    }

    #[test]
    fn assistant_markdown_renders_code_blocks_as_rows() {
        let mut c = Conversation::default();
        c.push(Message::assistant_text("Run:\n\n```rust\nfn main() {}\nlet x = 1;\n```\n\nDone **now**."));
        let st = TuiState::new(c, "deepseek-flash");
        let mut buf = Buffer::empty(60, 20);
        draw_frame(&mut buf, &st, &Keymap::claude_code());
        let s = screen(&buf);
        assert!(s.contains("fn main() {}") && s.contains("let x = 1;"), "{s}");
        assert!(!s.contains("```"), "fences are markup, not content");
        assert!(s.contains("Done now."), "bold markers are styling, not text: {s}");
    }

    #[test]
    fn long_assistant_text_wraps_within_the_pane() {
        let mut c = Conversation::default();
        c.push(Message::assistant_text("word ".repeat(60)));
        let st = TuiState::new(c, "m");
        let mut buf = Buffer::empty(40, 30);
        draw_frame(&mut buf, &st, &Keymap::claude_code());
        let rows_with_words = screen(&buf).lines().filter(|l| l.contains("word")).count();
        assert!(rows_with_words > 3, "60 words at width 40 wrap onto several rows");
    }

    #[test]
    fn edit_results_render_as_a_coloured_diff() {
        let mut c = Conversation::default();
        c.push(Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t".into(),
                content: "edited src/a.rs (1 replacement(s))\n@@ -1,1 +1,1 @@\n-old line\n+new line\n".into(),
                is_error: false,
            }],
        });
        let st = TuiState::new(c, "m");
        let mut buf = Buffer::empty(60, 16);
        draw_frame(&mut buf, &st, &Keymap::claude_code());
        let s = screen(&buf);
        assert!(s.contains("edited src/a.rs") && s.contains("-old line") && s.contains("+new line"), "{s}");
    }
}
