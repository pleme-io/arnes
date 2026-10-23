//! The prompt history's disk side (`history.*`, docs/persistence.md): load it
//! scoped to this directory at startup, append each prompt the reducer
//! records, and follow a reload.

use std::path::PathBuf;

use arnes_config::{ArnesConfig, HistoryKnobs};
use arnes_store::history::{self, Entry, Scope};
use arnes_tui::InputState;

pub struct PromptHistory {
    /// `$XDG_STATE_HOME/arnes/history.jsonl`; `None` with no state directory.
    path: Option<PathBuf>,
    cwd: PathBuf,
    knobs: HistoryKnobs,
}

impl PromptHistory {
    /// Read the file into `input` (when `history.record` is on) and bound it.
    pub fn open(cfg: &ArnesConfig, input: &mut InputState) -> Self {
        let path = arnes_store::Layout::from_env().map(|l| l.prompt_history());
        let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
        Self::at(path, cwd, cfg.history_knobs().0, input)
    }

    fn at(path: Option<PathBuf>, cwd: PathBuf, knobs: HistoryKnobs, input: &mut InputState) -> Self {
        let this = Self { path, cwd, knobs };
        this.load(input);
        this
    }

    fn load(&self, input: &mut InputState) {
        let Some(path) = self.path.as_deref().filter(|_| self.knobs.record) else { return };
        match history::compact(path, self.knobs.keep) {
            Ok(0) => {}
            Ok(n) => tracing::debug!(dropped = n, "prompt history compacted"),
            Err(e) => tracing::warn!("prompt history {}: {e}", path.display()),
        }
        let recalled = history::scoped(history::load(path, self.knobs.keep), &self.cwd, self.knobs.scope);
        input.load_history(recalled, self.knobs.scope == Scope::ProjectThenGlobal);
    }

    /// Append what `input` recorded since the last call. With `record` off
    /// the prompts are taken and dropped: they live in this session only.
    pub fn record(&self, input: &mut InputState) {
        let texts = input.take_recorded();
        let Some(path) = self.path.as_deref().filter(|_| self.knobs.record) else { return };
        let at = chrono::Utc::now().timestamp();
        for text in texts {
            if let Err(e) = history::append(path, &Entry { at, cwd: self.cwd.clone(), text }) {
                tracing::warn!("prompt history {}: {e}", path.display());
            }
        }
    }

    /// A reload: with `record` still on, a new scope or bound re-reads the
    /// file (everything recorded is on disk, so nothing is lost); turning
    /// `record` on or off keeps what this session already walks.
    pub fn reconfigure(&mut self, cfg: &ArnesConfig, input: &mut InputState) {
        let next = cfg.history_knobs().0;
        let reread = self.knobs.record && next.record && (next.scope != self.knobs.scope || next.keep != self.knobs.keep);
        self.knobs = next;
        if reread {
            self.load(input);
        }
    }

    #[cfg(test)]
    fn path(&self) -> &std::path::Path {
        self.path.as_deref().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arnes_tui::Action;
    use std::time::Instant;

    fn submit(input: &mut InputState, text: &str) {
        input.set_text(text);
        input.apply(Action::Submit, false, Instant::now());
    }

    fn up_all(input: &mut InputState) -> Vec<String> {
        let mut seen = Vec::new();
        for _ in 0..10 {
            input.apply(Action::HistoryPrevious, false, Instant::now());
            let t = input.editor.text();
            if seen.last() != Some(&t) {
                seen.push(t);
            }
        }
        seen
    }

    #[test]
    fn prompts_persist_across_sessions_this_directory_nearest() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state/history.jsonl");
        let knobs = HistoryKnobs::default();

        let mut a = InputState::new();
        let h = PromptHistory::at(Some(file.clone()), "/repo-a".into(), knobs, &mut a);
        submit(&mut a, "a one");
        submit(&mut a, "a two");
        h.record(&mut a);
        let mut b = InputState::new();
        let hb = PromptHistory::at(Some(file.clone()), "/repo-b".into(), knobs, &mut b);
        submit(&mut b, "b one");
        hb.record(&mut b);
        assert_eq!(history::load(h.path(), 0).len(), 3);

        let mut next = InputState::new();
        PromptHistory::at(Some(file.clone()), "/repo-a".into(), knobs, &mut next);
        assert_eq!(up_all(&mut next), ["a two", "a one", "b one"], "project first, then the rest");

        let project = HistoryKnobs { scope: Scope::Project, ..knobs };
        let mut only = InputState::new();
        PromptHistory::at(Some(file.clone()), "/repo-a".into(), project, &mut only);
        assert_eq!(up_all(&mut only), ["a two", "a one"]);
    }

    #[test]
    fn record_off_reads_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("history.jsonl");
        history::append(&file, &Entry { at: 1, cwd: "/r".into(), text: "old".into() }).unwrap();
        let off = HistoryKnobs { record: false, ..HistoryKnobs::default() };
        let mut i = InputState::new();
        let h = PromptHistory::at(Some(file.clone()), "/r".into(), off, &mut i);
        assert!(i.history().is_empty());
        submit(&mut i, "new");
        h.record(&mut i);
        assert_eq!(history::load(&file, 0).len(), 1);
        assert_eq!(i.history(), ["new"], "the session still walks its own prompts");
        assert!(i.take_recorded().is_empty(), "and nothing piles up waiting");
    }

    #[test]
    fn a_reload_to_a_new_scope_rereads_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("history.jsonl");
        history::append(&file, &Entry { at: 1, cwd: "/other".into(), text: "elsewhere".into() }).unwrap();
        let mut i = InputState::new();
        let mut h = PromptHistory::at(Some(file), "/r".into(), HistoryKnobs { scope: Scope::Project, ..HistoryKnobs::default() }, &mut i);
        assert!(i.history().is_empty());
        let cfg = ArnesConfig { history: arnes_config::HistorySection { scope: Some("global".into()), ..Default::default() }, ..ArnesConfig::default() };
        h.reconfigure(&cfg, &mut i);
        assert_eq!(i.history(), ["elsewhere"]);
    }
}
