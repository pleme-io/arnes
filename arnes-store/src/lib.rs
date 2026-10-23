//! What arnes keeps on disk, and where.
//!
//! One [`Layout`] names the four XDG roots once; every file arnes writes is a
//! typed method on it, so no other crate joins a path under those roots. The
//! plan, bounds and rungs are in `docs/persistence.md`.

pub mod cache;
pub mod ext;
pub mod history;
pub mod layout;
pub mod profiles;
pub mod retention;
pub mod session;

pub use layout::Layout;
pub use retention::{Retention, prune, rotate};
pub use session::{Session, SessionError, SessionMeta, Sessions};

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How much arnes keeps. Every field has a default; the config overrides
/// each one independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Record sessions at all.
    pub sessions: bool,
    pub session_retention: Retention,
    /// A log past this many bytes is rotated to `.1` at startup.
    pub log_max_bytes: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            sessions: true,
            session_retention: Retention {
                keep_newest: Some(200),
                max_age: Some(Duration::from_secs(90 * 24 * 60 * 60)),
            },
            log_max_bytes: 8 * 1024 * 1024,
        }
    }
}

/// What one [`sweep`] removed or rotated.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Swept {
    pub sessions_removed: Vec<PathBuf>,
    pub logs_rotated: Vec<PathBuf>,
}

/// Apply `policy` to what is on disk: prune sessions, rotate arnes's log and
/// every MCP server log. Each part runs even when another fails; failures
/// are logged, never raised, because a sweep that cannot run leaves
/// everything as it was.
#[must_use]
pub fn sweep(layout: &Layout, policy: &Policy, now: SystemTime) -> Swept {
    let mut out = Swept::default();
    match prune(&layout.sessions_dir(), ".json", policy.session_retention, now) {
        Ok(r) => out.sessions_removed = r,
        Err(e) => tracing::debug!(error = %e, "sweep: sessions"),
    }
    match rotate(&layout.log(), policy.log_max_bytes) {
        Ok(true) => out.logs_rotated.push(layout.log()),
        Ok(false) => {}
        Err(e) => tracing::debug!(error = %e, "sweep: arnes.log"),
    }
    match retention::rotate_logs_in(&layout.mcp_log_dir(), policy.log_max_bytes) {
        Ok(r) => out.logs_rotated.extend(r),
        Err(e) => tracing::debug!(error = %e, "sweep: mcp logs"),
    }
    out
}

/// Write `bytes` to `path` so a reader sees the old file or the new one,
/// never half of either: a sibling temp file, flushed, then renamed over.
/// The parent directory is created when missing.
///
/// # Errors
/// Any I/O failure, with the temp file removed.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().map_or_else(|| "tmp".into(), |n| n.to_string_lossy().into_owned());
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_of_an_empty_layout_touches_nothing() {
        let d = tempfile::tempdir().unwrap();
        let l = Layout::under(d.path());
        assert_eq!(sweep(&l, &Policy::default(), SystemTime::now()), Swept::default());
        assert!(!l.state.exists(), "a sweep creates no directories");
    }

    #[test]
    fn sweep_rotates_big_logs_and_prunes_sessions() {
        let d = tempfile::tempdir().unwrap();
        let l = Layout::under(d.path());
        write_atomic(&l.log(), b"0123456789").unwrap();
        write_atomic(&l.mcp_log_dir().join("gh.log"), b"0123456789").unwrap();
        write_atomic(&l.mcp_log_dir().join("small.log"), b"01").unwrap();
        for i in 0..3 {
            write_atomic(&l.sessions_dir().join(format!("{i}.json")), b"{}").unwrap();
        }
        let policy = Policy {
            sessions: true,
            session_retention: Retention { keep_newest: Some(1), max_age: None },
            log_max_bytes: 5,
        };
        let s = sweep(&l, &policy, SystemTime::now());
        assert_eq!(s.sessions_removed.len(), 2);
        assert_eq!(s.logs_rotated.len(), 2);
        assert!(l.mcp_log_dir().join("small.log").exists());
    }

    #[test]
    fn atomic_write_creates_parents_and_replaces() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a/b/c.json");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        let leftovers: Vec<_> = std::fs::read_dir(p.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "no temp file left behind");
    }
}
