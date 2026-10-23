//! The `/mcp` browser — servers, their tools, one tool's detail — as a pure
//! state machine with vim navigation. The runtime builds it from the pool,
//! feeds it keys, and draws [`McpBrowser::rows`]; nothing here does I/O.
//!
//! Keys are egaku's navigation grammar (`crate::nav`), the same on every
//! arnes surface: `j`/`k`, counts, `gg`/`G`, `Ctrl-d`/`u`/`f`/`b`, `H`/`M`/`L`,
//! `l`/`Enter` open, `h`/`Backspace` back, `/` filters a list (Enter keeps
//! it, Esc clears it) and searches the detail pane with `n`/`N`, `y` yanks
//! the row, `q` closes from anywhere, `Esc` backs out one layer and closes
//! at the top.

use crossterm::event::KeyEvent;
use egaku::{NavKey, NavKeymap, NavKind, NavOutcome, Navigator, SearchStyle};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerStatus {
    Connected,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolEntry {
    /// The tool's own name, without the `mcp__<server>__` prefix.
    pub name: String,
    pub wire_name: String,
    pub description: String,
    pub read_only: Option<bool>,
    pub schema: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerEntry {
    pub name: String,
    pub status: ServerStatus,
    pub command: String,
    pub log: Option<String>,
    pub tools: Vec<ToolEntry>,
    /// The last recorded probe, from the vigy store (`up 12ms, 3 tools, 4s ago`).
    pub health: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Servers,
    Tools(usize),
    Detail(usize, usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Normal,
    Selected,
    Muted,
    Ok,
    Err,
    Heading,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub text: String,
    pub kind: RowKind,
}

impl Row {
    fn new(text: impl Into<String>, kind: RowKind) -> Self {
        Self { text: text.into(), kind }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    /// Copy this text to the clipboard.
    Yank(String),
}

/// The browser: three levels, each its own [`Navigator`], so every level
/// keeps its cursor and search and reads keys exactly like every other
/// arnes surface.
#[derive(Debug, Clone)]
pub struct McpBrowser {
    pub servers: Vec<ServerEntry>,
    /// One line about the built-in tools, shown above the server list.
    pub builtin: String,
    pub view: View,
    /// Rows the list shows; the runtime sets it from the terminal.
    pub page: usize,
    server_nav: Navigator,
    tool_nav: Navigator,
    detail_nav: Navigator,
    /// The navigation keymap every view reads (`keys.nav.menu`).
    keymap: NavKeymap,
}

fn list_nav(keymap: &NavKeymap) -> Navigator {
    Navigator::new(NavKind::Cursor, SearchStyle::Filter, keymap.clone())
}

fn pager_nav(keymap: &NavKeymap) -> Navigator {
    Navigator::new(NavKind::Pager, SearchStyle::Jump, keymap.clone())
}

impl McpBrowser {
    #[must_use]
    pub fn new(servers: Vec<ServerEntry>, builtin: String) -> Self {
        Self {
            servers,
            builtin,
            view: View::Servers,
            page: 10,
            server_nav: list_nav(&NavKeymap::menu()),
            tool_nav: list_nav(&NavKeymap::menu()),
            detail_nav: pager_nav(&NavKeymap::menu()),
            keymap: NavKeymap::menu(),
        }
    }

    /// Read keys through `keymap` from now on, in every view.
    pub fn set_keymap(&mut self, keymap: &NavKeymap) {
        for nav in [&mut self.server_nav, &mut self.tool_nav, &mut self.detail_nav] {
            *nav.keymap_mut() = keymap.clone();
        }
        self.keymap = keymap.clone();
    }

    /// [`Self::set_keymap`], by value.
    #[must_use]
    pub fn with_keymap(mut self, keymap: &NavKeymap) -> Self {
        self.set_keymap(keymap);
        self
    }

    /// The selected server row (an index into [`Self::visible_servers`]).
    #[must_use]
    pub fn server_sel(&self) -> usize {
        self.server_nav.cursor()
    }

    /// The selected tool row (an index into [`Self::visible_tools`]).
    #[must_use]
    pub fn tool_sel(&self) -> usize {
        self.tool_nav.cursor()
    }

    /// The live filter of the current list.
    #[must_use]
    pub fn filter(&self) -> &str {
        self.nav().filter()
    }

    /// The navigator of the current view.
    #[must_use]
    pub fn nav(&self) -> &Navigator {
        match self.view {
            View::Servers => &self.server_nav,
            View::Tools(_) => &self.tool_nav,
            View::Detail(..) => &self.detail_nav,
        }
    }

    /// Indices of the servers the filter keeps.
    #[must_use]
    pub fn visible_servers(&self) -> Vec<usize> {
        (0..self.servers.len()).filter(|&i| self.server_nav.matches(&self.servers[i].name)).collect()
    }

    /// Indices of server `s`'s tools the filter keeps.
    #[must_use]
    pub fn visible_tools(&self, s: usize) -> Vec<usize> {
        self.servers.get(s).map_or_else(Vec::new, |srv| {
            (0..srv.tools.len())
                .filter(|&i| self.tool_nav.matches(&srv.tools[i].name) || self.tool_nav.matches(&srv.tools[i].description))
                .collect()
        })
    }

    /// The current view's rows as text: what search matches and yank copies.
    fn texts(&self) -> Vec<String> {
        match self.view {
            View::Servers => self.visible_servers().iter().map(|&i| self.servers[i].name.clone()).collect(),
            View::Tools(s) => self.visible_tools(s).iter().map(|&t| self.servers[s].tools[t].wire_name.clone()).collect(),
            View::Detail(s, t) => self.detail_rows(s, t).into_iter().map(|r| r.text).collect(),
        }
    }

    fn open(&mut self, row: usize) {
        match self.view {
            View::Servers => {
                if let Some(&s) = self.visible_servers().get(row) {
                    // Back lands on this server in the whole list.
                    self.server_nav.clear_search();
                    self.server_nav.set_cursor(s, self.servers.len());
                    self.tool_nav = list_nav(&self.keymap);
                    self.view = View::Tools(s);
                }
            }
            View::Tools(s) => {
                if let Some(&t) = self.visible_tools(s).get(row) {
                    self.detail_nav = pager_nav(&self.keymap);
                    self.view = View::Detail(s, t);
                }
            }
            View::Detail(..) => {}
        }
    }

    fn back(&mut self) -> Outcome {
        match self.view {
            View::Servers => return Outcome::Close,
            View::Tools(_) => self.view = View::Servers,
            View::Detail(s, _) => self.view = View::Tools(s),
        }
        Outcome::Stay
    }

    /// Apply one key.
    pub fn key(&mut self, ke: KeyEvent) -> Outcome {
        match crate::nav::nav_key(&ke) {
            Some(k) => self.nav_key(k),
            None => Outcome::Stay,
        }
    }

    /// Apply one navigation key.
    pub fn nav_key(&mut self, key: NavKey) -> Outcome {
        let texts = self.texts();
        let page = self.page.max(1);
        let nav = match self.view {
            View::Servers => &mut self.server_nav,
            View::Tools(_) => &mut self.tool_nav,
            View::Detail(..) => &mut self.detail_nav,
        };
        nav.set_viewport(page);
        match nav.handle(key, &texts) {
            NavOutcome::Open(row) => self.open(row),
            NavOutcome::Back if self.view != View::Servers => {
                self.back();
            }
            NavOutcome::Dismiss => return self.back(),
            NavOutcome::Close => return Outcome::Close,
            NavOutcome::Yank(row) => return texts.get(row).map_or(Outcome::Stay, |t| Outcome::Yank(t.clone())),
            _ => {}
        }
        Outcome::Stay
    }

    /// The panel title for the current view.
    #[must_use]
    pub fn title(&self) -> String {
        match self.view {
            View::Servers => format!(" MCP · {} server(s) ", self.servers.len()),
            View::Tools(s) => format!(" MCP › {} ", self.servers[s].name),
            View::Detail(s, t) => format!(" MCP › {} › {} ", self.servers[s].name, self.servers[s].tools[t].name),
        }
    }

    /// The key hint (or the live search) for the bottom border, rendered
    /// from the navigation keymap.
    #[must_use]
    pub fn footer(&self) -> String {
        let actions = match self.view {
            View::Servers => crate::nav::ROOT_LIST_FOOTER,
            View::Tools(_) => crate::nav::LIST_FOOTER,
            View::Detail(..) => crate::nav::DETAIL_FOOTER,
        };
        format!(" {} ", self.nav().footer(actions))
    }

    /// Everything to draw for the current view, and the scroll offset that
    /// keeps the cursor on screen in `height` rows.
    #[must_use]
    pub fn rows(&self, height: usize) -> (Vec<Row>, usize) {
        let height = height.max(1);
        match self.view {
            View::Servers => {
                let mut rows = vec![Row::new(self.builtin.clone(), RowKind::Muted)];
                let header = rows.len();
                for (i, &s) in self.visible_servers().iter().enumerate() {
                    let srv = &self.servers[s];
                    let (glyph, detail) = match &srv.status {
                        ServerStatus::Connected => ("✓", format!("{} tools", srv.tools.len())),
                        ServerStatus::Failed(why) => ("✘", why.clone()),
                    };
                    let detail = match &srv.health {
                        Some(h) => format!("{detail}  ·  {h}"),
                        None => detail,
                    };
                    let selected = i == self.server_nav.cursor();
                    let kind = match (&srv.status, selected) {
                        (_, true) => RowKind::Selected,
                        (ServerStatus::Connected, _) => RowKind::Ok,
                        (ServerStatus::Failed(_), _) => RowKind::Err,
                    };
                    let marker = if selected { "❯" } else { " " };
                    rows.push(Row::new(format!("{marker} {glyph} {:<24} {detail}", srv.name), kind));
                }
                let offset = keep_visible(header + self.server_nav.cursor(), height, rows.len());
                (rows, offset)
            }
            View::Tools(s) => {
                let srv = &self.servers[s];
                let mut rows = vec![Row::new(format!("command  {}", srv.command), RowKind::Muted)];
                match &srv.status {
                    ServerStatus::Connected => rows.push(Row::new("status   connected", RowKind::Ok)),
                    ServerStatus::Failed(why) => rows.push(Row::new(format!("status   failed: {why}"), RowKind::Err)),
                }
                if let Some(h) = &srv.health {
                    let kind = if h.starts_with("up") { RowKind::Ok } else { RowKind::Err };
                    rows.push(Row::new(format!("health   {h}"), kind));
                }
                if let Some(log) = &srv.log {
                    rows.push(Row::new(format!("log      {log}"), RowKind::Muted));
                }
                rows.push(Row::new(String::new(), RowKind::Normal));
                let header = rows.len();
                let visible = self.visible_tools(s);
                if visible.is_empty() {
                    rows.push(Row::new("  (no tools)", RowKind::Muted));
                }
                for (i, &t) in visible.iter().enumerate() {
                    let tool = &srv.tools[t];
                    let ro = if tool.read_only == Some(true) { "ro" } else { "  " };
                    let first = tool.description.lines().next().unwrap_or("");
                    let selected = i == self.tool_nav.cursor();
                    let marker = if selected { "❯" } else { " " };
                    rows.push(Row::new(
                        format!("{marker} {ro} {:<32} {first}", tool.name),
                        if selected { RowKind::Selected } else { RowKind::Normal },
                    ));
                }
                let offset = keep_visible(header + self.tool_nav.cursor(), height, rows.len());
                (rows, offset)
            }
            View::Detail(s, t) => {
                let rows = self.detail_rows(s, t);
                let offset = self.detail_nav.offset().min(rows.len().saturating_sub(height));
                (rows, offset)
            }
        }
    }

    fn detail_rows(&self, s: usize, t: usize) -> Vec<Row> {
        let Some(tool) = self.servers.get(s).and_then(|srv| srv.tools.get(t)) else {
            return Vec::new();
        };
        let mut rows = vec![
            Row::new(format!("wire name  {}", tool.wire_name), RowKind::Muted),
            Row::new(
                format!(
                    "read-only  {}",
                    match tool.read_only {
                        Some(true) => "yes (server declares readOnlyHint)",
                        Some(false) => "no",
                        None => "undeclared (gated as may-write)",
                    }
                ),
                RowKind::Muted,
            ),
            Row::new(String::new(), RowKind::Normal),
        ];
        rows.extend(tool.description.lines().map(|l| Row::new(l, RowKind::Normal)));
        rows.push(Row::new(String::new(), RowKind::Normal));
        rows.push(Row::new("parameters", RowKind::Heading));
        let required: Vec<&str> = tool.schema["required"]
            .as_array()
            .map(|a| a.iter().filter_map(serde_json::Value::as_str).collect())
            .unwrap_or_default();
        match tool.schema["properties"].as_object() {
            Some(props) if !props.is_empty() => {
                for (name, p) in props {
                    let ty = p["type"].as_str().map_or_else(|| p["type"].to_string(), str::to_string);
                    let req = if required.contains(&name.as_str()) { " *" } else { "" };
                    let desc = p["description"].as_str().unwrap_or("");
                    rows.push(Row::new(format!("  {name}{req}: {ty}  {desc}"), RowKind::Normal));
                }
            }
            _ => rows.push(Row::new("  (none)", RowKind::Muted)),
        }
        rows.push(Row::new(String::new(), RowKind::Normal));
        rows.push(Row::new("input schema", RowKind::Heading));
        let pretty = serde_json::to_string_pretty(&tool.schema).unwrap_or_default();
        rows.extend(pretty.lines().map(|l| Row::new(format!("  {l}"), RowKind::Muted)));
        rows
    }
}

fn keep_visible(cursor_row: usize, height: usize, total: usize) -> usize {
    let offset = (cursor_row + 1).saturating_sub(height);
    offset.min(total.saturating_sub(height))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crossterm::event::{KeyCode, KeyModifiers};

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn tool(name: &str) -> ToolEntry {
        ToolEntry {
            name: name.into(),
            wire_name: format!("mcp__s__{name}"),
            description: format!("does {name}\nsecond line"),
            read_only: Some(name.starts_with("get")),
            schema: serde_json::json!({
                "type": "object",
                "properties": {"q": {"type": "string", "description": "query"}},
                "required": ["q"]
            }),
        }
    }

    fn browser() -> McpBrowser {
        let servers = vec![
            ServerEntry {
                name: "alpha".into(),
                status: ServerStatus::Connected,
                command: "alpha-mcp".into(),
                log: Some("/log/alpha.log".into()),
                tools: (0..30).map(|i| tool(&format!("get_{i}"))).collect(),
                health: Some("up 12ms, 30 tools, 4s ago".into()),
            },
            ServerEntry {
                name: "beta".into(),
                status: ServerStatus::Failed("timeout".into()),
                command: "beta-mcp".into(),
                log: None,
                tools: vec![],
                health: None,
            },
            ServerEntry {
                name: "gamma".into(),
                status: ServerStatus::Connected,
                command: "gamma-mcp".into(),
                log: None,
                tools: vec![tool("search"), tool("write")],
                health: None,
            },
        ];
        McpBrowser::new(servers, "built-in: Bash Read".into())
    }

    #[test]
    fn jk_move_and_clamp() {
        let mut b = browser();
        b.key(key(KeyCode::Char('k')));
        assert_eq!(b.server_sel(), 0);
        for _ in 0..10 {
            b.key(key(KeyCode::Char('j')));
        }
        assert_eq!(b.server_sel(), 2);
    }

    #[test]
    fn gg_and_g_jump() {
        let mut b = browser();
        b.key(key(KeyCode::Char('G')));
        assert_eq!(b.server_sel(), 2);
        b.key(key(KeyCode::Char('g')));
        assert_eq!(b.server_sel(), 2, "a single g waits for the second");
        b.key(key(KeyCode::Char('g')));
        assert_eq!(b.server_sel(), 0);
    }

    #[test]
    fn open_back_and_detail() {
        let mut b = browser();
        b.key(key(KeyCode::Char('l')));
        assert_eq!(b.view, View::Tools(0));
        b.key(ctrl('d'));
        assert_eq!(b.tool_sel(), 5);
        b.key(key(KeyCode::Enter));
        assert_eq!(b.view, View::Detail(0, 5));
        let (rows, _) = b.rows(100);
        assert!(rows.iter().any(|r| r.text.contains("q *: string  query")));
        assert!(rows.iter().any(|r| r.text.contains("readOnlyHint")));
        b.key(key(KeyCode::Char('h')));
        assert_eq!(b.view, View::Tools(0));
        b.key(key(KeyCode::Esc));
        assert_eq!(b.view, View::Servers);
        assert_eq!(b.key(key(KeyCode::Esc)), Outcome::Close);
    }

    #[test]
    fn q_closes_from_any_depth() {
        let mut b = browser();
        b.key(key(KeyCode::Enter));
        b.key(key(KeyCode::Enter));
        assert_eq!(b.key(key(KeyCode::Char('q'))), Outcome::Close);
    }

    #[test]
    fn recorded_health_shows_on_the_server_row_and_in_its_view() {
        let mut b = browser();
        let (rows, _) = b.rows(100);
        assert!(rows.iter().any(|r| r.text.contains("alpha") && r.text.ends_with("up 12ms, 30 tools, 4s ago")));
        assert!(rows.iter().any(|r| r.text.contains("beta") && r.text.ends_with("timeout")), "no health, row unchanged");
        b.key(key(KeyCode::Char('l')));
        let (rows, _) = b.rows(100);
        let h = rows.iter().find(|r| r.text.starts_with("health")).unwrap();
        assert_eq!((h.text.as_str(), h.kind), ("health   up 12ms, 30 tools, 4s ago", RowKind::Ok));
    }

    #[test]
    fn filter_narrows_and_opens_the_match() {
        let mut b = browser();
        for c in "/gam".chars() {
            b.key(key(KeyCode::Char(c)));
        }
        b.key(key(KeyCode::Enter));
        assert_eq!(b.visible_servers(), vec![2]);
        b.key(key(KeyCode::Char('l')));
        assert_eq!(b.view, View::Tools(2));
        for c in "/wri".chars() {
            b.key(key(KeyCode::Char(c)));
        }
        b.key(key(KeyCode::Enter));
        b.key(key(KeyCode::Char('l')));
        assert_eq!(b.view, View::Detail(2, 1));
    }

    #[test]
    fn filter_typing_does_not_trigger_keys() {
        let mut b = browser();
        for c in "/qjk".chars() {
            assert_eq!(b.key(key(KeyCode::Char(c))), Outcome::Stay);
        }
        assert_eq!(b.filter(), "qjk");
        b.key(key(KeyCode::Esc));
        assert!(b.filter().is_empty());
        assert_eq!(b.visible_servers().len(), 3);
    }

    #[test]
    fn failed_server_shows_reason() {
        let b = browser();
        let (rows, _) = b.rows(100);
        assert!(rows.iter().any(|r| r.kind == RowKind::Err && r.text.contains("timeout")));
    }

    #[test]
    fn cursor_stays_on_screen() {
        let mut b = browser();
        b.key(key(KeyCode::Enter));
        b.key(key(KeyCode::Char('G')));
        let (rows, offset) = b.rows(10);
        let cursor = rows.iter().position(|r| r.kind == RowKind::Selected).unwrap();
        assert!(cursor >= offset && cursor < offset + 10);
    }

    #[test]
    fn detail_scrolls_and_clamps() {
        let mut b = browser();
        b.page = 5;
        b.key(key(KeyCode::Enter));
        b.key(key(KeyCode::Enter));
        b.key(key(KeyCode::Char('G')));
        let (rows, offset) = b.rows(5);
        assert_eq!(offset, rows.len() - 5);
    }
}
