//! `arnes-loop` — the agentic conversation loop.
//!
//! ```text
//!   user turn  ──► Anthropic API ──► assistant message
//!                       │
//!                       ▼
//!                  stop_reason?
//!                       │
//!             ┌─────────┼──────────┐
//!             ▼                    ▼
//!         tool_use             end_turn / max_tokens
//!             │                    │
//!   dispatch via MCP          return to user
//!             │
//!   append ToolResult
//!             │
//!             └── back to top
//! ```
//!
//! Phase 1.B: non-streaming only. Streaming lands when the binary wires
//! SSE parsing. The loop is generic over a `ToolDispatcher` trait — the
//! TUI binds it to its MCP client; tests use an in-memory fake.

pub mod ledger;
pub mod permission;

use arnes_anthropic::{AnthropicClient, MessagesResponse};
use arnes_core::{ContentBlock, Conversation, Message, Role, StopReason, Tool, ToolResult, Usage};
use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LoopError {
    #[error("provider: {0}")]
    Anthropic(#[from] arnes_anthropic::AnthropicError),
    #[error("mcp: {0}")]
    Mcp(#[from] arnes_mcp_client::McpError),
    #[error("tool dispatch: {0}")]
    Dispatch(String),
    #[error("exceeded max turns ({0})")]
    MaxTurns(u32),
    /// The prompt outgrew the model's window — the caller masks or
    /// compacts history and retries (docs/token-efficiency.md §4.5).
    #[error("context window exceeded")]
    ContextWindowExceeded,
}

pub type Result<T> = std::result::Result<T, LoopError>;

/// How the loop resolves `tool_use` blocks — the TUI binds this to its
/// live MCP client; tests use an in-memory fake that never touches I/O.
#[async_trait]
pub trait ToolDispatcher: Send + Sync {
    async fn dispatch(&self, name: &str, input: &serde_json::Value) -> Result<String>;

    /// Whether this call may run beside others from the same reply: it
    /// changes nothing and will not stop to ask. Unknown is no.
    fn concurrent(&self, _name: &str, _input: &serde_json::Value) -> bool {
        false
    }
}

/// The live MCP pool dispatches `mcp__<server>__<tool>` calls.
#[async_trait]
impl ToolDispatcher for arnes_mcp_client::McpPool {
    async fn dispatch(&self, name: &str, input: &serde_json::Value) -> Result<String> {
        Ok(self.call(name, input).await?)
    }
}

/// Run one agentic turn — send the conversation, handle any tool calls,
/// loop until `stop_reason = end_turn` or `max_turns`.
pub async fn run_turn<D: ToolDispatcher>(client: &AnthropicClient, conv: &mut Conversation, tools: &[Tool], dispatcher: &D, max_turns: u32) -> Result<Usage> {
    run_turn_recorded(client, conv, tools, dispatcher, max_turns, None).await
}

/// What passes between a running turn and the UI. In: prompts the user sent
/// while the turn ran, handed to the model at its next tool boundary (the UI
/// offers what is waiting, the turn takes it, the UI learns what was
/// delivered and drops it from its queue). Out: the conversation as of the
/// last finished round, so an interrupt keeps the work already done.
#[derive(Debug, Clone, Default)]
pub struct TurnLink(std::sync::Arc<std::sync::Mutex<LinkState>>);

#[derive(Debug, Default)]
struct LinkState {
    waiting: Vec<String>,
    delivered: Vec<String>,
    checkpoint: Option<Conversation>,
    /// Bumped on each checkpoint, so the UI redraws a round once.
    generation: u64,
}

impl TurnLink {
    /// Record `conv` as the last consistent point of the running turn: every
    /// tool use answered.
    pub fn checkpoint(&self, conv: &Conversation) {
        if let Ok(mut s) = self.0.lock() {
            s.checkpoint = Some(conv.clone());
            s.generation += 1;
        }
    }

    /// The checkpoint, when one landed after generation `seen`: the UI
    /// shows each finished round while the turn runs on.
    pub fn progress_since(&self, seen: u64) -> Option<(u64, Conversation)> {
        let s = self.0.lock().ok()?;
        (s.generation > seen).then(|| s.checkpoint.clone().map(|c| (s.generation, c))).flatten()
    }

    /// The last checkpoint, taken: what an interrupted turn had finished.
    pub fn take_checkpoint(&self) -> Option<Conversation> {
        self.0.lock().ok().and_then(|mut s| s.checkpoint.take())
    }

    /// Replace what is waiting with `prompts`, the UI's queue as it stands.
    pub fn offer(&self, prompts: Vec<String>) {
        if let Ok(mut s) = self.0.lock() {
            s.waiting = prompts;
        }
    }

    /// Everything waiting, now delivered.
    pub fn take(&self) -> Vec<String> {
        let Ok(mut s) = self.0.lock() else { return Vec::new() };
        let taken = std::mem::take(&mut s.waiting);
        s.delivered.extend(taken.iter().cloned());
        taken
    }

    /// What the turn took since the last call, oldest first.
    pub fn delivered(&self) -> Vec<String> {
        self.0.lock().map(|mut s| std::mem::take(&mut s.delivered)).unwrap_or_default()
    }
}

/// The text block a prompt sent mid-turn becomes, after that round's tool
/// results.
#[must_use]
pub fn mid_turn_text(prompt: &str) -> ContentBlock {
    ContentBlock::Text { text: format!("[The user sent this while you were working:]\n{prompt}") }
}

/// The model's window is full: the stop reason says so, or the provider
/// refused the request for its length (llama.cpp answers 400 "exceeds the
/// available context size").
#[must_use]
pub fn is_context_overflow(e: &LoopError) -> bool {
    match e {
        LoopError::ContextWindowExceeded => true,
        LoopError::Anthropic(arnes_anthropic::AnthropicError::Api { status: 400 | 413, body }) => {
            let b = body.to_ascii_lowercase();
            b.contains("context") && (b.contains("exceed") || b.contains("too long") || b.contains("too many tokens") || b.contains("context length") || b.contains("context size"))
        }
        _ => false,
    }
}

/// Where a compaction cuts: the start of the exchange in progress, the last
/// user message that is a prompt rather than tool results. Everything
/// before it is summarized; it and what follows are kept word for word.
#[must_use]
pub fn compaction_cut(conv: &Conversation) -> Option<usize> {
    conv.messages.iter().rposition(|m| m.role == Role::User && !m.content.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. }))).filter(|&i| i > 0)
}

/// Replace everything before the exchange in progress with a summary the
/// model writes of it (no tools, no stream). `focus`, when given, says what
/// the summary must keep. Returns the summary call's usage; `Ok(None)` when
/// there is nothing before the current exchange to summarize.
///
/// # Errors
/// The summary request failed; the conversation is untouched.
pub async fn compact(client: &AnthropicClient, conv: &mut Conversation, focus: Option<&str>) -> Result<Option<Usage>> {
    let Some(cut) = compaction_cut(conv) else { return Ok(None) };
    let mut ask = conv.clone();
    ask.messages.truncate(cut);
    let close = ask.messages.last().is_some_and(|m| m.role == Role::User);
    if close {
        ask.push(Message::assistant_text("(continuing)"));
    }
    let focus = focus.map_or(String::new(), |f| format!(" Above all keep: {f}."));
    ask.push(Message::user_text(format!(
        "Summarize the conversation so far for your own later use, in place of it: the goal, decisions and their reasons, files and commands that matter, what is done, what is open, and anything the user asked you to remember. Be complete but brief; write only the summary.{focus}"
    )));
    let mut quiet = client.clone();
    quiet.stream = None;
    let resp = quiet.send(&ask, &[]).await?;
    let summary = resp.to_message().text();
    let tail = conv.messages.split_off(cut);
    conv.messages = vec![
        Message::user_text(format!("[A summary of the earlier conversation, which you wrote when it was compacted:]\n{summary}")),
        Message::assistant_text("Understood; I continue from that summary."),
    ];
    conv.messages.extend(tail);
    Ok(Some(resp.usage))
}

/// [`run_turn`], recording every API call in `ledger` as it returns.
pub async fn run_turn_recorded<D: ToolDispatcher>(client: &AnthropicClient, conv: &mut Conversation, tools: &[Tool], dispatcher: &D, max_turns: u32, ledger: Option<&ledger::Ledger>) -> Result<Usage> {
    run_turn_with(client, conv, tools, dispatcher, max_turns, &TurnOpts { ledger, ..TurnOpts::default() }).await
}

/// How one turn runs beyond its client, tools and rounds.
#[derive(Default, Clone, Copy)]
pub struct TurnOpts<'a> {
    /// Every API call is recorded here as it returns.
    pub ledger: Option<&'a ledger::Ledger>,
    /// Prompts in, checkpoints out (see [`TurnLink`]).
    pub link: Option<&'a TurnLink>,
    /// The same call (tool and input) made this many times in one turn adds
    /// a note after that round's results; 0 never.
    pub loop_nudge: u32,
}

/// The note a repeated call earns: the model is told it is looping.
#[must_use]
pub fn loop_note(name: &str, times: u32) -> ContentBlock {
    ContentBlock::Text {
        text: format!(
            "[arnes: you have called {name} with the same input {times} times this turn and it answers the same way. Do not call it again with that input; change the approach, or say what blocks you.]"
        ),
    }
}

/// [`run_turn_recorded`], handing the model what `link` holds at each tool
/// boundary and checkpointing each finished round into it.
pub async fn run_turn_with<D: ToolDispatcher>(client: &AnthropicClient, conv: &mut Conversation, tools: &[Tool], dispatcher: &D, max_turns: u32, opts: &TurnOpts<'_>) -> Result<Usage> {
    let (ledger, link) = (opts.ledger, opts.link);
    let mut seen: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut total_usage = Usage::default();
    for turn in 0..max_turns {
        let resp: MessagesResponse = client.send(conv, tools).await?;
        if let (Some(l), Some(s)) = (ledger, client.served()) {
            l.record(&s, &resp.model, &resp.usage);
        }
        total_usage.merge(resp.usage);
        conv.push(resp.to_message());

        match resp.stop_reason {
            Some(StopReason::ToolUse) => {
                let uses = resp.tool_uses();
                if uses.is_empty() {
                    tracing::warn!(turn, "stop_reason=tool_use but no tool uses present");
                    break;
                }
                // Calls that change nothing and will not ask run together;
                // a reply with any other call runs them one by one, in order.
                let together = uses.len() > 1 && uses.iter().all(|(_, name, input)| dispatcher.concurrent(name, input));
                let outcomes = if together {
                    futures::future::join_all(uses.iter().map(|(_, name, input)| dispatcher.dispatch(name, input))).await
                } else {
                    let mut out = Vec::with_capacity(uses.len());
                    for (_, name, input) in &uses {
                        out.push(dispatcher.dispatch(name, input).await);
                    }
                    out
                };
                let mut result_blocks: Vec<ContentBlock> = Vec::with_capacity(uses.len());
                let mut notes = Vec::new();
                for (_, name, input) in &uses {
                    let n = seen.entry(format!("{name}\u{0}{input}")).or_default();
                    *n += 1;
                    if opts.loop_nudge > 0 && *n == opts.loop_nudge {
                        notes.push(loop_note(name, *n));
                    }
                }
                for ((id, name, _), outcome) in uses.into_iter().zip(outcomes) {
                    match outcome {
                        Ok(text) => result_blocks.push(ContentBlock::ToolResult { tool_use_id: id, content: text, is_error: false }),
                        Err(e) => {
                            tracing::warn!(tool = %name, error = %e, "tool dispatch failed");
                            result_blocks.push(ContentBlock::ToolResult { tool_use_id: id, content: format!("{e}"), is_error: true });
                        }
                    }
                }
                result_blocks.extend(notes);
                if let Some(l) = link {
                    result_blocks.extend(l.take().iter().map(|p| mid_turn_text(p)));
                }
                conv.push(Message { role: Role::User, content: result_blocks });
                if let Some(l) = link {
                    l.checkpoint(conv);
                }
            }
            // The server paused a long turn; resending the conversation
            // as-is lets it continue.
            Some(StopReason::PauseTurn) => {}
            Some(StopReason::ModelContextWindowExceeded) => {
                return Err(LoopError::ContextWindowExceeded);
            }
            Some(StopReason::Unknown) => {
                tracing::warn!(turn, "unknown stop_reason; treating as end of turn");
                return Ok(total_usage);
            }
            Some(StopReason::EndTurn | StopReason::MaxTokens | StopReason::StopSequence | StopReason::Refusal) | None => {
                return Ok(total_usage);
            }
        }
    }
    Err(LoopError::MaxTurns(max_turns))
}

/// Apply a pre-computed tool result to the conversation — used by UIs
/// that want to render each step before making the next request.
pub fn append_tool_result(conv: &mut Conversation, result: ToolResult) {
    conv.push(Message { role: Role::User, content: vec![ContentBlock::ToolResult { tool_use_id: result.tool_use_id, content: result.content, is_error: result.is_error }] });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake dispatcher that returns canned responses by name.
    struct FakeDispatcher;

    #[async_trait]
    impl ToolDispatcher for FakeDispatcher {
        async fn dispatch(&self, name: &str, _input: &serde_json::Value) -> Result<String> {
            Ok(format!("fake result for {name}"))
        }
    }

    #[tokio::test]
    async fn append_tool_result_adds_user_message() {
        let mut c = Conversation::default();
        append_tool_result(&mut c, ToolResult::ok("t1", "hello from tool"));
        assert_eq!(c.messages.len(), 1);
        match &c.messages[0].content[0] {
            ContentBlock::ToolResult { content, .. } => assert_eq!(content, "hello from tool"),
            other => panic!("expected tool_result, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_prompt_sent_mid_turn_follows_the_tool_results_and_the_round_is_checkpointed() {
        const TOOL: &str = r#"{"id":"m1","type":"message","role":"assistant","model":"m","content":[{"type":"tool_use","id":"t1","name":"read_file","input":{}}],"stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":1}}"#;
        const DONE: &str =
            r#"{"id":"m2","type":"message","role":"assistant","model":"m","content":[{"type":"text","text":"done"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#;
        let (url, seen) = arnes_anthropic::testing::scripted_server(vec![("200 OK", TOOL), ("200 OK", DONE)]);
        let client = AnthropicClient::new(None).with_base_url(url);
        let mut conv = Conversation::default();
        conv.push(Message::user_text("look at a"));
        let inbox = TurnLink::default();
        inbox.offer(vec!["also check b".into()]);
        run_turn_with(&client, &mut conv, &[], &FakeDispatcher, 4, &TurnOpts { link: Some(&inbox), ..TurnOpts::default() }).await.unwrap();
        let sent = seen.join().unwrap();
        assert!(!sent[0].contains("also check b"), "the first call predates the tool boundary");
        assert!(sent[1].contains("while you were working") && sent[1].contains("also check b"), "{}", sent[1]);
        let results = &conv.messages[2].content;
        assert!(matches!(results[0], ContentBlock::ToolResult { .. }), "tool results come first");
        assert!(matches!(&results[1], ContentBlock::Text { text } if text.ends_with("also check b")));
        assert_eq!(inbox.delivered(), ["also check b"]);
        let (generation, shown) = inbox.progress_since(0).expect("the round is shown while the turn runs");
        assert_eq!((generation, shown.messages.len()), (1, 3));
        let kept = inbox.take_checkpoint().expect("the finished round is checkpointed");
        assert_eq!(kept.messages.len(), 3, "the prompt, the tool use, and its answered results");
        assert!(matches!(kept.messages[2].content[0], ContentBlock::ToolResult { .. }));
        assert!(inbox.take_checkpoint().is_none(), "taken once");
        assert!(inbox.progress_since(1).is_none(), "one round, one generation");
        assert!(inbox.take().is_empty() && inbox.delivered().is_empty(), "delivered once");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn compaction_summarizes_before_the_exchange_in_progress_and_keeps_it() {
        const SUMMARY: &str =
            r#"{"id":"s","type":"message","role":"assistant","model":"m","content":[{"type":"text","text":"we fixed a.rs"}],"stop_reason":"end_turn","usage":{"input_tokens":9,"output_tokens":3}}"#;
        let mut conv = Conversation::default();
        conv.push(Message::user_text("fix a.rs"));
        conv.push(Message::assistant_text("fixed"));
        conv.push(Message::user_text("now b.rs"));
        conv.push(Message { role: Role::Assistant, content: vec![ContentBlock::ToolUse { id: "t".into(), name: "Read".into(), input: serde_json::json!({}) }] });
        conv.push(Message { role: Role::User, content: vec![ContentBlock::ToolResult { tool_use_id: "t".into(), content: "b".into(), is_error: false }] });
        assert_eq!(compaction_cut(&conv), Some(2), "tool results are not a prompt");
        let (url, seen) = arnes_anthropic::testing::one_shot_server("200 OK", SUMMARY);
        let client = AnthropicClient::new(None).with_base_url(url);
        assert!(compact(&client, &mut conv, Some("the b.rs plan")).await.unwrap().is_some());
        let asked = seen.join().unwrap();
        assert!(asked.contains("fix a.rs") && !asked.contains("now b.rs") && asked.contains("the b.rs plan") && !asked.contains("\"tools\""), "{asked}");
        assert_eq!(conv.messages.len(), 5);
        assert!(conv.messages[0].text().contains("we fixed a.rs"));
        assert_eq!(conv.messages[2].text(), "now b.rs", "the exchange in progress is kept word for word");
        let mut fresh = Conversation::default();
        fresh.push(Message::user_text("only prompt"));
        assert!(compact(&client, &mut fresh, None).await.unwrap().is_none(), "nothing before it, nothing to do");
    }

    #[test]
    fn an_overflow_is_the_stop_reason_or_a_providers_length_refusal() {
        let api = |status, body: &str| LoopError::Anthropic(arnes_anthropic::AnthropicError::Api { status, body: body.into() });
        assert!(is_context_overflow(&LoopError::ContextWindowExceeded));
        assert!(is_context_overflow(&api(400, r#"{"error":{"message":"the request exceeds the available context size, try increasing it"}}"#)));
        assert!(is_context_overflow(&api(400, "prompt is too long: context length 200000")));
        assert!(!is_context_overflow(&api(400, "invalid tool schema")));
        assert!(!is_context_overflow(&api(500, "context exceeded")), "a server error is not a length refusal");
    }

    struct Slow(bool);

    #[async_trait]
    impl ToolDispatcher for Slow {
        async fn dispatch(&self, name: &str, _input: &serde_json::Value) -> Result<String> {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            Ok(format!("done {name}"))
        }
        fn concurrent(&self, _name: &str, _input: &serde_json::Value) -> bool {
            self.0
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn calls_that_change_nothing_run_together_and_answer_in_order() {
        const TWO: &str = r#"{"id":"m1","type":"message","role":"assistant","model":"m","content":[{"type":"tool_use","id":"a","name":"Read","input":{}},{"type":"tool_use","id":"b","name":"Grep","input":{}}],"stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":1}}"#;
        const DONE: &str =
            r#"{"id":"m2","type":"message","role":"assistant","model":"m","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#;
        for (together, under) in [(true, true), (false, false)] {
            let (url, _seen) = arnes_anthropic::testing::scripted_server(vec![("200 OK", TWO), ("200 OK", DONE)]);
            let client = AnthropicClient::new(None).with_base_url(url);
            let mut conv = Conversation::default();
            conv.push(Message::user_text("look"));
            let t = std::time::Instant::now();
            run_turn(&client, &mut conv, &[], &Slow(together), 4).await.unwrap();
            assert_eq!(t.elapsed() < std::time::Duration::from_millis(550), under, "together = {together}: {:?}", t.elapsed());
            let ids: Vec<_> = conv.messages[2]
                .content
                .iter()
                .map(|b| match b {
                    ContentBlock::ToolResult { tool_use_id, content, .. } => format!("{tool_use_id}:{content}"),
                    _ => String::new(),
                })
                .collect();
            assert_eq!(ids, ["a:done Read", "b:done Grep"], "results keep the reply's order");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_same_call_made_again_and_again_earns_one_note() {
        const SAME: &str = r#"{"id":"m","type":"message","role":"assistant","model":"m","content":[{"type":"tool_use","id":"x","name":"Grep","input":{"pattern":"foo"}}],"stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":1}}"#;
        const OTHER: &str = r#"{"id":"m","type":"message","role":"assistant","model":"m","content":[{"type":"tool_use","id":"y","name":"Grep","input":{"pattern":"bar"}}],"stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":1}}"#;
        const DONE: &str =
            r#"{"id":"m","type":"message","role":"assistant","model":"m","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#;
        let (url, _seen) = arnes_anthropic::testing::scripted_server(vec![("200 OK", SAME), ("200 OK", OTHER), ("200 OK", SAME), ("200 OK", SAME), ("200 OK", SAME), ("200 OK", DONE)]);
        let client = AnthropicClient::new(None).with_base_url(url);
        let mut conv = Conversation::default();
        conv.push(Message::user_text("find foo"));
        run_turn_with(&client, &mut conv, &[], &FakeDispatcher, 8, &TurnOpts { loop_nudge: 3, ..TurnOpts::default() }).await.unwrap();
        let noted: Vec<usize> =
            conv.messages.iter().enumerate().filter(|(_, m)| m.content.iter().any(|b| matches!(b, ContentBlock::Text { text } if text.contains("same input 3 times")))).map(|(i, _)| i).collect();
        assert_eq!(noted, [8], "the third identical call, once; the pattern change does not count");
    }

    #[tokio::test]
    async fn fake_dispatcher_returns_canned() {
        let d = FakeDispatcher;
        let r = d.dispatch("read_file", &serde_json::json!({})).await.unwrap();
        assert_eq!(r, "fake result for read_file");
    }
}
