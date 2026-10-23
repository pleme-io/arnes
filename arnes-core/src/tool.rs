use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One tool exposed to the LLM. Matches Anthropic's tool-definition shape
/// exactly — `name`, `description`, `input_schema` (JSON Schema).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: ToolInputSchema,
}

/// A JSON-Schema-ish input spec. We keep it `Value` instead of typed
/// because the upstream LLM API takes arbitrary schemas — they may embed
/// $refs / enums / conditionals we can't easily strongly-type. Callers
/// build these from `schemars::schema_for!` output or hand-write them.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ToolInputSchema(pub Value);

impl ToolInputSchema {
    #[must_use]
    pub fn empty_object() -> Self {
        Self(serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        }))
    }
}

/// A single tool invocation the assistant requested — the loop takes this,
/// dispatches it against the MCP client or local tool registry, and
/// returns the result.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// The result of executing a [`ToolUse`]. Feeds back into the assistant
/// as a `user` message with a `tool_result` content block.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResult {
    #[must_use]
    pub fn ok(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    #[must_use]
    pub fn err(tool_use_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            content: message.into(),
            is_error: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_object_is_valid_json_schema() {
        let s = ToolInputSchema::empty_object();
        let v: Value = s.0;
        assert_eq!(v["type"], "object");
    }

    #[test]
    fn tool_result_builders() {
        let ok = ToolResult::ok("t1", "done");
        let err = ToolResult::err("t2", "failed");
        assert!(!ok.is_error);
        assert!(err.is_error);
        assert_eq!(err.content, "failed");
    }

    #[test]
    fn tool_round_trips_json() {
        let t = Tool {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: ToolInputSchema(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            })),
        };
        let s = serde_json::to_string(&t).unwrap();
        let back: Tool = serde_json::from_str(&s).unwrap();
        assert_eq!(back.name, "read_file");
    }
}
