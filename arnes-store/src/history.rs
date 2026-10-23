//! Prompt history: every submitted prompt, one JSON line each, in
//! `$XDG_STATE_HOME/arnes/history.jsonl`.
//!
//! One file for every directory, each line carrying its `cwd`, the same way a
//! session header does: the file is shared by every arnes process (appends
//! are one `write` of one line), and [`Scope`] decides which lines a session
//! walks. A line that does not parse is skipped by itself; the rest load.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One submitted prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Unix seconds when it was submitted.
    pub at: i64,
    /// The working directory it was submitted in.
    pub cwd: PathBuf,
    pub text: String,
}

/// Which lines a session recalls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    /// This directory's prompts only.
    Project,
    /// Every directory's, in one timeline.
    Global,
    /// This directory's first (newest nearest), then every other
    /// directory's beyond them.
    #[default]
    ProjectThenGlobal,
}

impl Scope {
    pub const NAMES: [&'static str; 3] = ["project-then-global", "project", "global"];

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "project" => Some(Self::Project),
            "global" => Some(Self::Global),
            "project-then-global" => Some(Self::ProjectThenGlobal),
            _ => None,
        }
    }
}

/// A recalled prompt, in walking order, with whether it came from this
/// directory (the picker ranks those first under `ProjectThenGlobal`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recalled {
    pub entry: Entry,
    pub here: bool,
}

/// Every entry in `path`, oldest first, the newest `keep` of them (0 = all).
/// A missing file is an empty history; an unparsable line is skipped alone.
#[must_use]
pub fn load(path: &Path, keep: usize) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let mut entries: Vec<Entry> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| match serde_json::from_str(l) {
            Ok(e) => Some(e),
            Err(e) => {
                tracing::debug!(error = %e, "history: skipped a line that does not parse");
                None
            }
        })
        .collect();
    if keep > 0 && entries.len() > keep {
        entries.drain(..entries.len() - keep);
    }
    entries
}

/// Append one entry as one line.
///
/// # Errors
/// When the file cannot be created or written.
pub fn append(path: &Path, entry: &Entry) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
    line.push('\n');
    std::fs::OpenOptions::new().create(true).append(true).open(path)?.write_all(line.as_bytes())
}

/// Rewrite `path` to its newest `keep` lines once it holds half as many
/// again, so the file stays bounded without a rewrite on every start.
/// Returns how many lines were dropped.
///
/// # Errors
/// When the file cannot be read or replaced.
pub fn compact(path: &Path, keep: usize) -> std::io::Result<usize> {
    if keep == 0 {
        return Ok(0);
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= keep + keep / 2 {
        return Ok(0);
    }
    let dropped = lines.len() - keep;
    let mut out = lines[dropped..].join("\n");
    out.push('\n');
    crate::write_atomic(path, out.as_bytes())?;
    Ok(dropped)
}

/// `entries` (oldest first) narrowed to `scope` for a session in `cwd`, in
/// walking order: the last element is the one `Up` recalls first.
#[must_use]
pub fn scoped(entries: Vec<Entry>, cwd: &Path, scope: Scope) -> Vec<Recalled> {
    let tag = |entry: Entry| Recalled { here: entry.cwd == cwd, entry };
    match scope {
        Scope::Global => entries.into_iter().map(tag).collect(),
        Scope::Project => entries.into_iter().filter(|e| e.cwd == cwd).map(tag).collect(),
        Scope::ProjectThenGlobal => {
            let (here, elsewhere): (Vec<Entry>, Vec<Entry>) = entries.into_iter().partition(|e| e.cwd == cwd);
            elsewhere.into_iter().chain(here).map(tag).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(at: i64, cwd: &str, text: &str) -> Entry {
        Entry { at, cwd: cwd.into(), text: text.into() }
    }

    #[test]
    fn appends_load_back_in_order_and_a_bad_line_is_skipped_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/history.jsonl");
        append(&path, &e(1, "/a", "first")).unwrap();
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"not json\n\n").unwrap();
        append(&path, &e(2, "/a", "two\nlines")).unwrap();
        assert_eq!(load(&path, 0), [e(1, "/a", "first"), e(2, "/a", "two\nlines")]);
        assert_eq!(load(&path, 1), [e(2, "/a", "two\nlines")], "keep takes the newest");
        assert!(load(&dir.path().join("missing"), 0).is_empty());
    }

    #[test]
    fn compaction_waits_for_half_again_then_keeps_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        for i in 0..15 {
            append(&path, &e(i, "/a", &format!("p{i}"))).unwrap();
        }
        assert_eq!(compact(&path, 10).unwrap(), 0, "15 = 10 + 10/2 is still inside the slack");
        append(&path, &e(15, "/a", "p15")).unwrap();
        assert_eq!(compact(&path, 10).unwrap(), 6);
        let left = load(&path, 0);
        assert_eq!((left.len(), left[0].text.as_str(), left[9].text.as_str()), (10, "p6", "p15"));
        assert_eq!(compact(&path, 0).unwrap(), 0, "0 keeps everything");
        assert_eq!(compact(&dir.path().join("none"), 10).unwrap(), 0);
    }

    #[test]
    fn scopes_order_this_directory_nearest() {
        let all = vec![e(1, "/a", "a1"), e(2, "/b", "b1"), e(3, "/a", "a2"), e(4, "/b", "b2")];
        let texts = |r: Vec<Recalled>| r.into_iter().map(|r| (r.entry.text, r.here)).collect::<Vec<_>>();
        assert_eq!(texts(scoped(all.clone(), Path::new("/a"), Scope::Project)), [("a1".into(), true), ("a2".into(), true)]);
        assert_eq!(
            texts(scoped(all.clone(), Path::new("/a"), Scope::Global)),
            [("a1".into(), true), ("b1".into(), false), ("a2".into(), true), ("b2".into(), false)]
        );
        assert_eq!(
            texts(scoped(all, Path::new("/a"), Scope::ProjectThenGlobal)),
            [("b1".into(), false), ("b2".into(), false), ("a1".into(), true), ("a2".into(), true)],
            "Up walks /a's prompts first, then the rest"
        );
    }

    #[test]
    fn scope_names_parse() {
        for n in Scope::NAMES {
            assert!(Scope::parse(n).is_some(), "{n}");
        }
        assert_eq!(Scope::parse("everywhere"), None);
    }
}
