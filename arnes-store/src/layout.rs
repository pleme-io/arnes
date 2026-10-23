//! The four roots and every path under them.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

const APP: &str = "arnes";

/// Where arnes's files live: `$XDG_{CONFIG,STATE,CACHE,DATA}_HOME/arnes`, each
/// falling back to its `$HOME` default. See `docs/persistence.md` for what
/// lives in each root and how it is bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub config: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
    pub data: PathBuf,
}

impl Layout {
    /// From the process environment. `None` only when neither an XDG variable
    /// nor `$HOME` names a root, in which case nothing is written.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        Self::from_vars(|k| std::env::var_os(k))
    }

    /// From any variable lookup (tests pass a map).
    #[must_use]
    pub fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> Option<Self> {
        let home = var("HOME").map(PathBuf::from);
        let root = |xdg: &str, fallback: &str| -> Option<PathBuf> {
            var(xdg)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .or_else(|| home.as_ref().map(|h| h.join(fallback)))
                .map(|p| p.join(APP))
        };
        Some(Self {
            config: root("XDG_CONFIG_HOME", ".config")?,
            state: root("XDG_STATE_HOME", ".local/state")?,
            cache: root("XDG_CACHE_HOME", ".cache")?,
            data: root("XDG_DATA_HOME", ".local/share")?,
        })
    }

    /// All four roots under one directory (tests, and `--state-dir`-style overrides).
    #[must_use]
    pub fn under(dir: &Path) -> Self {
        Self {
            config: dir.join("config"),
            state: dir.join("state"),
            cache: dir.join("cache"),
            data: dir.join("data"),
        }
    }

    // ---- state ----

    /// The usage ledger, one JSON line per model call.
    #[must_use]
    pub fn usage_ledger(&self) -> PathBuf {
        self.state.join("usage.jsonl")
    }

    /// arnes's own log.
    #[must_use]
    pub fn log(&self) -> PathBuf {
        self.state.join("arnes.log")
    }

    /// The last profile used in each working directory (docs/profiles.md §2).
    #[must_use]
    pub fn profiles(&self) -> PathBuf {
        self.state.join("profiles.json")
    }

    /// Each MCP server's stderr goes to `<dir>/<server>.log`.
    #[must_use]
    pub fn mcp_log_dir(&self) -> PathBuf {
        self.state.join("mcp")
    }

    /// Every submitted prompt, one JSON line each (`history`).
    #[must_use]
    pub fn prompt_history(&self) -> PathBuf {
        self.state.join("history.jsonl")
    }

    /// Saved sessions, one `<id>.json` each.
    #[must_use]
    pub fn sessions_dir(&self) -> PathBuf {
        self.state.join("sessions")
    }

    // ---- cache ----

    /// A provider's model catalogue.
    #[must_use]
    pub fn models_cache(&self, provider: &str) -> PathBuf {
        self.cache.join(format!("models-{}.json", file_safe(provider)))
    }

    // ---- data ----

    /// Blue extensions' persistent state, one `<extension>.json` each.
    #[must_use]
    pub fn ext_dir(&self) -> PathBuf {
        self.data.join("ext")
    }

    /// One extension's state file.
    #[must_use]
    pub fn ext(&self, extension: &str) -> PathBuf {
        self.ext_dir().join(format!("{}.json", file_safe(extension)))
    }
}

/// A name made safe as one path component: anything outside `[A-Za-z0-9._-]`
/// becomes `_`, and a leading dot is escaped so no name is hidden or `..`.
#[must_use]
pub fn file_safe(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' })
        .collect();
    if s.is_empty() || s.starts_with('.') {
        s.insert(0, '_');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let m: HashMap<String, OsString> = pairs.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn nothing_to_root_on_is_none() {
        assert_eq!(Layout::from_vars(vars(&[])), None);
    }

    #[test]
    fn home_fallbacks() {
        let l = Layout::from_vars(vars(&[("HOME", "/h")])).unwrap();
        assert_eq!(l.state, PathBuf::from("/h/.local/state/arnes"));
        assert_eq!(l.cache, PathBuf::from("/h/.cache/arnes"));
        assert_eq!(l.data, PathBuf::from("/h/.local/share/arnes"));
        assert_eq!(l.config, PathBuf::from("/h/.config/arnes"));
        assert_eq!(l.usage_ledger(), PathBuf::from("/h/.local/state/arnes/usage.jsonl"));
    }

    #[test]
    fn xdg_wins_and_relative_xdg_is_ignored() {
        let l = Layout::from_vars(vars(&[("HOME", "/h"), ("XDG_STATE_HOME", "/s"), ("XDG_CACHE_HOME", "rel")])).unwrap();
        assert_eq!(l.log(), PathBuf::from("/s/arnes/arnes.log"));
        assert_eq!(l.cache, PathBuf::from("/h/.cache/arnes"), "the XDG spec says relative values are ignored");
    }

    #[test]
    fn names_cannot_escape_their_directory() {
        let l = Layout::under(Path::new("/r"));
        assert_eq!(l.ext("../../etc/passwd"), PathBuf::from("/r/data/ext/_.._.._etc_passwd.json"));
        assert_eq!(l.ext("/wc"), PathBuf::from("/r/data/ext/_wc.json"));
        assert_eq!(file_safe(""), "_");
        assert_eq!(l.models_cache("deepseek"), PathBuf::from("/r/cache/models-deepseek.json"));
    }
}
