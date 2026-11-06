//! `arnes-tools` — the built-in tools: Read, Write, Edit, Glob, Grep, Bash.
//!
//! Named and shaped exactly like Claude Code's (`file_path`, `old_string`,
//! `new_string`, `replace_all`, `pattern`, `path`, `glob`, `command`,
//! `timeout`) so models and permission rules written for one carry over.
//! No pleme-io primitive provided these (docs/integrations.md); they are
//! built here and lift to a shared crate when a second consumer appears.
//!
//! - Read / Glob / Grep are **read-only** — the permission gate lets them
//!   run unasked in default mode.
//! - Write / Edit / Bash may change the world — they are always asked.
//! - Edit and Write answer with a unified diff (egaku `chigai`), which the
//!   TUI renders coloured.
//! - Every output is capped (head + tail with a marker) so one large file
//!   or command cannot flood the context (docs/token-efficiency.md §4.4).

use std::path::{Path, PathBuf};
use std::time::Duration;

use arnes_core::{Tool, ToolInputSchema};
use serde_json::{Value, json};

/// Largest tool output kept inline (characters).
pub const OUTPUT_CAP: usize = 30_000;
/// Default / maximum Bash timeout.
pub const BASH_DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
pub const BASH_MAX_TIMEOUT: Duration = Duration::from_secs(600);
/// Read returns at most this many lines by default.
pub const READ_DEFAULT_LIMIT: usize = 2000;
/// Glob / Grep stop after this many results.
pub const MAX_RESULTS: usize = 250;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("{0}")]
    Invalid(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, ToolError>;

pub mod skills;

/// The built-in tool set, rooted at a working directory.
#[derive(Debug, Clone)]
pub struct Builtins {
    root: PathBuf,
    /// Built-ins configured off by name (`:modules-off ("Bash")`): not served,
    /// not sent to the model, and their names free for a blue tool.
    off: Vec<String>,
    /// Bash commands moved to the background, shared by every set the
    /// session builds so a reload keeps them.
    jobs: Jobs,
}

const NAMES: [&str; 8] = ["Bash", "BashOutput", "Edit", "Glob", "Grep", "KillShell", "Read", "Write"];

/// Background Bash jobs: started with `run_in_background`, or moved there by
/// the user (`Ctrl-B`) while running.
#[derive(Debug, Clone, Default)]
pub struct Jobs(std::sync::Arc<JobsInner>);

#[derive(Debug, Default)]
struct JobsInner {
    table: std::sync::Mutex<JobTable>,
    /// Wakes every Bash call in the foreground: move to the background.
    detach: tokio::sync::Notify,
    running: std::sync::atomic::AtomicUsize,
}

#[derive(Debug, Default)]
struct JobTable {
    next: u32,
    jobs: std::collections::BTreeMap<u32, Job>,
}

#[derive(Debug)]
struct Job {
    command: String,
    run: Run,
    read: usize,
}

/// One running command: its output so far, its exit once known, and the
/// way to stop it.
#[derive(Debug, Clone)]
struct Run {
    out: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    done: tokio::sync::watch::Receiver<Option<String>>,
    kill: std::sync::Arc<tokio::sync::Notify>,
}

impl Run {
    fn start(command: &str, root: &Path) -> Result<Self> {
        use tokio::io::AsyncReadExt;
        let mut child = tokio::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .current_dir(root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let out = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let pump = |mut r: Box<dyn tokio::io::AsyncRead + Unpin + Send>, out: std::sync::Arc<std::sync::Mutex<Vec<u8>>>| {
            tokio::spawn(async move {
                let mut buf = [0u8; 8192];
                while let Ok(n) = r.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut o) = out.lock() {
                        o.extend_from_slice(&buf[..n]);
                    }
                }
            })
        };
        let readers = [child.stdout.take().map(|s| pump(Box::new(s), out.clone())), child.stderr.take().map(|s| pump(Box::new(s), out.clone()))];
        let (tx, done) = tokio::sync::watch::channel(None);
        let kill = std::sync::Arc::new(tokio::sync::Notify::new());
        let stop = kill.clone();
        tokio::spawn(async move {
            let code = tokio::select! {
                status = child.wait() => status.ok().and_then(|s| s.code()).map_or_else(|| "killed by signal".to_owned(), |c| c.to_string()),
                () = stop.notified() => {
                    let _ = child.kill().await;
                    "killed".to_owned()
                }
            };
            for r in readers.into_iter().flatten() {
                let _ = r.await;
            }
            let _ = tx.send(Some(code));
        });
        Ok(Self { out, done, kill })
    }

    fn exit(&self) -> Option<String> {
        self.done.borrow().clone()
    }

    fn output_from(&self, from: usize) -> (String, usize) {
        let o = self.out.lock().map(|o| o.clone()).unwrap_or_default();
        (String::from_utf8_lossy(&o[from.min(o.len())..]).into_owned(), o.len())
    }
}

impl Jobs {
    /// Move every Bash command running in the foreground to the background.
    /// False when none is running.
    pub fn detach(&self) -> bool {
        let running = self.0.running.load(std::sync::atomic::Ordering::SeqCst) > 0;
        if running {
            self.0.detach.notify_waiters();
        }
        running
    }

    fn adopt(&self, command: &str, run: Run) -> u32 {
        let mut t = self.0.table.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        t.next += 1;
        let id = t.next;
        t.jobs.insert(id, Job { command: command.to_owned(), run, read: 0 });
        id
    }

    fn output(&self, id: u32) -> Result<String> {
        let mut t = self.0.table.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let ids: Vec<u32> = t.jobs.keys().copied().collect();
        let job = t.jobs.get_mut(&id).ok_or_else(|| ToolError::Invalid(format!("no background job {id}; jobs: {ids:?}")))?;
        let (new, end) = job.run.output_from(job.read);
        job.read = end;
        let state = job.run.exit().map_or_else(|| "[running]".to_owned(), |c| format!("[exit {c}]"));
        Ok(cap(&format!("{}{new}\n{state}", if new.is_empty() { "(no new output)\n" } else { "" })))
    }

    fn kill(&self, id: u32) -> Result<String> {
        let t = self.0.table.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let job = t.jobs.get(&id).ok_or_else(|| ToolError::Invalid(format!("no background job {id}")))?;
        if job.run.exit().is_some() {
            return Ok(format!("job {id} had already finished: {}", job.command));
        }
        job.run.kill.notify_one();
        Ok(format!("job {id} stopped: {}", job.command))
    }
}

impl Builtins {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), off: Vec::new(), jobs: Jobs::default() }
    }

    /// The same set sharing `jobs` (the session's), so a rebuilt set keeps
    /// the commands already in the background.
    #[must_use]
    pub fn with_jobs(mut self, jobs: Jobs) -> Self {
        self.jobs = jobs;
        self
    }

    /// The background jobs this set records into.
    #[must_use]
    pub fn jobs(&self) -> &Jobs {
        &self.jobs
    }

    /// The same set with the built-ins named in `off` turned off. A name that
    /// is not a built-in is ignored (it may name a command or a segment).
    #[must_use]
    pub fn with_off(mut self, off: &[String]) -> Self {
        self.off = off.iter().filter(|n| Self::owns(n)).cloned().collect();
        self
    }

    /// Does this set serve `name`: a built-in, and not turned off?
    #[must_use]
    pub fn serves(&self, name: &str) -> bool {
        Self::owns(name) && !self.off.iter().any(|o| o == name)
    }

    /// The definitions of the built-ins not in `off`, sorted by name.
    #[must_use]
    pub fn tools_except(off: &[String]) -> Vec<Tool> {
        Self::tools().into_iter().filter(|t| !off.contains(&t.name)).collect()
    }

    /// Does this set own `name`?
    #[must_use]
    pub fn owns(name: &str) -> bool {
        NAMES.contains(&name)
    }

    /// `Some(true)` for tools that cannot change anything.
    #[must_use]
    pub fn read_only(name: &str) -> Option<bool> {
        match name {
            "Read" | "Glob" | "Grep" => Some(true),
            "Write" | "Edit" | "Bash" | "KillShell" => Some(false),
            "BashOutput" => Some(true),
            _ => None,
        }
    }

    /// The tool definitions, sorted by name — a fixed, cache-stable block.
    #[must_use] #[rustfmt::skip]
    pub fn tools() -> Vec<Tool> {
        let t = |name: &str, description: &str, schema: Value| Tool {
            name: name.into(),
            description: description.into(),
            input_schema: ToolInputSchema(schema),
        };
        vec![
            t("Bash", "Run a shell command in the working directory. Output (stdout+stderr) is capped; `timeout` is in milliseconds (default 120000, max 600000). `run_in_background: true` starts it as a job and returns its id at once; the user may also move a running command to the background (Ctrl-B). Read a job with BashOutput, stop it with KillShell.", json!({
                "type": "object",
                "properties": {"command": {"type": "string"}, "timeout": {"type": "integer"}, "description": {"type": "string"}, "run_in_background": {"type": "boolean"}},
                "required": ["command"]
            })),
            t("BashOutput", "The output a background Bash job wrote since the last read, then [running] or [exit N].", json!({
                "type": "object",
                "properties": {"job": {"type": "integer"}},
                "required": ["job"]
            })),
            t("Edit", "Replace exact text in a file. `old_string` must match exactly once unless `replace_all` is true. Returns a unified diff.", json!({
                "type": "object",
                "properties": {"file_path": {"type": "string"}, "old_string": {"type": "string"}, "new_string": {"type": "string"}, "replace_all": {"type": "boolean"}},
                "required": ["file_path", "old_string", "new_string"]
            })),
            t("Glob", "Find files by glob pattern (e.g. \"src/**/*.rs\"), honouring .gitignore. Newest first.", json!({
                "type": "object",
                "properties": {"pattern": {"type": "string"}, "path": {"type": "string"}},
                "required": ["pattern"]
            })),
            t("Grep", "Search file contents with a regular expression, honouring .gitignore. Returns path:line:text.", json!({
                "type": "object",
                "properties": {"pattern": {"type": "string"}, "path": {"type": "string"}, "glob": {"type": "string"}, "-i": {"type": "boolean"}},
                "required": ["pattern"]
            })),
            t("KillShell", "Stop a background Bash job.", json!({
                "type": "object",
                "properties": {"job": {"type": "integer"}},
                "required": ["job"]
            })),
            t("Read", "Read a text file with line numbers. `offset` (1-based line) and `limit` select a range (default 2000 lines).", json!({
                "type": "object",
                "properties": {"file_path": {"type": "string"}, "offset": {"type": "integer"}, "limit": {"type": "integer"}},
                "required": ["file_path"]
            })),
            t("Write", "Create or overwrite a file with `content`. Returns a unified diff against the previous content.", json!({
                "type": "object",
                "properties": {"file_path": {"type": "string"}, "content": {"type": "string"}},
                "required": ["file_path", "content"]
            })),
        ]
    }

    /// Run one tool.
    ///
    /// # Errors
    /// Invalid input (named), or the underlying I/O error.
    pub async fn call(&self, name: &str, input: &Value) -> Result<String> {
        match name {
            "Read" => self.read(input),
            "Write" => self.write(input),
            "Edit" => self.edit(input),
            "Glob" => self.glob(input),
            "Grep" => self.grep(input),
            "Bash" => self.bash(input).await,
            "BashOutput" => self.jobs.output(job_field(input)?),
            "KillShell" => self.jobs.kill(job_field(input)?),
            other => Err(ToolError::Invalid(format!("no built-in tool {other:?}"))),
        }
    }

    fn resolve(&self, p: &str) -> PathBuf {
        let path = Path::new(p);
        if path.is_absolute() { path.to_owned() } else { self.root.join(path) }
    }

    fn read(&self, input: &Value) -> Result<String> {
        let path = self.resolve(str_field(input, "file_path")?);
        let text = std::fs::read_to_string(&path)?;
        let offset = usize_field(input, "offset").unwrap_or(1).max(1);
        let limit = usize_field(input, "limit").unwrap_or(READ_DEFAULT_LIMIT);
        let mut out = String::new();
        for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
            let line: String = line.chars().take(2000).collect();
            out.push_str(&format!("{:>6}\t{line}\n", i + 1));
        }
        if out.is_empty() {
            out = format!("(no lines at offset {offset}; the file has {} lines)", text.lines().count());
        }
        Ok(cap(&out))
    }

    fn write(&self, input: &Value) -> Result<String> {
        let path = self.resolve(str_field(input, "file_path")?);
        let content = str_field(input, "content")?;
        let before = std::fs::read_to_string(&path).unwrap_or_default();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, content)?;
        let diff = egaku::chigai::unified(&before, content, 3);
        Ok(cap(&format!("wrote {} ({} bytes)\n{diff}", path.display(), content.len())))
    }

    fn edit(&self, input: &Value) -> Result<String> {
        let path = self.resolve(str_field(input, "file_path")?);
        let old = str_field(input, "old_string")?;
        let new = str_field(input, "new_string")?;
        let all = input.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
        if old == new {
            return Err(ToolError::Invalid("old_string and new_string are identical".into()));
        }
        let before = std::fs::read_to_string(&path)?;
        let n = before.matches(old).count();
        match (n, all) {
            (0, _) => return Err(ToolError::Invalid(format!("old_string not found in {}", path.display()))),
            (n, false) if n > 1 => {
                return Err(ToolError::Invalid(format!("old_string appears {n} times in {}; add surrounding context to make it unique, or set replace_all", path.display())));
            }
            _ => {}
        }
        let after = if all { before.replace(old, new) } else { before.replacen(old, new, 1) };
        std::fs::write(&path, &after)?;
        let diff = egaku::chigai::unified(&before, &after, 3);
        Ok(cap(&format!("edited {} ({} replacement(s))\n{diff}", path.display(), if all { n } else { 1 })))
    }

    fn walk(&self, input: &Value) -> PathBuf {
        input.get("path").and_then(Value::as_str).map_or_else(|| self.root.clone(), |p| self.resolve(p))
    }

    fn glob(&self, input: &Value) -> Result<String> {
        let pattern = str_field(input, "pattern")?;
        let matcher = globset::Glob::new(pattern).map_err(|e| ToolError::Invalid(format!("bad glob {pattern:?}: {e}")))?.compile_matcher();
        let base = self.walk(input);
        let mut hits: Vec<(std::time::SystemTime, String)> = ignore::WalkBuilder::new(&base)
            .build()
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
            .filter_map(|e| {
                let rel = e.path().strip_prefix(&base).ok()?.to_owned();
                matcher.is_match(&rel).then(|| {
                    let mtime = e.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(std::time::UNIX_EPOCH);
                    (mtime, rel.to_string_lossy().into_owned())
                })
            })
            .collect();
        hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let total = hits.len();
        let mut out: Vec<String> = hits.into_iter().take(MAX_RESULTS).map(|(_, p)| p).collect();
        if total > MAX_RESULTS {
            out.push(format!("… {} more (narrow the pattern)", total - MAX_RESULTS));
        }
        if out.is_empty() {
            return Ok(format!("no files match {pattern:?}"));
        }
        Ok(cap(&out.join("\n")))
    }

    fn grep(&self, input: &Value) -> Result<String> {
        let pattern = str_field(input, "pattern")?;
        let insensitive = input.get("-i").and_then(Value::as_bool).unwrap_or(false);
        let re = regex::RegexBuilder::new(pattern).case_insensitive(insensitive).build().map_err(|e| ToolError::Invalid(format!("bad regex {pattern:?}: {e}")))?;
        let filter = input.get("glob").and_then(Value::as_str).map(|g| globset::Glob::new(g).map(|g| g.compile_matcher())).transpose().map_err(|e| ToolError::Invalid(format!("bad glob: {e}")))?;
        let base = self.walk(input);
        let mut out = Vec::new();
        let mut total = 0usize;
        for entry in ignore::WalkBuilder::new(&base).build().filter_map(std::result::Result::ok) {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let rel = entry.path().strip_prefix(&base).unwrap_or(entry.path()).to_owned();
            if filter.as_ref().is_some_and(|m| !m.is_match(&rel)) {
                continue;
            }
            // Binary or unreadable files are skipped, not errors.
            let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
            for (i, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    total += 1;
                    if out.len() < MAX_RESULTS {
                        let line: String = line.chars().take(500).collect();
                        out.push(format!("{}:{}:{line}", rel.display(), i + 1));
                    }
                }
            }
        }
        if total > out.len() {
            out.push(format!("… {} more matches (narrow the pattern or add glob)", total - out.len()));
        }
        if out.is_empty() {
            return Ok(format!("no matches for {pattern:?}"));
        }
        Ok(cap(&out.join("\n")))
    }

    async fn bash(&self, input: &Value) -> Result<String> {
        let command = str_field(input, "command")?;
        let timeout = usize_field(input, "timeout").map_or(BASH_DEFAULT_TIMEOUT, |ms| Duration::from_millis(ms as u64)).min(BASH_MAX_TIMEOUT);
        let run = Run::start(command, &self.root)?;
        let moved = |id: u32, why: &str| format!("{why}: job {id}. Read its output with BashOutput {{\"job\": {id}}}; stop it with KillShell {{\"job\": {id}}}.");
        if input.get("run_in_background").and_then(Value::as_bool) == Some(true) {
            return Ok(moved(self.jobs.adopt(command, run), "started in the background"));
        }
        let inner = &self.jobs.0;
        inner.running.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut done = run.done.clone();
        let outcome = tokio::select! {
            r = done.wait_for(Option::is_some) => Ok(r.map(|c| c.clone()).ok().flatten()),
            () = tokio::time::sleep(timeout) => Err(()),
            () = inner.detach.notified() => Ok(None),
        };
        inner.running.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        match outcome {
            Err(()) => {
                run.kill.notify_one();
                Err(ToolError::Invalid(format!("command timed out after {}s and was killed", timeout.as_secs())))
            }
            Ok(Some(code)) => Ok(cap(&format!("{}\n[exit {code}]", run.output_from(0).0.trim_end_matches('\n')))),
            Ok(None) => {
                let (so_far, _) = run.output_from(0);
                let id = self.jobs.adopt(command, run);
                Ok(cap(&format!("{}\n{so_far}", moved(id, "the user moved it to the background (Ctrl-B) while it ran"))))
            }
        }
    }
}

fn job_field(input: &Value) -> Result<u32> {
    input.get("job").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).ok_or_else(|| ToolError::Invalid("missing integer field \"job\"".into()))
}

fn str_field<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input.get(key).and_then(Value::as_str).ok_or_else(|| ToolError::Invalid(format!("missing string field {key:?}")))
}

fn usize_field(input: &Value, key: &str) -> Option<usize> {
    input.get(key).and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok())
}

/// Keep the head and tail of oversized output, with a marker saying how
/// much was dropped — never silently truncated.
#[must_use]
pub fn cap(text: &str) -> String {
    let n = text.chars().count();
    if n <= OUTPUT_CAP {
        return text.to_owned();
    }
    let head: String = text.chars().take(OUTPUT_CAP * 2 / 3).collect();
    let tail: String = text.chars().skip(n - OUTPUT_CAP / 4).collect();
    format!("{head}\n… [{} characters omitted] …\n{tail}", n - head.chars().count() - tail.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> (tempfile::TempDir, Builtins) {
        let d = tempfile::tempdir().unwrap();
        let b = Builtins::new(d.path());
        (d, b)
    }

    #[test]
    fn tool_set_is_sorted_and_classified() {
        let names: Vec<_> = Builtins::tools().into_iter().map(|t| t.name).collect();
        assert_eq!(names, NAMES);
        for n in NAMES {
            assert!(Builtins::read_only(n).is_some(), "{n} must be classified");
        }
        assert_eq!(Builtins::read_only("Read"), Some(true));
        assert_eq!(Builtins::read_only("Bash"), Some(false));
    }

    #[tokio::test]
    async fn read_numbers_lines_and_honours_offset_limit() {
        let (d, b) = tmp();
        std::fs::write(d.path().join("f.txt"), "a\nb\nc\nd\n").unwrap();
        let out = b.call("Read", &json!({"file_path": "f.txt", "offset": 2, "limit": 2})).await.unwrap();
        assert_eq!(out, "     2\tb\n     3\tc\n");
    }

    #[tokio::test]
    async fn edit_requires_a_unique_match_and_returns_a_diff() {
        let (d, b) = tmp();
        let f = d.path().join("m.rs");
        std::fs::write(&f, "fn a() {}\nfn a() {}\n").unwrap();
        let err = b.call("Edit", &json!({"file_path": "m.rs", "old_string": "fn a", "new_string": "fn b"})).await.unwrap_err();
        assert!(err.to_string().contains("appears 2 times"), "{err}");
        let out = b.call("Edit", &json!({"file_path": "m.rs", "old_string": "fn a", "new_string": "fn b", "replace_all": true})).await.unwrap();
        assert!(out.contains("-fn a() {}") && out.contains("+fn b() {}"), "{out}");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "fn b() {}\nfn b() {}\n");
        let missing = b.call("Edit", &json!({"file_path": "m.rs", "old_string": "nope", "new_string": "x"})).await.unwrap_err();
        assert!(missing.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn write_creates_dirs_and_diffs_against_the_old_content() {
        let (d, b) = tmp();
        let out = b.call("Write", &json!({"file_path": "a/b/new.txt", "content": "hello\n"})).await.unwrap();
        assert!(out.contains("+hello"), "{out}");
        assert_eq!(std::fs::read_to_string(d.path().join("a/b/new.txt")).unwrap(), "hello\n");
    }

    #[tokio::test]
    async fn glob_and_grep_honour_gitignore() {
        let (d, b) = tmp();
        std::process::Command::new("git").arg("init").arg("-q").current_dir(d.path()).status().unwrap();
        std::fs::write(d.path().join(".gitignore"), "secret.rs\n").unwrap();
        std::fs::create_dir(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/lib.rs"), "fn needle() {}\n").unwrap();
        std::fs::write(d.path().join("secret.rs"), "fn needle() {}\n").unwrap();
        let g = b.call("Glob", &json!({"pattern": "**/*.rs"})).await.unwrap();
        assert!(g.contains("src/lib.rs") && !g.contains("secret.rs"), "{g}");
        let r = b.call("Grep", &json!({"pattern": "needle"})).await.unwrap();
        assert_eq!(r, "src/lib.rs:1:fn needle() {}");
    }

    /// Every read of job `id` until it reports an exit, within 20 s.
    async fn until_done(b: &Builtins, id: u32) -> String {
        let mut all = String::new();
        for _ in 0..200 {
            let out = b.call("BashOutput", &json!({"job": id})).await.unwrap();
            all.push_str(&out);
            if out.contains("[exit") {
                return all;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("job {id} did not finish: {all}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_background_job_is_read_and_stopped_by_its_id() {
        let (_d, b) = tmp();
        let started = b.call("Bash", &json!({"command": "echo one; sleep 0.2; echo two", "run_in_background": true})).await.unwrap();
        assert!(started.contains("job 1"), "{started}");
        let out = until_done(&b, 1).await;
        assert!(out.contains("one") && out.contains("two") && out.ends_with("[exit 0]"), "{out}");
        assert!(b.call("BashOutput", &json!({"job": 1})).await.unwrap().starts_with("(no new output)"), "each read returns only what is new");
        b.call("Bash", &json!({"command": "sleep 30", "run_in_background": true})).await.unwrap();
        assert!(b.call("KillShell", &json!({"job": 2})).await.unwrap().contains("stopped"));
        assert!(until_done(&b, 2).await.ends_with("[exit killed]"));
        assert!(b.call("BashOutput", &json!({"job": 9})).await.is_err());
        assert_eq!(Builtins::read_only("BashOutput"), Some(true));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ctrl_b_moves_a_running_command_to_the_background() {
        let (_d, b) = tmp();
        assert!(!b.jobs().detach(), "nothing running, nothing moved");
        let jobs = b.jobs().clone();
        let running = tokio::spawn({
            let b = b.clone();
            async move { b.call("Bash", &json!({"command": "echo early; sleep 1; echo late"})).await }
        });
        let mut detached = false;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if jobs.detach() {
                detached = true;
                break;
            }
        }
        assert!(detached, "the call registered as running");
        let moved = running.await.unwrap().unwrap();
        assert!(moved.contains("background (Ctrl-B)") && moved.contains("job 1"), "{moved}");
        let rest = until_done(&b, 1).await;
        let all = format!("{moved}{rest}");
        assert!(all.contains("early") && all.contains("late") && rest.ends_with("[exit 0]"), "the job ran on: {all}");
    }

    #[tokio::test]
    async fn bash_captures_output_exit_code_and_times_out() {
        let (_d, b) = tmp();
        let out = b.call("Bash", &json!({"command": "echo hi; echo err >&2; exit 3"})).await.unwrap();
        assert!(out.contains("hi") && out.contains("err") && out.ends_with("[exit 3]"), "{out}");
        let slow = b.call("Bash", &json!({"command": "sleep 5", "timeout": 200})).await.unwrap_err();
        assert!(slow.to_string().contains("timed out"), "{slow}");
    }

    #[test]
    fn cap_keeps_head_and_tail_with_a_marker() {
        let big = "x".repeat(OUTPUT_CAP * 2);
        let c = cap(&big);
        assert!(c.chars().count() < OUTPUT_CAP + 100);
        assert!(c.contains("characters omitted"));
        assert_eq!(cap("small"), "small");
    }
}
