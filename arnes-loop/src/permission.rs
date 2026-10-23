//! The permission gate — nothing with side effects runs unasked.
//!
//! Modelled on Claude Code's permission modes, over arnes's tool surface:
//!
//! | mode      | read-only tools | everything else          |
//! |-----------|-----------------|--------------------------|
//! | `default` | allowed         | asked                    |
//! | `plan`    | allowed         | refused (plan = look)    |
//! | `bypass`  | allowed         | allowed                  |
//!
//! "Read-only" is the server's own MCP `readOnlyHint`; a tool whose server
//! says nothing is treated as able to write — silence is never read as
//! safety. Deny rules win over everything; allow rules and "always" answers
//! skip the question. A refusal becomes a tool **error** carrying the
//! reason, so the model sees why and can adapt instead of silently looping.
//!
//! The decision is a pure function ([`Policy::decide`]); asking a human is
//! the [`Approver`] seam, so the TUI and headless mode differ only there.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use arnes_mcp_client::{CALL_TOOL, McpPool, SEARCH_TOOL};
use async_trait::async_trait;

use crate::{LoopError, Result, ToolDispatcher};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PermissionMode {
    #[default]
    Default,
    Plan,
    Bypass,
}

impl PermissionMode {
    pub const ALL: [Self; 3] = [Self::Default, Self::Plan, Self::Bypass];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Plan => "plan",
            Self::Bypass => "bypass",
        }
    }

    /// `Shift-Tab` order, as in Claude Code.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Default => Self::Plan,
            Self::Plan => Self::Bypass,
            Self::Bypass => Self::Default,
        }
    }

    /// # Errors
    /// Names the accepted modes.
    pub fn parse(s: &str) -> std::result::Result<Self, String> {
        arnes_core::parse_named(s, &Self::ALL, Self::name, "permission-mode")
    }
}

/// What the policy says before anyone is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(String),
    Ask,
}

/// A human's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Allow this tool for the rest of the session.
    AllowAlways,
    Deny(String),
}

/// One question for a human.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalRequest {
    pub tool: String,
    pub input: serde_json::Value,
    pub read_only: Option<bool>,
    /// A guardrail warning the operator must see before answering.
    pub warning: Option<String>,
}

#[async_trait]
pub trait Approver: Send + Sync {
    async fn ask(&self, req: ApprovalRequest) -> Verdict;
}

/// Headless: there is no one to ask, so the answer is no — with the way
/// to make it yes.
pub struct NoHuman;

#[async_trait]
impl Approver for NoHuman {
    async fn ask(&self, req: ApprovalRequest) -> Verdict {
        Verdict::Deny(format!(
            "{} needs approval and no one is at the terminal; add an allow rule or use \
             permission-mode bypass",
            req.tool
        ))
    }
}

/// guardrail for the built-in tools.
pub enum Guard {
    Off,
    /// The engine could not be built: fail toward asking — every Bash
    /// command is asked, in every mode.
    Unavailable(String),
    On(Box<guardrail::RegexEngine>),
}

/// What guardrail says about one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Clear,
    Block(String),
    Warn(String),
}

impl Guard {
    /// Build the production engine (defaults + rules.d + guardrail.yaml,
    /// cached) — exactly what the Claude Code hook enforces.
    #[must_use]
    pub fn production() -> Self {
        match guardrail::production::production_engine() {
            Ok(e) => Self::On(Box::new(e)),
            Err(e) => Self::Unavailable(e.to_string()),
        }
    }

    /// Bash: the command. Write / Edit: the text they would put in a file
    /// (content lines only warn, as in guardrail's own hook).
    #[must_use]
    pub fn screen(&self, tool: &str, args: &serde_json::Value) -> Screen {
        use guardrail::{Decision as G, RuleEngine as _};
        let field = |k: &str| args.get(k).and_then(serde_json::Value::as_str).unwrap_or_default();
        match (self, tool) {
            (Self::Off, _) => Screen::Clear,
            (Self::Unavailable(why), "Bash") => Screen::Warn(format!("guardrail unavailable ({why}); review this command")),
            (Self::Unavailable(_), _) => Screen::Clear,
            (Self::On(engine), "Bash") => match engine.check(field("command")) {
                G::Allow => Screen::Clear,
                G::Block { rule, message } => Screen::Block(format!("guardrail blocked [{rule}]: {message}")),
                G::Warn { rule, message } => Screen::Warn(format!("guardrail [{rule}]: {message}")),
                // guardrail's Decision is non_exhaustive: a kind this build
                // does not know is shown to the human, never silently allowed.
                _ => Screen::Warn("guardrail returned an unrecognised decision; review this command".into()),
            },
            (Self::On(engine), "Write" | "Edit") => {
                let text = if tool == "Write" { field("content") } else { field("new_string") };
                text.lines()
                    .find_map(|l| match engine.check(l) {
                        G::Allow => None,
                        G::Block { rule, message } | G::Warn { rule, message } => {
                            Some(Screen::Warn(format!("guardrail [{rule}] in the new content: {message}")))
                        }
                        _ => Some(Screen::Warn("guardrail returned an unrecognised decision on the new content".into())),
                    })
                    .unwrap_or(Screen::Clear)
            }
            _ => Screen::Clear,
        }
    }
}

/// `*` matches any run of characters; everything else is literal.
fn glob(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || !name.ends_with(last) || name.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

/// Mode + rules — the pure part of the gate.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    pub mode: PermissionMode,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

impl Policy {
    #[must_use]
    pub fn decide(&self, tool: &str, read_only: Option<bool>, always: &HashSet<String>) -> Decision {
        if let Some(rule) = self.deny.iter().find(|p| glob(p, tool)) {
            return Decision::Deny(format!("{tool} is denied by rule {rule:?}"));
        }
        if always.contains(tool) || self.allow.iter().any(|p| glob(p, tool)) {
            return Decision::Allow;
        }
        match (self.mode, read_only == Some(true)) {
            (_, true) | (PermissionMode::Bypass, _) => Decision::Allow,
            (PermissionMode::Plan, false) => Decision::Deny(format!(
                "plan mode allows only read-only tools; {tool} may write (Shift-Tab changes mode)"
            )),
            (PermissionMode::Default, false) => Decision::Ask,
        }
    }
}

/// A [`ToolDispatcher`] that consults the policy, and a human when the
/// policy says ask, before letting a call reach the MCP pool.
pub struct GatedDispatcher {
    pool: Arc<McpPool>,
    /// The built-in tools (Read/Write/Edit/Glob/Grep/Bash), when enabled.
    builtins: Option<arnes_tools::Builtins>,
    /// Tools implemented in blue, declared in the config.
    blue: arnes_blue::Tools,
    /// The skills the `Skill` tool loads (docs/profiles.md §3); read-only.
    skills: Option<Arc<arnes_tools::skills::Skills>>,
    /// blue hooks; only `before-tool` fires here.
    hooks: arnes_blue::Hooks,
    /// guardrail's production engine — the rule set the Claude Code hook
    /// enforces — screening Bash commands and Write/Edit content.
    guard: Guard,
    approver: Arc<dyn Approver>,
    policy: Mutex<Policy>,
    always: Mutex<HashSet<String>>,
}

impl GatedDispatcher {
    #[must_use]
    pub fn new(pool: Arc<McpPool>, approver: Arc<dyn Approver>, policy: Policy) -> Self {
        Self {
            pool,
            builtins: None,
            blue: arnes_blue::Tools::default(),
            skills: None,
            hooks: arnes_blue::Hooks::default(),
            guard: Guard::Off,
            approver,
            policy: Mutex::new(policy),
            always: Mutex::new(HashSet::new()),
        }
    }

    /// Screen side-effecting built-ins with guardrail.
    #[must_use]
    pub fn with_guard(mut self, guard: Guard) -> Self {
        self.guard = guard;
        self
    }

    /// Serve the built-in tools too, gated exactly like MCP tools.
    #[must_use]
    pub fn with_builtins(mut self, builtins: arnes_tools::Builtins) -> Self {
        self.builtins = Some(builtins);
        self
    }

    /// Serve tools implemented in blue, gated exactly like the others.
    #[must_use]
    pub fn with_blue_tools(mut self, tools: arnes_blue::Tools) -> Self {
        self.blue = tools;
        self
    }

    /// Serve the `Skill` tool over `skills` (none: not served).
    #[must_use]
    pub fn with_skills(mut self, skills: Arc<arnes_tools::skills::Skills>) -> Self {
        self.skills = (!skills.is_empty()).then_some(skills);
        self
    }

    fn is_skill(&self, tool: &str) -> bool {
        self.skills.is_some() && tool == arnes_tools::skills::TOOL
    }

    /// Run blue `before-tool` hooks on every call that the rules do not
    /// already deny.
    #[must_use]
    pub fn with_hooks(mut self, hooks: arnes_blue::Hooks) -> Self {
        self.hooks = hooks;
        self
    }

    fn is_builtin(&self, tool: &str) -> bool {
        self.builtins.as_ref().is_some_and(|b| b.serves(tool))
    }

    /// Replace mode and rules (config hot reload). Session "always"
    /// answers survive — they were the operator's, not the file's.
    pub fn set_policy(&self, policy: Policy) {
        if let Ok(mut p) = self.policy.lock() {
            *p = policy;
        }
    }

    pub fn set_mode(&self, mode: PermissionMode) {
        if let Ok(mut p) = self.policy.lock() {
            p.mode = mode;
        }
    }

    /// The mode and rules in force now.
    #[must_use]
    pub fn policy(&self) -> Policy {
        self.policy.lock().map(|p| p.clone()).unwrap_or_default()
    }

    #[must_use]
    pub fn mode(&self) -> PermissionMode {
        self.policy.lock().map_or(PermissionMode::Default, |p| p.mode)
    }

    #[must_use]
    pub fn pool(&self) -> &McpPool {
        &self.pool
    }

    /// A gate for a reconfigured session: the same approver and the same
    /// session "always" answers, over `pool` and `policy`. Built-ins, blue
    /// tools, hooks and the guard are the caller's to add again (they come
    /// from the new config). The running turn keeps the gate it started
    /// with; the next turn gets this one.
    #[must_use]
    pub fn successor(&self, pool: Arc<McpPool>, policy: Policy) -> Self {
        let next = Self::new(pool, self.approver.clone(), policy);
        if let (Ok(mine), Ok(mut theirs)) = (self.always.lock(), next.always.lock()) {
            theirs.clone_from(&mine);
        }
        next
    }
}

#[async_trait]
impl ToolDispatcher for GatedDispatcher {
    async fn dispatch(&self, name: &str, input: &serde_json::Value) -> Result<String> {
        // The search meta-tool only reads the local catalogue.
        if name == SEARCH_TOOL {
            return Ok(self.pool.call(name, input).await?);
        }
        // A deferred call is judged by the tool it actually invokes.
        let (tool, args) = if name == CALL_TOOL {
            let inner = input.get("name").and_then(serde_json::Value::as_str).unwrap_or_default();
            (inner.to_owned(), input.get("arguments").cloned().unwrap_or(serde_json::json!({})))
        } else {
            (name.to_owned(), input.clone())
        };
        let read_only = if self.is_skill(&tool) {
            Some(true)
        } else if self.is_builtin(&tool) {
            arnes_tools::Builtins::read_only(&tool)
        } else if self.blue.owns(&tool) {
            self.blue.read_only(&tool)
        } else {
            self.pool.read_only(&tool)
        };
        let mut decision = {
            let policy = self.policy.lock().map_err(|_| LoopError::Dispatch("policy lock".into()))?;
            let always = self.always.lock().map_err(|_| LoopError::Dispatch("policy lock".into()))?;
            policy.decide(&tool, read_only, &always)
        };
        // guardrail sits above the policy: a Block refuses in every mode, a
        // Warn forces the question even where a rule or mode would allow.
        let mut warning = None;
        if self.is_builtin(&tool) {
            match self.guard.screen(&tool, &args) {
                Screen::Clear => {}
                Screen::Block(why) => return Err(LoopError::Dispatch(why)),
                Screen::Warn(why) => {
                    warning = Some(why);
                    if decision == Decision::Allow {
                        decision = Decision::Ask;
                    }
                }
            }
        }
        // blue `before-tool` hooks: a veto after the rules, before a human is
        // asked, so a hook's refusal never costs the operator a question.
        if !matches!(decision, Decision::Deny(_)) {
            let (verdict, effects) = self.hooks.before_tool(&tool, &args).await;
            forward_effects("hook before-tool", &effects);
            if let arnes_blue::ToolVerdict::Deny(why) = verdict {
                return Err(LoopError::Dispatch(why));
            }
        }
        match decision {
            Decision::Allow => {}
            Decision::Deny(why) => return Err(LoopError::Dispatch(why)),
            Decision::Ask => {
                let req = ApprovalRequest { tool: tool.clone(), input: args.clone(), read_only, warning };
                match self.approver.ask(req).await {
                    Verdict::Allow => {}
                    Verdict::AllowAlways => {
                        if let Ok(mut a) = self.always.lock() {
                            a.insert(tool.clone());
                        }
                    }
                    Verdict::Deny(why) => {
                        return Err(LoopError::Dispatch(format!("the user declined {tool}: {why}")));
                    }
                }
            }
        }
        if let (true, Some(s)) = (self.is_skill(&tool), &self.skills) {
            return s.call(&args).map_err(|e| LoopError::Dispatch(e.to_string()));
        }
        if let (true, Some(b)) = (self.is_builtin(&tool), &self.builtins) {
            return b.call(&tool, &args).await.map_err(|e| LoopError::Dispatch(e.to_string()));
        }
        if self.blue.owns(&tool) {
            let outcome = self.blue.call(&tool, &args).await.map_err(|e| LoopError::Dispatch(e.to_string()))?;
            forward_effects(&tool, &outcome.effects);
            return Ok(outcome.text().to_owned());
        }
        Ok(self.pool.call(&tool, &args).await?)
    }
}

/// A blue tool runs inside a turn, away from the prompt: its hints and toasts
/// become notices (the TUI shows them as toasts), and a prompt insert, which
/// only a slash command can make, is reported rather than dropped.
fn forward_effects(tool: &str, effects: &[arnes_blue::Effect]) {
    use arnes_blue::{Effect, Level};
    for e in effects {
        match e {
            Effect::Hint(t) | Effect::Toast(Level::Info, t) => tracing::info!(target: "arnes::notice", "{tool}: {t}"),
            Effect::Toast(Level::Warn, t) => tracing::warn!("{tool}: {t}"),
            Effect::Toast(Level::Error, t) => tracing::error!("{tool}: {t}"),
            Effect::Insert(_) => tracing::warn!("{tool}: arnes_insert cannot edit the prompt from inside a turn"),
            Effect::Arm { .. } | Effect::Panel { .. } | Effect::Set { .. } => {
                tracing::warn!("{tool}: arnes_clear, arnes_exit and arnes_panel work from a command, not inside a turn");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arnes_core::{Tool, ToolInputSchema};

    fn policy(mode: PermissionMode) -> Policy {
        Policy { mode, allow: vec![], deny: vec![] }
    }

    #[test]
    fn mode_table() {
        let none = HashSet::new();
        for mode in PermissionMode::ALL {
            let read = policy(mode).decide("t", Some(true), &none);
            let write = policy(mode).decide("t", Some(false), &none);
            let silent = policy(mode).decide("t", None, &none);
            let expected_write = match mode {
                PermissionMode::Default => Decision::Ask,
                PermissionMode::Bypass => Decision::Allow,
                PermissionMode::Plan => write.clone(),
            };
            assert_eq!(read, Decision::Allow, "{mode:?}: reads always allowed");
            assert_eq!(write, expected_write, "{mode:?}");
            assert_eq!(silent, write, "{mode:?}: an unannotated tool is treated as able to write");
        }
        assert!(matches!(policy(PermissionMode::Plan).decide("t", None, &none), Decision::Deny(_)));
    }

    #[test]
    fn deny_rules_win_over_everything() {
        let p = Policy { mode: PermissionMode::Bypass, allow: vec!["mcp__*".into()], deny: vec!["mcp__github__*".into()] };
        let always: HashSet<String> = ["mcp__github__merge".to_string()].into();
        assert!(matches!(p.decide("mcp__github__merge", Some(true), &always), Decision::Deny(_)));
        assert_eq!(p.decide("mcp__codesearch__search", None, &HashSet::new()), Decision::Allow);
    }

    #[test]
    fn globs() {
        assert!(glob("mcp__codesearch__*", "mcp__codesearch__search_exact"));
        assert!(glob("*__get_*", "mcp__kubernetes__get_pod_logs"));
        assert!(!glob("mcp__github__*", "mcp__gitlab__x"));
        assert!(glob("exact", "exact") && !glob("exact", "exactly"));
    }

    #[test]
    fn modes_cycle_and_parse() {
        assert_eq!(PermissionMode::Default.next().next().next(), PermissionMode::Default);
        assert_eq!(PermissionMode::parse("plan"), Ok(PermissionMode::Plan));
        assert!(PermissionMode::parse("yolo").is_err());
    }

    struct Scripted(Verdict, Mutex<Vec<String>>);

    #[async_trait]
    impl Approver for Scripted {
        async fn ask(&self, req: ApprovalRequest) -> Verdict {
            self.1.lock().unwrap().push(req.tool);
            self.0.clone()
        }
    }

    fn pool() -> Arc<McpPool> {
        let t = |n: &str| Tool { name: n.into(), description: String::new(), input_schema: ToolInputSchema(serde_json::json!({"type": "object"})) };
        Arc::new(McpPool::with_tools(vec![t("mcp__s__read"), t("mcp__s__write")], &["mcp__s__read"]))
    }

    #[tokio::test]
    async fn a_decline_reaches_the_model_as_an_error_with_the_reason() {
        let approver = Arc::new(Scripted(Verdict::Deny("not now".into()), Mutex::new(vec![])));
        let g = GatedDispatcher::new(pool(), approver.clone(), Policy::default());
        let err = g.dispatch("mcp__s__write", &serde_json::json!({})).await.unwrap_err().to_string();
        assert!(err.contains("declined") && err.contains("not now"), "{err}");
        assert_eq!(approver.1.lock().unwrap().as_slice(), ["mcp__s__write"]);
    }

    #[tokio::test]
    async fn mcp_call_is_gated_by_the_inner_tool() {
        let approver = Arc::new(Scripted(Verdict::Deny("no".into()), Mutex::new(vec![])));
        let g = GatedDispatcher::new(pool(), approver.clone(), Policy::default());
        let _ = g.dispatch(CALL_TOOL, &serde_json::json!({"name": "mcp__s__write", "arguments": {}})).await;
        assert_eq!(approver.1.lock().unwrap().as_slice(), ["mcp__s__write"], "asked about the real tool");
    }

    #[tokio::test]
    async fn read_only_tools_and_search_are_never_asked() {
        let approver = Arc::new(Scripted(Verdict::Deny("no".into()), Mutex::new(vec![])));
        let g = GatedDispatcher::new(pool(), approver.clone(), Policy::default());
        let _ = g.dispatch("mcp__s__read", &serde_json::json!({})).await;
        let _ = g.dispatch(SEARCH_TOOL, &serde_json::json!({"query": "x"})).await;
        assert!(approver.1.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn always_skips_later_questions() {
        let approver = Arc::new(Scripted(Verdict::AllowAlways, Mutex::new(vec![])));
        let g = GatedDispatcher::new(pool(), approver.clone(), Policy::default());
        let _ = g.dispatch("mcp__s__write", &serde_json::json!({})).await;
        let _ = g.dispatch("mcp__s__write", &serde_json::json!({})).await;
        assert_eq!(approver.1.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_successor_gate_keeps_the_session_answers_and_takes_the_new_policy() {
        let approver = Arc::new(Scripted(Verdict::AllowAlways, Mutex::new(vec![])));
        let g = GatedDispatcher::new(pool(), approver.clone(), Policy::default());
        let _ = g.dispatch("mcp__s__write", &serde_json::json!({})).await;
        let next = g.successor(pool(), Policy { mode: PermissionMode::Plan, ..Policy::default() });
        assert_eq!(next.mode(), PermissionMode::Plan);
        let next = g.successor(pool(), Policy::default());
        let _ = next.dispatch("mcp__s__write", &serde_json::json!({})).await;
        assert_eq!(approver.1.lock().unwrap().len(), 1, "the always answer carried over");
    }

    #[tokio::test]
    async fn headless_denies_with_the_way_out() {
        let g = GatedDispatcher::new(pool(), Arc::new(NoHuman), Policy::default());
        let err = g.dispatch("mcp__s__write", &serde_json::json!({})).await.unwrap_err().to_string();
        assert!(err.contains("allow rule") && err.contains("bypass"), "{err}");
    }

    #[tokio::test]
    async fn builtin_reads_run_unasked_and_writes_are_asked() {
        let dir = std::env::temp_dir().join(format!("arnes-gate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.txt"), "x\n").unwrap();
        let approver = Arc::new(Scripted(Verdict::Deny("no".into()), Mutex::new(vec![])));
        let g = GatedDispatcher::new(pool(), approver.clone(), Policy::default())
            .with_builtins(arnes_tools::Builtins::new(&dir));
        let read = g.dispatch("Read", &serde_json::json!({"file_path": "f.txt"})).await.unwrap();
        assert!(read.contains('x'));
        let err = g.dispatch("Bash", &serde_json::json!({"command": "echo hi"})).await.unwrap_err();
        assert!(err.to_string().contains("declined"));
        assert_eq!(approver.1.lock().unwrap().as_slice(), ["Bash"], "only the side-effecting tool was asked");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn the_skill_tool_is_read_only_served_in_plan_mode_and_absent_without_skills() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("x")).unwrap();
        std::fs::write(dir.path().join("x/SKILL.md"), "---\nname: x\ndescription: d\n---\nthe body\n").unwrap();
        let (skills, _) = arnes_tools::skills::Skills::discover(&[dir.path().to_path_buf()], &[], &[], 10);
        let approver = Arc::new(Scripted(Verdict::Deny("no".into()), Mutex::new(vec![])));
        let plan = Policy { mode: PermissionMode::Plan, ..Policy::default() };
        let g = GatedDispatcher::new(pool(), approver.clone(), plan.clone()).with_skills(Arc::new(skills));
        assert_eq!(g.dispatch("Skill", &serde_json::json!({"name": "x"})).await.unwrap(), "the body\n");
        assert!(approver.1.lock().unwrap().is_empty(), "never asked: it only reads");
        let none = GatedDispatcher::new(pool(), approver, plan).with_skills(Arc::new(arnes_tools::skills::Skills::default()));
        assert!(none.dispatch("Skill", &serde_json::json!({"name": "x"})).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn blue_tools_are_served_and_gated_like_the_rest() {
        let dir = std::env::temp_dir().join(format!("arnes-gate-blue-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.b");
        std::fs::write(&path, "def echo(input)\n  json_get(json_parse(input), \"text\")\nend\n").unwrap();
        let spec = |name: &str, read_only| arnes_blue::ToolSpec {
            name: name.into(),
            description: String::new(),
            schema: None,
            read_only,
            script: arnes_blue::Script::new(&path, "echo"),
        };
        let approver = Arc::new(Scripted(Verdict::Deny("no".into()), Mutex::new(vec![])));
        let g = GatedDispatcher::new(pool(), approver.clone(), Policy::default())
            .with_blue_tools(arnes_blue::Tools::new(vec![spec("peek", true), spec("poke", false)]));
        let out = g.dispatch("peek", &serde_json::json!({"text": "hi"})).await.unwrap();
        assert_eq!(out, "hi");
        let err = g.dispatch("poke", &serde_json::json!({"text": "hi"})).await.unwrap_err();
        assert!(err.to_string().contains("declined"), "{err}");
        assert_eq!(approver.1.lock().unwrap().as_slice(), ["poke"], "only the side-effecting blue tool was asked");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_before_tool_hook_vetoes_before_anyone_is_asked() {
        let dir = std::env::temp_dir().join(format!("arnes-gate-hook-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("h.b");
        std::fs::write(&path, "def guard(tool, input)\n  if tool == \"mcp__s__write\"\n    \"writes are off today\"\n  else\n    \"allow\"\n  end\nend\n").unwrap();
        let approver = Arc::new(Scripted(Verdict::Allow, Mutex::new(vec![])));
        let g = GatedDispatcher::new(pool(), approver.clone(), Policy::default())
            .with_hooks(arnes_blue::Hooks::new(vec![(arnes_blue::HookEvent::BeforeTool, arnes_blue::Script::new(&path, "guard"))]));
        let err = g.dispatch("mcp__s__write", &serde_json::json!({})).await.unwrap_err().to_string();
        assert!(err.contains("writes are off today"), "{err}");
        assert!(approver.1.lock().unwrap().is_empty(), "the human is not asked about a vetoed call");
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn guarded(approver: Arc<Scripted>, dir: &std::path::Path, mode: PermissionMode) -> GatedDispatcher {
        let rules = guardrail::config::default_rules();
        let engine = guardrail::RegexEngine::new(rules).unwrap();
        GatedDispatcher::new(pool(), approver, Policy { mode, allow: vec![], deny: vec![] })
            .with_builtins(arnes_tools::Builtins::new(dir))
            .with_guard(Guard::On(Box::new(engine)))
    }

    #[tokio::test]
    async fn guardrail_blocks_before_anyone_is_asked_even_in_bypass() {
        let dir = std::env::temp_dir().join(format!("arnes-guard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let approver = Arc::new(Scripted(Verdict::Allow, Mutex::new(vec![])));
        let g = guarded(approver.clone(), &dir, PermissionMode::Bypass);
        let err = g.dispatch("Bash", &serde_json::json!({"command": "rm -rf /"})).await.unwrap_err().to_string();
        assert!(err.contains("guardrail blocked") && err.contains("rm-rf-root"), "{err}");
        assert!(approver.1.lock().unwrap().is_empty(), "a block never reaches the question");
        let ok = g.dispatch("Bash", &serde_json::json!({"command": "echo fine"})).await.unwrap();
        assert!(ok.contains("fine"), "bypass still runs clean commands");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unavailable_guard_fails_toward_asking_for_bash() {
        let g = Guard::Unavailable("no config".into());
        assert!(matches!(g.screen("Bash", &serde_json::json!({"command": "ls"})), Screen::Warn(_)));
        assert_eq!(g.screen("Read", &serde_json::json!({})), Screen::Clear);
    }

    #[test]
    fn dangerous_write_content_warns() {
        let rules = guardrail::config::default_rules();
        let g = Guard::On(Box::new(guardrail::RegexEngine::new(rules).unwrap()));
        let s = g.screen("Write", &serde_json::json!({"file_path": "x.sh", "content": "#!/bin/sh\nrm -rf /\n"}));
        assert!(matches!(s, Screen::Warn(ref w) if w.contains("new content")), "{s:?}");
    }
}
