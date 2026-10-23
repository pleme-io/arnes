//! The TUI's session recorder: saves after every turn, restarts on `/clear`,
//! switches on `/resume`.

use std::path::PathBuf;

use arnes_core::Conversation;
use arnes_store::{Layout, Session, Sessions};
use chrono::Utc;

pub struct Recorder {
    store: Sessions,
    current: Session,
    /// Record at all (`:sessions "off"` still allows resuming).
    enabled: bool,
    base: String,
    restarts: u32,
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_default()
}

impl Recorder {
    /// `None` when there is no state directory to keep sessions in.
    pub fn open(enabled: bool, id: &str, conversation: &Conversation) -> Option<Self> {
        let store = Sessions::new(Layout::from_env()?.sessions_dir());
        Some(Self {
            current: Session::new(id, cwd(), conversation.clone(), Utc::now()),
            store,
            enabled,
            base: id.to_owned(),
            restarts: 0,
        })
    }

    /// Save `conversation` as the current session. An empty one is not
    /// worth a file.
    pub fn save(&mut self, conversation: &Conversation) {
        if !self.enabled || conversation.is_empty() {
            return;
        }
        self.current.conversation = conversation.clone();
        if let Err(e) = self.store.save(&mut self.current, Utc::now()) {
            tracing::warn!("session not saved: {e}");
        }
    }

    /// `/clear`: the old session stays on disk; the next save starts a new one.
    pub fn restart(&mut self, conversation: &Conversation) {
        self.restarts += 1;
        let id = format!("{}-{}", self.base, self.restarts);
        self.current = Session::new(id, cwd(), conversation.clone(), Utc::now());
    }

    /// Switch to a saved session; later saves continue its file.
    ///
    /// # Errors
    /// The query names no single readable session.
    pub fn resume(&mut self, query: &str) -> Result<Conversation, String> {
        let here = cwd();
        let meta = self.store.resolve(query, Some(&here)).map_err(|e| e.to_string())?;
        let session = self.store.load(&meta.id).map_err(|e| e.to_string())?;
        let conversation = session.conversation.clone();
        self.current = session;
        Ok(conversation)
    }

    /// The current session's id and title.
    pub fn describe(&self) -> String {
        let t = &self.current.meta.title;
        if t.is_empty() { self.current.meta.id.clone() } else { format!("{} ({t})", self.current.meta.id) }
    }

    /// The newest `n` sessions, one line each, numbered for `/resume <n>`.
    pub fn recent(&self, n: usize) -> Vec<String> {
        let list = self.store.list();
        if list.is_empty() {
            return vec!["no saved sessions".into()];
        }
        list.iter()
            .take(n)
            .enumerate()
            .map(|(i, m)| {
                let title = if m.title.is_empty() { "(untitled)" } else { &m.title };
                format!("{} {}  {}  {} msgs  {}", i + 1, m.updated.format("%m-%d %H:%M"), title, m.messages, m.id)
            })
            .collect()
    }
}
