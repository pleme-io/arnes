//! The screen frame (docs/design.md): the grid from [`crate::layout`],
//! and everything drawn in it except the conversation's content.
//!
//! Each region is painted only inside its own rectangle, so nothing is
//! ever drawn over anything else. Every animation reads `now`; with motion
//! off each one is drawn at its end state.

use std::time::Instant;

use egaku_term::anim::{self, Spinner};
use egaku_term::crossterm::style::Color;
use egaku_term::theme::{Palette, blend};
use egaku_term::{Buffer, Style, draw};

use crate::keymap::Keymap;
use crate::layout::{Demand, Region, layout};
use crate::look::{Blip, Timing, opened};
use crate::render::{display_width_until, draw_conversation, draw_pager, inner_cells, text_grapheme, truncate};
use crate::state::{ToastLevel, TuiState};

/// Most rows the prompt grows to before it scrolls.
pub(crate) const PROMPT_MAX_ROWS: u16 = 8;

/// Paint the full arnes UI at the current instant.
pub fn draw_frame(buf: &mut Buffer, state: &TuiState, keymap: &Keymap) {
    draw_frame_at(buf, state, keymap, Instant::now());
}

/// The rows the frame wants this instant (a claimed surface grows open).
#[must_use]
pub fn demand(state: &TuiState, now: Instant) -> Demand {
    #[allow(clippy::cast_possible_truncation)]
    let prompt_rows = (state.input.editor.row_count() as u16).clamp(1, PROMPT_MAX_ROWS);
    let open = if state.motion { opened(state.surface.1, now, Timing::open()) } else { 1.0 };
    let full = if state.approval.is_some() {
        approval_rows(state)
    } else if state.input.picker.is_some() {
        state.spec.picker_rows
    } else {
        0
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let claim = if full == 0 { 0 } else { (3.0 + f32::from(full.saturating_sub(3)) * open).round() as u16 }.min(full);
    Demand { prompt_rows, notices: state.notice_rows(), claim }
}

/// [`draw_frame`] at a given instant: a frame is a pure function of
/// `(state, now)`.
pub fn draw_frame_at(buf: &mut Buffer, state: &TuiState, keymap: &Keymap, now: Instant) {
    let palette = state.palette;
    let r = layout(buf.width(), buf.height(), demand(state, now));
    if buf.width() < 4 {
        return;
    }
    // The surface in front eases its border to the accent as it opens.
    let open = if state.motion { opened(state.surface.1, now, Timing::open()) } else { 1.0 };
    let front = Palette { accent: blend(palette.border, palette.accent, open), ..palette };

    if !r.conversation.is_empty() {
        let c = r.conversation.rect();
        if state.input.show_help {
            let lines = crate::render::help_lines(keymap, state.nav.keymap());
            draw_pager(buf, c, " shortcuts ", &lines, state, &front);
        } else if let Some(b) = &state.mcp {
            draw_mcp(buf, c, b, state, &front, now);
        } else if let Some((title, lines)) = &state.panel {
            draw_pager(buf, c, &format!(" {title} "), lines, state, &front);
        } else {
            draw_conversation(buf, c, state, &palette, now);
        }
    }
    if !r.claim.is_empty() {
        if let Some(a) = &state.approval {
            draw_approval(buf, r.claim, a, state, &palette, now);
        } else if let Some(pp) = &state.input.picker {
            let what = match pp.kind {
                crate::input::PickerKind::Mention => "@ file",
                crate::input::PickerKind::Command => "/ command",
                crate::input::PickerKind::Model => "model",
                crate::input::PickerKind::Choice(command) => command,
                crate::input::PickerKind::History => "history",
            };
            let title = format!(" {what} · {} ", pp.nav.keymap().footer(crate::nav::PICKER_FOOTER));
            draw::picker_with(buf, r.claim.rect(), &pp.picker.view(), &title, &front);
            ease_selection(buf, r.claim, &palette, state.blips.flash(Blip::Pick, now, state.motion));
        }
    }
    draw_notices(buf, r.notices, state, &palette, now);
    draw_activity(buf, r.activity, state, &palette, now);
    if !r.prompt.is_empty() {
        draw_prompt(buf, r.prompt, state, &palette, now);
    }
    draw_status(buf, r.status, state, &palette, now);
}

/// The selected row of a list drawn in `area` (the one on the selection
/// ground) eases in from the plain ground as the selection moves.
fn ease_selection(buf: &mut Buffer, area: Region, palette: &Palette, flash: f32) {
    if flash <= 0.0 {
        return;
    }
    let bg = blend(palette.selection, palette.background, flash);
    for y in area.y..area.y + area.h {
        let on = (area.x + 1..area.x + area.w.saturating_sub(1)).any(|x| buf.get(x, y).is_some_and(|c| c.style().bg == palette.selection));
        if on {
            for x in area.x + 1..area.x + area.w.saturating_sub(1) {
                if let Some(c) = buf.get_mut(x, y).filter(|c| c.style().bg == palette.selection) {
                    c.set_bg(bg);
                }
            }
        }
    }
}

fn approval_rows(state: &TuiState) -> u16 {
    let warn = u16::from(state.approval.as_ref().is_some_and(|a| a.warning.is_some()));
    // Border, tool and input, the warning, the three answers, the key hint.
    2 + 2 + warn + 3 + 1
}

/// The approval question, docked above the notices: tool, input, the
/// guardrail's warning, then the three answers with the chosen one on
/// the selection ground.
fn draw_approval(buf: &mut Buffer, area: Region, a: &crate::state::ApprovalView, state: &TuiState, palette: &Palette, now: Instant) {
    let edge = Palette { accent: palette.warning, ..*palette };
    draw::bordered_block_with(buf, area.rect(), " approve? ", true, &edge);
    let (ix, iy, iw, ih) = inner_cells(draw::block_inner(area.rect()));
    let mut lines: Vec<(String, Style, bool)> = vec![
        (a.tool.clone(), Style::default().fg(palette.foreground).bold(), false),
        (truncate(&a.input, usize::from(iw).saturating_sub(2)), Style::default().fg(palette.muted), false),
    ];
    if let Some(w) = &a.warning {
        lines.push((format!("\u{25b2} {}", truncate(w, usize::from(iw).saturating_sub(4))), Style::default().fg(palette.warning), false));
    }
    for (i, opt) in crate::state::ApprovalView::OPTIONS.iter().enumerate() {
        let sel = i == a.selected;
        let marker = if sel { "\u{276f}" } else { " " };
        lines.push((format!("{marker} {}. {opt}", i + 1), Style::default().fg(if sel { palette.accent } else { palette.foreground }), sel));
    }
    lines.push((format!("1/2/3 or y/a/n \u{b7} {}", state.nav_footer()), Style::default().fg(palette.muted), false));
    let flash = state.blips.flash(Blip::Pick, now, state.motion);
    for (row, (text, st, sel)) in (0..ih).zip(lines) {
        let st = if sel {
            let bg = blend(palette.selection, palette.background, flash);
            buf.blank(ix, iy + row, iw, Style::default().bg(bg));
            st.bg(bg)
        } else {
            st
        };
        buf.set_stringn(ix + 1, iy + row, &text, iw.saturating_sub(1), st);
    }
}

fn level_color(level: ToastLevel, palette: &Palette) -> Color {
    match level {
        ToastLevel::Info => palette.accent,
        ToastLevel::Warn => palette.warning,
        ToastLevel::Error => palette.error,
    }
}

/// The notice lane: one row per live notice, newest lowest, each behind
/// an edge in its severity colour. A notice slides in along its own row
/// and eases into the muted tone over the last 30% of its life. When more
/// are live than the lane holds, the top row says how many wait.
fn draw_notices(buf: &mut Buffer, area: Region, state: &TuiState, palette: &Palette, now: Instant) {
    if area.is_empty() {
        return;
    }
    let live: Vec<_> = state.toasts.iter().filter(|t| t.age(now) < 1.0).collect();
    let shown = usize::from(area.h).min(live.len());
    let hidden = live.len() - shown;
    for (row, t) in (0..area.h).zip(&live[live.len() - shown..]) {
        let y = area.y + row;
        let color = level_color(t.level, palette);
        let (arrive, fade) = if state.motion {
            (opened(t.born, now, Timing::toast_in()), ((t.age(now) - 0.7) / 0.3).clamp(0.0, 1.0))
        } else {
            (1.0, if t.age(now) > 0.7 { 1.0 } else { 0.0 })
        };
        let color = blend(color, palette.muted, fade);
        let count = if t.count > 1 { format!(" \u{d7}{}", t.count) } else { String::new() };
        let more = if row == 0 && hidden > 0 { format!("  +{hidden} more \u{b7} /notices") } else { String::new() };
        let text = format!(" {} {}{count}{more}", t.level.glyph(), t.text);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let x = area.x + ((1.0 - arrive) * f32::from(area.w.min(24))).round() as u16;
        let end = area.x + area.w;
        let edge = buf.set_stringn(x, y, "\u{258e}", end.saturating_sub(x), Style::default().fg(color));
        let body = truncate(&text, usize::from(end.saturating_sub(edge)));
        buf.set_stringn(edge, y, &body, end.saturating_sub(edge), Style::default().fg(color));
    }
}

/// The activity line: what is happening now. Working: a spinner, the
/// current step (thinking, a tool and its server, waiting on an
/// approval), its elapsed time. The last tool to finish ticks at the
/// right. Idle: the route that answers, in the muted tone.
fn draw_activity(buf: &mut Buffer, area: Region, state: &TuiState, palette: &Palette, now: Instant) {
    if area.is_empty() {
        return;
    }
    let (x0, row) = (area.x + 1, area.y);
    let end = x0 + area.w.saturating_sub(2);
    let act = &state.activity;
    let mut x = x0;
    let put = |buf: &mut Buffer, x: u16, s: &str, c: Color| buf.set_stringn(x, row, s, end.saturating_sub(x), Style::default().fg(c));
    if state.busy {
        let since = state.busy_since.unwrap_or(now);
        let elapsed = now.saturating_duration_since(since);
        let frame = if state.motion { Spinner::Braille.frame(elapsed) } else { "\u{2022}" };
        if state.approval.is_some() {
            x = put(buf, x, "\u{25c6}", palette.warning);
            let tool = state.approval.as_ref().map_or("", |p| p.tool.as_str());
            x = put(buf, x, &format!(" waiting on you \u{b7} {}", crate::activity::tool_label(tool).0), palette.warning);
        } else if let Some((name, started)) = act.running.last() {
            x = put(buf, x, frame, palette.accent);
            let (tool, server) = crate::activity::tool_label(name);
            x = put(buf, x, &format!(" {tool}"), palette.foreground);
            if let Some(s) = server {
                x = put(buf, x, &format!(" on {s}"), palette.muted);
            }
            let others = act.running.len() - 1;
            let more = if others > 0 { format!(" +{others}") } else { String::new() };
            x = put(buf, x, &format!(" {}{more}", crate::activity::clock(now.saturating_duration_since(*started))), palette.muted);
        } else {
            x = put(buf, x, frame, palette.accent);
            x = put(buf, x, " ", palette.foreground);
            let word = "thinking";
            let n = word.chars().count();
            for (i, ch) in word.chars().enumerate() {
                let glow = if state.motion { anim::shimmer(i, n, elapsed, state.spec.shimmer) } else { 0.0 };
                if x >= end {
                    break;
                }
                x = buf.set_char(x, row, ch, Style::default().fg(blend(palette.muted, palette.foreground, glow)));
            }
            x = put(buf, x, &format!(" {}", crate::activity::clock(elapsed)), palette.muted);
        }
        x = put(buf, x, " \u{b7} Esc interrupts", palette.muted);
    } else {
        let route = if act.route.is_empty() { state.model.as_str() } else { act.route.as_str() };
        let idle = state.hosted_segments.get("idle").cloned().unwrap_or_else(|| format!("ready \u{b7} {route}"));
        x = put(buf, x, &idle, palette.muted);
    }
    // The transient on the right: an approval answer, a fallback, or the
    // last tool's tick, flashing on arrival then settling to muted.
    let right: Option<(String, Color, Instant)> = act
        .current_note(now)
        .map(|(s, good, t)| (s.to_owned(), if good { palette.success } else { palette.warning }, t))
        .or_else(|| {
            act.current_tick(now).map(|t| {
                let (tool, _) = crate::activity::tool_label(&t.name);
                let mark = if t.ok { "\u{2713}" } else { "\u{2717}" };
                (format!("{mark} {tool} {}", crate::activity::clock(t.took)), if t.ok { palette.success } else { palette.error }, t.at)
            })
        });
    if let Some((text, color, at)) = right {
        let flash = if state.motion { 1.0 - opened(at, now, Timing::fast()) } else { 0.0 };
        let settle = if state.motion { opened(at, now, state.spec.hold) } else { 0.0 };
        let c = blend(blend(color, palette.muted, settle), palette.foreground, flash);
        let tw = u16::try_from(display_width_until(&text, usize::MAX)).unwrap_or(u16::MAX);
        if x + 2 + tw <= end {
            put(buf, end - tw, &text, c);
        }
    }
}

fn draw_status(buf: &mut Buffer, area: Region, state: &TuiState, palette: &Palette, now: Instant) {
    if area.is_empty() {
        return;
    }
    let vim = state.input.vim.as_ref().map_or(String::new(), |v| match v.mode() {
        escriba_core::Mode::Command => format!(" :{}", v.command_line()),
        m => format!(" -- {} --", m.as_str().to_uppercase()),
    });
    let queued = match state.input.queued.len() {
        0 => String::new(),
        n => format!(" \u{b7} queued {n}"),
    };
    // Segments, each with the blip that acknowledges its change. A hint
    // leads: it is what the operator most needs, and a narrow terminal
    // clips the end.
    let mut segs: Vec<(String, Option<Blip>)> = Vec::new();
    if !vim.is_empty() {
        segs.push((vim, Some(Blip::Mode)));
    }
    if let Some(h) = &state.input.hint {
        segs.push((format!("  {h}  \u{b7}"), Some(Blip::Hint)));
    }
    let hosted = |name: &str, kernel: String| state.hosted_segments.get(name).cloned().unwrap_or(kernel);
    segs.push((hosted("model", format!(" {}", state.model)), Some(Blip::Model)));
    if !state.profile.is_empty() {
        segs.push((hosted("profile", format!(" \u{b7} profile {}", state.profile)), None));
    }
    segs.push((hosted("effort", format!(" \u{b7} effort {}", state.effort)), Some(Blip::Effort)));
    segs.push((hosted("tools", format!(" \u{b7} {} tools", state.tools)), None));
    segs.push((hosted("permission-mode", format!(" \u{b7} {} mode", state.permission_mode)), Some(Blip::Permission)));
    for extra in &state.segments {
        segs.push((format!(" \u{b7} {extra}"), None));
    }
    segs.push((queued, None));
    let left: String = segs.iter().map(|(s, _)| s.as_str()).collect();
    let usage = state.usage.summary();
    let right = if usage.is_empty() { " ? for shortcuts ".to_string() } else { format!(" {usage} \u{b7} ? ") };
    draw::status_line_with(buf, area.rect(), &left, &right, palette);
    let mut x = area.x;
    for (s, blip) in &segs {
        let w = u16::try_from(display_width_until(s, usize::MAX)).unwrap_or(0);
        if let Some(b) = blip {
            let f = state.blips.flash(*b, now, state.motion);
            if f > 0.0 {
                for cx in x..(x + w).min(area.x + area.w) {
                    if let Some(c) = buf.get_mut(cx, area.y) {
                        c.set_fg(blend(palette.foreground, palette.accent, f));
                    }
                }
            }
        }
        x += w;
    }
}

fn draw_prompt(buf: &mut Buffer, area: Region, state: &TuiState, palette: &Palette, now: Instant) {
    let r = area.rect();
    let search = state.input.search.as_ref();
    let title = if search.is_some() { " reverse-i-search " } else { " > " };
    // A sent prompt pulses the border.
    let pulse = state.blips.flash(Blip::Submit, now, state.motion);
    let edge = Palette { accent: blend(palette.accent, palette.success, pulse), ..*palette };
    draw::bordered_block_with(buf, r, title, true, &edge);
    let (ix, iy, iw, ih) = inner_cells(draw::block_inner(r));
    if iw == 0 || ih == 0 {
        return;
    }
    if let Some(s) = search {
        let found = s.hit.and_then(|i| state.input.history().get(i)).map_or("", String::as_str);
        let line = format!("`{}': {found}", s.query);
        buf.set_stringn(ix, iy, &line, iw, Style::default().fg(palette.accent));
        return;
    }
    let editor = &state.input.editor;
    let (crow, ccol) = editor.cursor();
    let first = crow.saturating_sub(usize::from(ih) - 1);
    for (i, text) in editor.rows().iter().enumerate().skip(first).take(usize::from(ih)) {
        #[allow(clippy::cast_possible_truncation)]
        let y = iy + (i - first) as u16;
        buf.set_stringn(ix, y, text, iw, Style::default().fg(palette.foreground));
        if i == crow {
            let under: String = text_grapheme(text, ccol).unwrap_or_else(|| " ".into());
            #[allow(clippy::cast_possible_truncation)]
            let x = ix + (display_width_until(text, ccol) as u16).min(iw.saturating_sub(1));
            let cursor = match state.input.vim.as_ref().map(|v| v.host.cursor_shape()) {
                Some(escriba_core::CursorShape::Bar) => Style::default().underlined(),
                _ => Style::default().reversed(),
            };
            buf.set_stringn(x, y, &under, 1, cursor);
        }
    }
}

fn draw_mcp(buf: &mut Buffer, r: egaku::Rect, b: &crate::mcp_browser::McpBrowser, state: &TuiState, palette: &Palette, now: Instant) {
    use crate::mcp_browser::RowKind;
    draw::bordered_block_with(buf, r, &b.title(), true, palette);
    let (ix, iy, iw, ih) = inner_cells(draw::block_inner(r));
    if ih < 2 {
        return;
    }
    let body = ih - 1;
    let (rows, offset) = b.rows(usize::from(body));
    let flash = state.blips.flash(Blip::Scroll, now, state.motion);
    for (i, row) in (0..body).zip(rows.iter().skip(offset)) {
        let st = match row.kind {
            RowKind::Normal => Style::default().fg(palette.foreground),
            RowKind::Selected => Style::default().fg(palette.accent).bold(),
            RowKind::Muted => Style::default().fg(palette.muted),
            RowKind::Ok => Style::default().fg(palette.success),
            RowKind::Err => Style::default().fg(palette.error),
            RowKind::Heading => Style::default().fg(palette.warning).bold(),
        };
        let y = iy + i;
        let st = if row.kind == RowKind::Selected {
            let bg = blend(palette.selection, palette.background, flash);
            buf.blank(ix, y, iw, Style::default().bg(bg));
            st.bg(bg)
        } else {
            st
        };
        buf.set_stringn(ix + 1, y, &row.text, iw.saturating_sub(1), st);
    }
    // The position: muted, lit while the list moves, settling back.
    if rows.len() > usize::from(body) {
        let pos = format!(" {}/{} ", (offset + usize::from(body)).min(rows.len()), rows.len());
        let w = u16::try_from(pos.chars().count()).unwrap_or(0);
        let lit = if state.motion {
            state.blips.at(Blip::Scroll).map_or(0.0, |t| 1.0 - opened(t, now, state.spec.hold))
        } else {
            0.0
        };
        buf.set_stringn(ix + iw.saturating_sub(w + 1), iy + body, &pos, w, Style::default().fg(blend(palette.muted, palette.accent, lit)));
    }
    buf.set_stringn(ix + 1, iy + body, &b.footer(), iw.saturating_sub(12), Style::default().fg(palette.muted));
}
