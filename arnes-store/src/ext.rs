//! Persistent state for blue extensions: string keys to string values, one
//! bounded JSON file per program.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// At most this many keys per extension.
pub const MAX_KEYS: usize = 256;
/// At most this many bytes of keys and values together per extension.
pub const MAX_BYTES: usize = 64 * 1024;

/// A pending change: `Some` sets, `None` deletes.
pub type Write = (String, Option<String>);

/// Why a commit was refused. Nothing is written when it is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtStoreError {
    #[error("{keys} keys exceeds the limit of {MAX_KEYS}")]
    TooManyKeys { keys: usize },
    #[error("{bytes} bytes exceeds the limit of {MAX_BYTES}")]
    TooLarge { bytes: usize },
    #[error("{path}: {message}")]
    Io { path: PathBuf, message: String },
}

/// One extension's state as loaded from disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtStore {
    path: Option<PathBuf>,
    map: BTreeMap<String, String>,
}

impl ExtStore {
    /// Load `path`; a missing or unreadable file is empty state. `None`
    /// (no data root) is state that lives only for this call.
    #[must_use]
    pub fn load(path: Option<PathBuf>) -> Self {
        let map = path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { path, map }
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Apply `writes` in order, check the bounds, and write the file
    /// atomically. An over-limit result is refused whole.
    ///
    /// # Errors
    /// [`ExtStoreError`]; the file and `self` are unchanged.
    pub fn commit(&mut self, writes: &[Write]) -> Result<(), ExtStoreError> {
        if writes.is_empty() {
            return Ok(());
        }
        let mut next = self.map.clone();
        for (k, v) in writes {
            match v {
                Some(v) => next.insert(k.clone(), v.clone()),
                None => next.remove(k),
            };
        }
        if next.len() > MAX_KEYS {
            return Err(ExtStoreError::TooManyKeys { keys: next.len() });
        }
        let bytes: usize = next.iter().map(|(k, v)| k.len() + v.len()).sum();
        if bytes > MAX_BYTES {
            return Err(ExtStoreError::TooLarge { bytes });
        }
        if let Some(p) = &self.path {
            let json = serde_json::to_vec_pretty(&next).map_err(|e| ExtStoreError::Io { path: p.clone(), message: e.to_string() })?;
            crate::write_atomic(p, &json).map_err(|e| ExtStoreError::Io { path: p.clone(), message: e.to_string() })?;
        }
        self.map = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(k: &str, v: &str) -> Write {
        (k.into(), Some(v.into()))
    }

    #[test]
    fn missing_file_is_empty_and_empty_commit_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.json");
        let mut s = ExtStore::load(Some(p.clone()));
        assert_eq!(s.get("a"), None);
        s.commit(&[]).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn commit_then_reload_is_identity_and_deletes_apply_in_order() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.json");
        let mut s = ExtStore::load(Some(p.clone()));
        s.commit(&[set("a", "1"), set("b", "2"), ("a".into(), None), set("c", "3")]).unwrap();
        let back = ExtStore::load(Some(p));
        assert_eq!(back, s);
        assert_eq!((back.get("a"), back.get("b"), back.get("c")), (None, Some("2"), Some("3")));
    }

    #[test]
    fn over_the_bounds_is_refused_whole() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.json");
        let mut s = ExtStore::load(Some(p.clone()));
        s.commit(&[set("keep", "v")]).unwrap();
        let many: Vec<Write> = (0..=MAX_KEYS).map(|i| set(&format!("k{i}"), "v")).collect();
        assert!(matches!(s.commit(&many), Err(ExtStoreError::TooManyKeys { .. })));
        let big = "x".repeat(MAX_BYTES);
        assert!(matches!(s.commit(&[set("big", &big)]), Err(ExtStoreError::TooLarge { .. })));
        assert_eq!(ExtStore::load(Some(p)).get("keep"), Some("v"));
        assert_eq!(s.get("big"), None);
    }
}
