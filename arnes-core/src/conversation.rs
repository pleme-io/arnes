use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::message::Message;

/// Who sent a message — matches the Anthropic API roles exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    /// System-role messages in arnes are lifted into the top-level
    /// `Conversation::system` string — the Anthropic API takes them
    /// separately. The enum variant exists for completeness.
    System,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ConversationId(pub u64);

impl std::fmt::Display for ConversationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "conv#{}", self.0)
    }
}

/// A full conversation — system prompt + message history.
///
/// Anthropic's Messages API format: `system: String` is top-level,
/// `messages: Vec<Message>` alternates user/assistant.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Conversation {
    pub id: ConversationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

impl Conversation {
    #[must_use]
    pub fn new(id: ConversationId, model: impl Into<String>) -> Self {
        Self {
            id,
            system: None,
            messages: Vec::new(),
            model: model.into(),
            max_tokens: Some(crate::DEFAULT_MAX_TOKENS),
        }
    }

    pub fn push(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    #[must_use]
    pub fn with_system(mut self, prompt: impl Into<String>) -> Self {
        self.system = Some(prompt.into());
        self
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Remove every thinking and redacted-thinking block, and any message
    /// left empty by that. Returns how many blocks went. A thinking block is
    /// signed by the provider that wrote it, so when the conversation moves
    /// to a route that echoes history back, another provider's blocks are
    /// a refused request; the text and tool calls around them stay.
    pub fn drop_thinking(&mut self) -> usize {
        use crate::message::ContentBlock;
        let mut dropped = 0;
        for m in &mut self.messages {
            let before = m.content.len();
            m.content.retain(|b| !matches!(b, ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. }));
            dropped += before - m.content.len();
        }
        self.messages.retain(|m| !m.content.is_empty());
        dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ContentBlock, Message};

    #[test]
    fn role_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"user\"");
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            "\"assistant\""
        );
    }

    #[test]
    fn drop_thinking_keeps_the_text_and_tool_calls() {
        let mut c = Conversation::new(ConversationId(1), "m");
        c.push(Message::user_text("q"));
        c.push(Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking { thinking: "t".into(), signature: Some("sig".into()) },
                ContentBlock::Text { text: "a".into() },
            ],
        });
        c.push(Message { role: Role::Assistant, content: vec![ContentBlock::RedactedThinking { data: "x".into() }] });
        assert_eq!(c.drop_thinking(), 2);
        assert_eq!(c.len(), 2, "a message left empty goes");
        assert_eq!(c.messages[1].text(), "a");
        assert_eq!(c.drop_thinking(), 0);
    }

    #[test]
    fn conversation_builds() {
        let mut c =
            Conversation::new(ConversationId(1), "claude-sonnet-4-6").with_system("You are arnes.");
        c.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "hi".into() }],
        });
        assert_eq!(c.len(), 1);
        assert_eq!(c.system.as_deref(), Some("You are arnes."));
    }
}
