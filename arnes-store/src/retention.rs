//! Bounds on what accumulates: keep-newest/max-age pruning and log rotation.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// What a directory of records keeps. `None` means unbounded on that axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    pub keep_newest: Option<usize>,
    pub max_age: Option<Duration>,
}

/// Remove files in `dir` whose name ends with `suffix` beyond `keep` newest
/// (by modification time) or older than `max_age` at `now`. Subdirectories and
/// other files are never touched. A missing directory removes nothing.
///
/// # Errors
/// When `dir` exists but cannot be read. A single file that cannot be removed
/// is skipped (logged), so one stuck file never stops the sweep.
pub fn prune(dir: &Path, suffix: &str, keep: Retention, now: SystemTime) -> std::io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| e.file_name().to_string_lossy().ends_with(suffix))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    let mut removed = Vec::new();
    for (i, (mtime, path)) in files.into_iter().enumerate() {
        let too_many = keep.keep_newest.is_some_and(|n| i >= n);
        let too_old = keep.max_age.is_some_and(|age| now.duration_since(mtime).is_ok_and(|d| d > age));
        if too_many || too_old {
            match std::fs::remove_file(&path) {
                Ok(()) => removed.push(path),
                Err(e) => tracing::debug!(path = %path.display(), error = %e, "retention: could not remove"),
            }
        }
    }
    Ok(removed)
}

/// When `path` is larger than `max_bytes`, move it to `<path>.1` (replacing
/// any older `.1`) so the next writer starts a fresh file. Returns whether it
/// rotated. A missing file does nothing.
///
/// # Errors
/// When the file exists and cannot be inspected or renamed.
pub fn rotate(path: &Path, max_bytes: u64) -> std::io::Result<bool> {
    let len = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if len <= max_bytes {
        return Ok(false);
    }
    let mut old = path.as_os_str().to_owned();
    old.push(".1");
    std::fs::rename(path, PathBuf::from(old))?;
    Ok(true)
}

/// Rotate every `*.log` directly in `dir` (the MCP server logs).
///
/// # Errors
/// When `dir` exists but cannot be read.
pub fn rotate_logs_in(dir: &Path, max_bytes: u64) -> std::io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut rotated = Vec::new();
    for p in entries.filter_map(Result::ok).map(|e| e.path()) {
        if p.extension().is_some_and(|x| x == "log") && rotate(&p, max_bytes).unwrap_or(false) {
            rotated.push(p);
        }
    }
    Ok(rotated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, FileTimes};

    fn touch(dir: &Path, name: &str, age_secs: u64, now: SystemTime) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, name).unwrap();
        let t = now - Duration::from_secs(age_secs);
        File::options().write(true).open(&p).unwrap().set_times(FileTimes::new().set_modified(t)).unwrap();
        p
    }

    #[test]
    fn missing_dir_is_empty_not_an_error() {
        let d = tempfile::tempdir().unwrap();
        let r = prune(&d.path().join("nope"), ".json", Retention { keep_newest: Some(1), max_age: None }, SystemTime::now());
        assert!(r.unwrap().is_empty());
    }

    #[test]
    fn unbounded_keeps_everything() {
        let d = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        for i in 0..5 {
            touch(d.path(), &format!("{i}.json"), i * 1000, now);
        }
        let r = prune(d.path(), ".json", Retention { keep_newest: None, max_age: None }, now).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn keeps_newest_and_drops_old_only_matching_suffix() {
        let d = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        touch(d.path(), "a.json", 10, now);
        touch(d.path(), "b.json", 20, now);
        let c = touch(d.path(), "c.json", 30, now);
        let old = touch(d.path(), "d.json", 10_000, now);
        touch(d.path(), "keep.txt", 99_999, now);
        let keep = Retention { keep_newest: Some(3), max_age: Some(Duration::from_secs(25)) };
        let mut removed = prune(d.path(), ".json", keep, now).unwrap();
        removed.sort();
        assert_eq!(removed, vec![c, old]);
        assert!(d.path().join("keep.txt").exists());
        assert!(d.path().join("a.json").exists() && d.path().join("b.json").exists());
    }

    #[test]
    fn rotates_only_past_the_cap() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.log");
        assert!(!rotate(&p, 4).unwrap(), "missing file");
        std::fs::write(&p, "1234").unwrap();
        assert!(!rotate(&p, 4).unwrap(), "at the cap");
        std::fs::write(&p, "12345").unwrap();
        assert!(rotate(&p, 4).unwrap());
        assert!(!p.exists());
        assert_eq!(std::fs::read_to_string(d.path().join("x.log.1")).unwrap(), "12345");
        assert_eq!(rotate_logs_in(d.path(), 1).unwrap().len(), 0, ".1 files are not logs");
    }
}
