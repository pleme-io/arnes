use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::conversation::Role;

/// One message — role + typed content blocks.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    #[must_use]
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    #[must_use]
    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    /// Concatenate every `Text` block's text — useful for quick display.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = String::new();
        for block in &self.content {
            if let ContentBlock::Text { text } = block {
                out.push_str(text);
            }
        }
        out
    }
}

/// Typed content block — matches Anthropic's content-block shapes 1:1.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text.
    Text { text: String },
    /// The assistant requests a tool call. Fields match the API wire shape.
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// Reasoning the model emitted before answering — `deepseek-reasoner`,
    /// or Anthropic extended thinking. Whether it is echoed back in later
    /// requests is a provider fact (`RequestParams::echo_thinking`).
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// Reasoning the provider returned encrypted.
    RedactedThinking { data: String },
    /// The user returns a tool result to the assistant.
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

/// Why the model stopped generating — drives the loop decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Natural end of turn — assistant finished speaking.
    EndTurn,
    /// Assistant wants a tool call; the loop must dispatch + continue.
    ToolUse,
    /// Hit `max_tokens`.
    MaxTokens,
    /// Hit a stop sequence.
    StopSequence,
    /// Refusal / content policy.
    Refusal,
    /// Server paused a long turn; resend to let it continue.
    PauseTurn,
    /// The prompt no longer fits the model's context window.
    ModelContextWindowExceeded,
    /// A stop reason this build does not know. Parsing it must never fail
    /// the whole response — a new provider value is data, not a crash.
    #[serde(other)]
    Unknown,
}

impl StopReason {
    #[must_use]
    pub const fn requires_continuation(self) -> bool {
        matches!(self, Self::ToolUse | Self::PauseTurn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_helper_concatenates_blocks() {
        let m = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "Hello ".into(),
                },
                ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({ "path": "/tmp/x" }),
                },
                ContentBlock::Text {
                    text: "world.".into(),
                },
            ],
        };
        assert_eq!(m.text(), "Hello world.");
    }

    #[test]
    fn tool_use_serializes_with_type_tag() {
        let b = ContentBlock::ToolUse {
            id: "t1".into(),
            name: "read_file".into(),
            input: serde_json::json!({ "path": "/tmp/x" }),
        };
        let s = serde_json::to_string(&b).unwrap();
        assert!(s.contains(r#""type":"tool_use""#));
        assert!(s.contains(r#""name":"read_file""#));
    }

    #[test]
    fn stop_reason_drives_loop() {
        assert!(StopReason::ToolUse.requires_continuation());
        assert!(!StopReason::EndTurn.requires_continuation());
        assert!(!StopReason::MaxTokens.requires_continuation());
    }

    #[test]
    fn thinking_blocks_parse_from_the_wire() {
        let b: ContentBlock =
            serde_json::from_str(r#"{"type":"thinking","thinking":"hmm","signature":"s"}"#).unwrap();
        assert!(matches!(b, ContentBlock::Thinking { ref thinking, .. } if thinking == "hmm"));
        let r: ContentBlock =
            serde_json::from_str(r#"{"type":"redacted_thinking","data":"x"}"#).unwrap();
        assert!(matches!(r, ContentBlock::RedactedThinking { .. }));
    }

    #[test]
    fn unknown_stop_reason_parses_instead_of_failing() {
        let r: StopReason = serde_json::from_str(r#""some_future_reason""#).unwrap();
        assert_eq!(r, StopReason::Unknown);
        let w: StopReason = serde_json::from_str(r#""model_context_window_exceeded""#).unwrap();
        assert_eq!(w, StopReason::ModelContextWindowExceeded);
        assert!(StopReason::PauseTurn.requires_continuation());
    }
}
