//! The last profile used in each working directory (docs/profiles.md §2):
//! `state/profiles.json`, `{"<dir>": "<profile>"}`, rewritten atomically. An
//! unreadable file is an empty memory, never an error: remembering is a
//! convenience, and the next switch rewrites it.

use std::collections::BTreeMap;
use std::path::Path;

use crate::layout::Layout;

fn read(layout: &Layout) -> BTreeMap<String, String> {
    std::fs::read(layout.profiles()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// The profile last used in `dir`, if any.
#[must_use]
pub fn last(layout: &Layout, dir: &Path) -> Option<String> {
    read(layout).remove(&dir.display().to_string())
}

/// Remember `profile` for `dir`; `None` forgets it.
///
/// # Errors
/// The file cannot be written.
pub fn remember(layout: &Layout, dir: &Path, profile: Option<&str>) -> std::io::Result<()> {
    let mut all = read(layout);
    let key = dir.display().to_string();
    match profile {
        Some(p) => all.insert(key, p.to_owned()),
        None => all.remove(&key),
    };
    let bytes = serde_json::to_vec_pretty(&all).map_err(std::io::Error::other)?;
    crate::write_atomic(&layout.profiles(), &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_remembers_its_own_profile_and_forgets_on_none() {
        let root = tempfile::tempdir().unwrap();
        let l = Layout::under(root.path());
        let (a, b) = (Path::new("/code/a"), Path::new("/code/b"));
        assert_eq!(last(&l, a), None);
        remember(&l, a, Some("ops")).unwrap();
        remember(&l, b, Some("base")).unwrap();
        assert_eq!(last(&l, a).as_deref(), Some("ops"));
        assert_eq!(last(&l, b).as_deref(), Some("base"));
        remember(&l, a, None).unwrap();
        assert_eq!(last(&l, a), None);
        std::fs::write(l.profiles(), b"not json").unwrap();
        assert_eq!(last(&l, b), None, "a broken file is an empty memory");
        remember(&l, b, Some("x")).unwrap();
        assert_eq!(last(&l, b).as_deref(), Some("x"));
    }
}
