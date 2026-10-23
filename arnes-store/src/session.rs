//! Resumable sessions: one `sessions/<id>.json` per TUI session.

use std::path::{Path, PathBuf};

use arnes_core::{Conversation, Role};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::layout::file_safe;

const TITLE_CHARS: usize = 60;

/// What a session is, without its messages: enough to list and pick one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    /// The ledger's session id, so a session joins its usage rows.
    pub id: String,
    pub cwd: PathBuf,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    /// The first line of the first prompt.
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub messages: usize,
}

/// A saved session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    #[serde(flatten)]
    pub meta: SessionMeta,
    pub conversation: Conversation,
}

impl Session {
    #[must_use]
    pub fn new(id: impl Into<String>, cwd: impl Into<PathBuf>, conversation: Conversation, now: DateTime<Utc>) -> Self {
        let mut s = Self {
            meta: SessionMeta { id: id.into(), cwd: cwd.into(), created: now, updated: now, title: String::new(), messages: 0 },
            conversation,
        };
        s.refresh(now);
        s
    }

    fn refresh(&mut self, now: DateTime<Utc>) {
        self.meta.updated = now;
        self.meta.messages = self.conversation.messages.len();
        if self.meta.title.is_empty() {
            self.meta.title = title_of(&self.conversation);
        }
    }
}

fn title_of(c: &Conversation) -> String {
    c.messages
        .iter()
        .filter(|m| m.role == Role::User)
        .map(arnes_core::Message::text)
        .find(|t| !t.trim().is_empty())
        .map(|t| t.trim().lines().next().unwrap_or_default().chars().take(TITLE_CHARS).collect())
        .unwrap_or_default()
}

/// Why a session could not be found or read.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("no saved sessions")]
    None,
    #[error("no session matches {0:?}")]
    NoMatch(String),
    #[error("{query:?} matches {} sessions: {}", .ids.len(), .ids.join(", "))]
    Ambiguous { query: String, ids: Vec<String> },
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{path}: {source}")]
    Parse { path: PathBuf, source: serde_json::Error },
}

/// The session directory.
#[derive(Debug, Clone)]
pub struct Sessions {
    dir: PathBuf,
}

impl Sessions {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    #[must_use]
    pub fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{}.json", file_safe(id)))
    }

    /// Stamp and write `session` atomically: a crash leaves the previous save.
    ///
    /// # Errors
    /// Serialization or I/O.
    pub fn save(&self, session: &mut Session, now: DateTime<Utc>) -> Result<(), SessionError> {
        session.refresh(now);
        let path = self.path(&session.meta.id);
        let json = serde_json::to_vec(session).map_err(|source| SessionError::Parse { path: path.clone(), source })?;
        crate::write_atomic(&path, &json).map_err(|source| SessionError::Io { path, source })
    }

    /// # Errors
    /// Missing, unreadable or not a session.
    pub fn load(&self, id: &str) -> Result<Session, SessionError> {
        read(&self.path(id))
    }

    /// Every readable session, most recently updated first. A file that
    /// does not parse is skipped, so one bad save never hides the rest.
    #[must_use]
    pub fn list(&self) -> Vec<SessionMeta> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mut out: Vec<SessionMeta> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .filter_map(|p| {
                let text = std::fs::read_to_string(&p).ok()?;
                serde_json::from_str::<SessionMeta>(&text)
                    .inspect_err(|e| tracing::debug!(path = %p.display(), error = %e, "sessions: skipped"))
                    .ok()
            })
            .collect();
        out.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| b.id.cmp(&a.id)));
        out
    }

    /// Pick one: `""` or `"last"` is the newest (in `cwd` when given, else
    /// anywhere), a number `n` is the n-th of [`list`](Self::list) counting
    /// from 1, anything else a unique id prefix.
    ///
    /// # Errors
    /// Nothing saved, nothing matching, or a prefix matching several.
    pub fn resolve(&self, query: &str, cwd: Option<&Path>) -> Result<SessionMeta, SessionError> {
        let all = self.list();
        if all.is_empty() {
            return Err(SessionError::None);
        }
        let q = query.trim();
        if q.is_empty() || q == "last" {
            let here = cwd.and_then(|c| all.iter().find(|m| m.cwd == c));
            return Ok(here.unwrap_or(&all[0]).clone());
        }
        if let Ok(n) = q.parse::<usize>()
            && n >= 1
            && q.len() < 8
        {
            return all.get(n - 1).cloned().ok_or_else(|| SessionError::NoMatch(q.into()));
        }
        let hits: Vec<&SessionMeta> = all.iter().filter(|m| m.id.starts_with(q)).collect();
        match hits.as_slice() {
            [] => Err(SessionError::NoMatch(q.into())),
            [one] => Ok((*one).clone()),
            many => Err(SessionError::Ambiguous { query: q.into(), ids: many.iter().map(|m| m.id.clone()).collect() }),
        }
    }
}

fn read(path: &Path) -> Result<Session, SessionError> {
    let text = std::fs::read_to_string(path).map_err(|source| SessionError::Io { path: path.into(), source })?;
    serde_json::from_str(&text).map_err(|source| SessionError::Parse { path: path.into(), source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use arnes_core::{ConversationId, Message};
    use chrono::TimeZone;

    fn at(s: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + s, 0).unwrap()
    }

    fn conv(prompt: &str) -> Conversation {
        let mut c = Conversation::new(ConversationId(1), "m");
        c.push(Message::user_text(prompt));
        c.push(Message::assistant_text("ok"));
        c
    }

    fn saved(store: &Sessions, id: &str, cwd: &str, t: i64) {
        let mut s = Session::new(id, cwd, conv(&format!("prompt {id}\nsecond line")), at(0));
        store.save(&mut s, at(t)).unwrap();
    }

    #[test]
    fn nothing_saved() {
        let d = tempfile::tempdir().unwrap();
        let s = Sessions::new(d.path().join("sessions"));
        assert!(s.list().is_empty());
        assert!(matches!(s.resolve("", None), Err(SessionError::None)));
    }

    #[test]
    fn save_then_load_is_identity() {
        let d = tempfile::tempdir().unwrap();
        let store = Sessions::new(d.path());
        let original = conv("hello");
        let mut s = Session::new("20260928T101010-1", "/w", original.clone(), at(0));
        store.save(&mut s, at(5)).unwrap();
        let back = store.load("20260928T101010-1").unwrap();
        assert_eq!(serde_json::to_value(&back.conversation).unwrap(), serde_json::to_value(&original).unwrap());
        assert_eq!(back.meta.title, "hello");
        assert_eq!(back.meta.messages, 2);
        assert_eq!(back.meta.updated, at(5));
        assert_eq!(back.meta.created, at(0));
    }

    #[test]
    fn resolve_by_recency_cwd_number_and_prefix() {
        let d = tempfile::tempdir().unwrap();
        let store = Sessions::new(d.path());
        saved(&store, "aaa-1", "/x", 10);
        saved(&store, "aab-2", "/y", 20);
        saved(&store, "bbb-3", "/x", 30);
        assert_eq!(store.list().iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["bbb-3", "aab-2", "aaa-1"]);
        assert_eq!(store.resolve("", None).unwrap().id, "bbb-3");
        assert_eq!(store.resolve("last", Some(Path::new("/y"))).unwrap().id, "aab-2");
        assert_eq!(store.resolve("", Some(Path::new("/nowhere"))).unwrap().id, "bbb-3");
        assert_eq!(store.resolve("3", None).unwrap().id, "aaa-1");
        assert_eq!(store.resolve("aab", None).unwrap().id, "aab-2");
        assert!(matches!(store.resolve("aa", None), Err(SessionError::Ambiguous { .. })));
        assert!(matches!(store.resolve("zzz", None), Err(SessionError::NoMatch(_))));
        assert!(matches!(store.resolve("9", None), Err(SessionError::NoMatch(_))));
        assert_eq!(store.load("bbb-3").unwrap().meta.title, "prompt bbb-3");
    }

    #[test]
    fn a_corrupt_file_is_skipped_not_fatal() {
        let d = tempfile::tempdir().unwrap();
        let store = Sessions::new(d.path());
        saved(&store, "good", "/x", 1);
        std::fs::write(d.path().join("bad.json"), "{not json").unwrap();
        assert_eq!(store.list().len(), 1);
        assert!(matches!(store.load("bad"), Err(SessionError::Parse { .. })));
    }
}
