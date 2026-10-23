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
pub async fn run_turn<D: ToolDispatcher>(
    client: &AnthropicClient,
    conv: &mut Conversation,
    tools: &[Tool],
    dispatcher: &D,
    max_turns: u32,
) -> Result<Usage> {
    run_turn_recorded(client, conv, tools, dispatcher, max_turns, None).await
}

/// [`run_turn`], recording every API call in `ledger` as it returns.
pub async fn run_turn_recorded<D: ToolDispatcher>(
    client: &AnthropicClient,
    conv: &mut Conversation,
    tools: &[Tool],
    dispatcher: &D,
    max_turns: u32,
    ledger: Option<&ledger::Ledger>,
) -> Result<Usage> {
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
                let mut result_blocks: Vec<ContentBlock> = Vec::with_capacity(uses.len());
                for (id, name, input) in uses {
                    match dispatcher.dispatch(&name, &input).await {
                        Ok(text) => result_blocks.push(ContentBlock::ToolResult {
                            tool_use_id: id,
                            content: text,
                            is_error: false,
                        }),
                        Err(e) => {
                            tracing::warn!(tool = %name, error = %e, "tool dispatch failed");
                            result_blocks.push(ContentBlock::ToolResult {
                                tool_use_id: id,
                                content: format!("{e}"),
                                is_error: true,
                            });
                        }
                    }
                }
                conv.push(Message {
                    role: Role::User,
                    content: result_blocks,
                });
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
            Some(StopReason::EndTurn | StopReason::MaxTokens | StopReason::StopSequence |
StopReason::Refusal) | None => {
                return Ok(total_usage);
            }
        }
    }
    Err(LoopError::MaxTurns(max_turns))
}

/// Apply a pre-computed tool result to the conversation — used by UIs
/// that want to render each step before making the next request.
pub fn append_tool_result(conv: &mut Conversation, result: ToolResult) {
    conv.push(Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: result.tool_use_id,
            content: result.content,
            is_error: result.is_error,
        }],
    });
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

    #[tokio::test]
    async fn fake_dispatcher_returns_canned() {
        let d = FakeDispatcher;
        let r = d
            .dispatch("read_file", &serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(r, "fake result for read_file");
    }
}
