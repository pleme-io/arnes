use arnes_core::{Conversation, Message};
use arnes_tui::{Keymap, TuiState, draw_frame};
use egaku_term::Buffer;

fn screen(buf: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.height() {
        let mut row = String::new();
        for x in 0..buf.width() {
            row.push_str(buf.get(x, y).map_or(" ", |c| c.symbol()));
        }
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

#[test]
fn math_transcript_gallery() {
    let mut c = Conversation::default();
    c.push(Message::user_text("you choose"));
    c.push(Message::assistant_text(include_str!("fixtures/math-transcript.md")));
    let state = TuiState::new(c, "deepseek-flash");
    let mut buf = Buffer::empty(90, 90);
    draw_frame(&mut buf, &state, &Keymap::claude_code());
    let s = screen(&buf);
    if std::env::var_os("ARNES_GALLERY").is_some() {
        println!("{s}");
    }
    assert!(!s.contains("\\int"), "raw TeX leaked:\n{s}");

    let rows: Vec<&str> = s.lines().map(|l| l.trim_start_matches('│').trim_end_matches('│')).collect();
    let find = |needle: &str| rows.iter().position(|r| r.contains(needle)).unwrap_or_else(|| panic!("{needle:?} missing:\n{s}"));
    let col = |row: usize, needle: &str| rows[row].find(needle).map(|b| rows[row][..b].chars().count());
    let blank = |r: &str| r.trim().is_empty();

    let body = find("Sure!")..find("• second");
    assert!(
        rows[body.clone()].windows(2).all(|w| !(blank(w[0]) && blank(w[1]))),
        "a doubled blank row:\n{s}"
    );
    let step = find("Step-by-step");
    assert!(blank(rows[step - 1]) && !blank(rows[step - 2]), "one blank before a block:\n{s}");
    let math = find("∫₀¹ ln(1+x) dx.");
    assert!(!blank(rows[math - 1]), "display math sits under its sentence:\n{s}");

    let find_row = find("Find an antiderivative");
    assert_eq!(col(find_row + 1, "Use"), col(find_row, "Find"), "item continuation hangs under the text:\n{s}");
    let bullet = find("• a bullet");
    let lead = rows[bullet + 1].chars().take_while(|c| *c == ' ').count();
    assert_eq!(Some(lead), col(bullet, "a bullet"), "wrapped bullet hangs under its text:\n{s}");

    let f1 = find("F(1) =");
    assert_eq!(col(f1, "="), col(f1 + 1, "="), "aligned rows share the = column:\n{s}");
    assert!(rows[find("╭─ rust")].contains('╮') && s.contains('╰'), "framed code:\n{s}");
    assert!(rows[find("A blockquote")].contains("▎ A blockquote"), "quote gutter:\n{s}");
}
