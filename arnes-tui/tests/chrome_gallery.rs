//! The chrome and its motion, rendered offscreen at pinned instants.
//! No sleeps: every frame is `draw_frame_at(…, now)`.

use std::time::{Duration, Instant};

use arnes_core::{Conversation, Message};
use arnes_tui::look::{self, Surface};
use arnes_tui::{Keymap, ToastLevel, TuiState, draw_frame_at};
use egaku_term::Buffer;
use egaku_term::crossterm::style::Color;
use egaku_term::theme::ColorDepth;

const MS: fn(u64) -> Duration = Duration::from_millis;

fn frame(st: &TuiState, w: u16, h: u16, now: Instant) -> Buffer {
    let mut b = Buffer::empty(w, h);
    draw_frame_at(&mut b, st, &Keymap::claude_code(), now);
    b
}

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

fn find(buf: &Buffer, needle: &str) -> Option<(u16, u16)> {
    let s = screen(buf);
    s.lines().enumerate().find_map(|(y, l)| {
        l.find(needle).map(|b| (u16::try_from(l[..b].chars().count()).unwrap(), u16::try_from(y).unwrap()))
    })
}

fn settled(st: &mut TuiState, t0: Instant) {
    st.surface = (st.front(), t0.checked_sub(Duration::from_secs(10)).unwrap_or(t0));
}

#[test]
fn the_activity_line_spins_shimmers_and_counts() {
    let t0 = Instant::now();
    let mut st = TuiState::new(Conversation::default(), "m");
    settled(&mut st, t0);
    st.set_busy_at(true, t0);
    let a = frame(&st, 70, 14, t0 + MS(900));
    let b = frame(&st, 70, 14, t0 + MS(980));
    let (x, y) = find(&a, "thinking 0.9s").expect("live line");
    assert_ne!(a.get(x - 2, y).unwrap().symbol(), b.get(x - 2, y).unwrap().symbol(), "the spinner advances");
    let glow = |buf: &Buffer| (0..8).map(|i| buf.get(x + i, y).unwrap().style().fg).collect::<Vec<_>>();
    assert_ne!(glow(&a), glow(&b), "the shimmer moves across the word");
    assert!(screen(&frame(&st, 70, 14, t0 + Duration::from_secs(75))).contains("thinking 1m15s"));
}

#[test]
fn reduced_motion_draws_the_end_state() {
    let t0 = Instant::now();
    let mut st = TuiState::new(Conversation::default(), "m");
    st.motion = false;
    st.set_busy_at(true, t0);
    let a = frame(&st, 70, 14, t0 + MS(900));
    let b = frame(&st, 70, 14, t0 + MS(980));
    assert_eq!(screen(&a), screen(&b), "nothing moves with motion off");
}

#[test]
fn a_notice_slides_along_its_row_then_fades() {
    let t0 = Instant::now();
    let mut st = TuiState::new(Conversation::default(), "m");
    settled(&mut st, t0);
    st.toast_at(ToastLevel::Warn, "fallback to local", t0);
    let arriving = frame(&st, 70, 12, t0);
    let shown = frame(&st, 70, 12, t0 + Duration::from_secs(1));
    let (xa, ya) = find(&arriving, "fallback").expect("in its row from the start");
    let (xs, ys) = find(&shown, "fallback").expect("settled");
    assert_eq!(ya, ys, "it moves along its own row, never across rows");
    assert!(xa > xs, "it slides in from the right");
    assert_eq!(shown.get(xs, ys).unwrap().style().fg, st.palette.warning);
    let fading = frame(&st, 70, 12, t0 + MS(7_600));
    let (xf, yf) = find(&fading, "fallback").unwrap();
    let c = fading.get(xf, yf).unwrap().style().fg;
    assert!(c != st.palette.warning && c != st.palette.muted, "part-way to muted: {c:?}");
}

#[test]
fn a_finished_reply_unrolls() {
    let t0 = Instant::now();
    let mut st = TuiState::new(Conversation::default(), "m");
    settled(&mut st, t0);
    let mut c = Conversation::default();
    c.push(Message::assistant_text((0..12).fold(String::new(), |acc, i| acc + &format!("line {i}\n\n"))));
    st.receive_conversation(c, t0);
    let start = screen(&frame(&st, 60, 40, t0));
    let end = screen(&frame(&st, 60, 40, t0 + Duration::from_secs(2)));
    assert!(start.contains("arnes") && !start.contains("line 11"), "{start}");
    assert!(end.contains("line 0") && end.contains("line 11"), "{end}");
}

#[test]
fn the_picker_rises_as_it_opens() {
    let t0 = Instant::now();
    let mut st = TuiState::new(Conversation::default(), "m");
    st.input.open_mention_picker((0..20).map(|i| format!("src/f{i}.rs")).collect());
    st.note_surface(t0);
    assert_eq!(st.surface.0, Surface::Picker);
    let top = |b: &Buffer| find(b, "@ file").map(|(_, y)| y).unwrap();
    let opening = frame(&st, 70, 30, t0);
    let open = frame(&st, 70, 30, t0 + Duration::from_secs(1));
    assert!(top(&opening) > top(&open), "a shorter picker at first");
}

#[test]
fn the_history_picker_claims_the_picker_rows_at_every_size() {
    let t0 = Instant::now();
    let mut st = TuiState::new(Conversation::default(), "m");
    let recalled = ["cargo test -p arnes-tui", "a prompt\nover two lines", "git status"]
        .iter()
        .enumerate()
        .map(|(n, t)| arnes_store::history::Recalled {
            entry: arnes_store::history::Entry { at: i64::try_from(n).unwrap(), cwd: "/r".into(), text: (*t).into() },
            here: true,
        })
        .collect();
    st.input.load_history(recalled, true);
    st.input.open_history_picker(10);
    settled(&mut st, t0);
    // A terminal too short for picker rows keeps the prompt (the layout's
    // rule for every picker); it must still draw.
    let _ = frame(&st, 24, 8, t0);
    for (w, h) in [(80, 24), (40, 16), (30, 12)] {
        let b = frame(&st, w, h, t0);
        let s = screen(&b);
        assert!(find(&b, "history").is_some(), "{w}x{h}: the picker names itself\n{s}");
        assert!(s.contains("git status"), "{w}x{h}: the newest prompt shows first\n{s}");
    }
    let s = screen(&frame(&st, 80, 24, t0));
    assert!(s.contains("a prompt \u{23ce} over two lines"), "a multi-line prompt is one row\n{s}");
}

#[test]
fn an_opening_panel_eases_its_border_to_the_accent() {
    let t0 = Instant::now();
    let mut st = TuiState::new(Conversation::default(), "m");
    st.input.type_char('?');
    st.note_surface(t0);
    let corner = |b: &Buffer| b.get(0, 0).unwrap().style().fg;
    assert_eq!(corner(&frame(&st, 80, 40, t0)), st.palette.border);
    assert_eq!(corner(&frame(&st, 80, 40, t0 + Duration::from_secs(1))), st.palette.accent);
}

#[test]
fn tema_picks_the_fleet_theme() {
    let t0 = Instant::now();
    let mut nord = TuiState::new(Conversation::default(), "m");
    settled(&mut nord, t0);
    let mut vellum = nord.clone();
    vellum.palette = look::palette(Some("vellum"), ColorDepth::TrueColor).0;
    let fg = |st: &TuiState| frame(st, 60, 12, t0).get(0, 0).unwrap().style().fg;
    assert_ne!(fg(&nord), fg(&vellum));
}

#[test]
fn a_256_colour_terminal_gets_no_24_bit_escapes() {
    let t0 = Instant::now();
    let mut c = Conversation::default();
    c.push(Message::assistant_text("```rust\nfn main() {}\n```"));
    let mut st = TuiState::new(c, "m");
    settled(&mut st, t0);
    let mut b = frame(&st, 60, 14, t0);
    look::project(&mut b, ColorDepth::Ansi256);
    assert!(b.cells().iter().all(|c| !matches!(c.style().fg, Color::Rgb { .. }) && !matches!(c.style().bg, Color::Rgb { .. })));
}

fn busy_everything(t0: Instant) -> TuiState {
    let mut c = Conversation::default();
    for i in 0..30 {
        c.push(Message::user_text(format!("message number {i}")));
    }
    let mut st = TuiState::new(c, "m");
    for i in 0..5 {
        st.toast_at(ToastLevel::Error, format!("notice {i}"), t0);
    }
    st.set_busy_at(true, t0);
    st.input.open_mention_picker((0..20).map(|i| format!("src/f{i}.rs")).collect());
    st.surface = (st.front(), t0.checked_sub(Duration::from_secs(10)).unwrap_or(t0));
    st
}

#[test]
fn no_region_covers_another_at_any_size() {
    let t0 = Instant::now();
    let st = busy_everything(t0);
    let now = t0 + Duration::from_secs(1);
    for (w, h) in [(4u16, 3u16), (10, 5), (20, 8), (40, 12), (80, 24), (120, 50), (200, 70)] {
        let r = arnes_tui::layout::layout(w, h, arnes_tui::chrome::demand(&st, now));
        let all = r.all();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert!(!a.overlaps(b), "{w}x{h}: {a:?} over {b:?}");
            }
        }
        let buf = frame(&st, w, h, now);
        let s = screen(&buf);
        let rows: Vec<&str> = s.lines().collect();
        for (y, line) in rows.iter().enumerate() {
            let in_notices = (r.notices.y..r.notices.y + r.notices.h).contains(&u16::try_from(y).unwrap());
            if line.contains("notice ") {
                assert!(in_notices, "{w}x{h} row {y}: notice drawn outside the lane:\n{s}");
            }
            if line.contains("message number") {
                assert!(u16::try_from(y).unwrap() < r.conversation.y + r.conversation.h, "{w}x{h}: conversation text outside its region");
            }
        }
        if r.conversation.h >= 3 {
            let conv = &rows[usize::from(r.conversation.y)..usize::from(r.conversation.y + r.conversation.h)];
            assert!(conv.iter().any(|l| l.contains("message number 29")), "{w}x{h}: the newest message is never covered:\n{s}");
        }
    }
}

#[test]
fn every_change_is_acknowledged_then_settles() {
    let t0 = Instant::now() + Duration::from_secs(5);
    let mut st = TuiState::new(Conversation::default(), "m");
    settled(&mut st, t0);
    st.observe(t0);
    st.permission_mode = "plan".into();
    st.observe(t0);
    let seg = |b: &Buffer| {
        let (x, y) = find(b, "plan mode").unwrap();
        b.get(x, y).unwrap().style().fg
    };
    assert_eq!(seg(&frame(&st, 90, 14, t0)), st.palette.accent, "lit at the change");
    assert_eq!(seg(&frame(&st, 90, 14, t0 + MS(150))), st.palette.foreground, "settled after the fast duration");

    st.conversation.push(Message::user_text("go"));
    st.observe(t0);
    let corner = |b: &Buffer| {
        let r = arnes_tui::layout::layout(90, 14, arnes_tui::chrome::demand(&st, t0));
        b.get(0, r.prompt.y).unwrap().style().fg
    };
    assert_eq!(corner(&frame(&st, 90, 14, t0)), st.palette.success, "the prompt pulses when a prompt is sent");
    assert_eq!(corner(&frame(&st, 90, 14, t0 + MS(200))), st.palette.accent);

    st.motion = false;
    st.effort = "max".into();
    st.observe(t0);
    let (x, y) = find(&frame(&st, 90, 14, t0), "effort max").unwrap();
    assert_eq!(frame(&st, 90, 14, t0).get(x, y).unwrap().style().fg, st.palette.foreground, "reduced motion: no flash");
}

#[test]
fn tools_tick_and_approvals_answer_in_the_activity_line() {
    use arnes_tui::activity::Event;
    let t0 = Instant::now();
    let mut st = TuiState::new(Conversation::default(), "m");
    settled(&mut st, t0);
    st.set_busy_at(true, t0);
    st.activity_event(Event::ToolStart("mcp__postgres__query".into()), t0);
    let s = screen(&frame(&st, 90, 14, t0 + MS(1_300)));
    assert!(s.contains("query on postgres 1.3s"), "{s}");
    st.activity_event(Event::ToolEnd { name: "mcp__postgres__query".into(), ok: true }, t0 + MS(1_500));
    let s = screen(&frame(&st, 90, 14, t0 + MS(1_600)));
    assert!(s.contains("\u{2713} query 1.5s") && s.contains("thinking"), "{s}");
    assert!(!screen(&frame(&st, 90, 14, t0 + MS(4_000))).contains("\u{2713} query"), "the tick settles away");

    st.answered("Bash", false, t0);
    let s = screen(&frame(&st, 90, 14, t0 + MS(50)));
    assert!(s.contains("denied Bash"), "{s}");
    st.set_busy_at(false, t0);
    st.activity.route = "local llama.cpp \u{b7} free".into();
    let s = screen(&frame(&st, 90, 14, t0 + Duration::from_secs(9)));
    assert!(s.contains("ready \u{b7} local llama.cpp \u{b7} free"), "idle is calm and names the route:\n{s}");
}

#[test]
fn the_approval_question_claims_rows_and_the_conversation_reflows() {
    let t0 = Instant::now();
    let mut c = Conversation::default();
    for i in 0..30 {
        c.push(Message::user_text(format!("message number {i}")));
    }
    let mut st = TuiState::new(c, "m");
    st.approval = Some(arnes_tui::ApprovalView { tool: "Bash".into(), input: "{\"command\":\"ls\"}".into(), selected: 0, warning: None });
    settled(&mut st, t0);
    let s = screen(&frame(&st, 80, 30, t0));
    let rows: Vec<&str> = s.lines().collect();
    let q = rows.iter().position(|l| l.contains("approve?")).unwrap();
    let last = rows.iter().rposition(|l| l.contains("message number 29")).unwrap();
    assert!(last < q, "the newest message sits above the question, uncovered:\n{s}");
}

#[test]
fn the_notice_log_keeps_every_notice_with_counts() {
    let t0 = Instant::now();
    let mut st = TuiState::new(Conversation::default(), "m");
    st.toast_at(ToastLevel::Warn, "retry 1", t0);
    st.toast_at(ToastLevel::Warn, "retry 1", t0);
    st.toast_at(ToastLevel::Error, "gone", t0);
    st.expire_toasts(t0 + Duration::from_secs(60));
    assert!(st.toasts.is_empty());
    assert_eq!(st.notice_lines(), vec!["\u{2718} gone".to_owned(), "\u{25b2} retry 1 \u{d7}2".to_owned()]);
}

#[test]
fn a_crowded_lane_says_how_many_wait() {
    let t0 = Instant::now();
    let st = busy_everything(t0);
    let s = screen(&frame(&st, 100, 40, t0 + Duration::from_secs(1)));
    assert!(s.contains("+1 more \u{b7} /notices"), "four live, three rows: {s}");
}

/// `ARNES_GALLERY=1 cargo test -p arnes-tui --test chrome_gallery -- --nocapture`
/// prints a working scene: a tool running, two notices, a finished tick.
#[test]
fn working_scene_gallery() {
    use arnes_tui::activity::Event;
    let t0 = Instant::now();
    let mut c = Conversation::default();
    c.push(Message::user_text("list the tables and their sizes, please"));
    c.push(Message::assistant_text("Looking at the database now. The first query lists every table with its row count and on-disk size, ordered by size."));
    let mut st = TuiState::new(c, "local");
    st.activity.route = "local llama.cpp \u{b7} free".into();
    settled(&mut st, t0);
    st.set_busy_at(true, t0);
    st.toast_at(ToastLevel::Warn, "deepseek 402: fell back to local", t0);
    st.toast_at(ToastLevel::Error, "MCP unavailable: grafana", t0);
    st.toast_at(ToastLevel::Error, "MCP unavailable: grafana", t0);
    st.activity_event(Event::ToolStart("Read".into()), t0);
    st.activity_event(Event::ToolEnd { name: "Read".into(), ok: true }, t0 + MS(400));
    st.activity_event(Event::ToolStart("mcp__postgres__query".into()), t0 + MS(500));
    let b = frame(&st, 78, 16, t0 + MS(1_300));
    let s = screen(&b);
    if std::env::var_os("ARNES_GALLERY").is_some() {
        println!("{s}");
    }
    assert!(s.contains("query on postgres") && s.contains("\u{d7}2"));
}
