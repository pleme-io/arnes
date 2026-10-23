//! `arnes-core` — foundational types for the arnes agent harness.
//!
//! **arnes** (Portuguese: *harness*) is a Rust/tatara-lisp open-source
//! agent runner — the kind of thing Claude Code / Cursor / `OpenCode` are,
//! but native to the pleme-io stack: shikumi config, ratatui UI, MCP via
//! anvil, deep integration with escriba (the editor).
//!
//! This crate has **no I/O** — pure types only. It's the vocabulary
//! everything else speaks: Messages, Conversations, Tools, `StopReasons`,
//! Usage. Every type derives `serde::{Serialize, Deserialize}` +
//! `schemars::JsonSchema` so the API surface is spec-first.

extern crate self as arnes_core;

pub mod conversation;
pub mod error;
pub mod message;
pub mod thinking;
pub mod tool;
pub mod usage;

pub use conversation::{Conversation, ConversationId, Role};
pub use error::{ArnesError, Result};
pub use message::{ContentBlock, Message, StopReason};
pub use thinking::{parse_named, EchoRule, Effort, ThinkingMode};
pub use tool::{Tool, ToolInputSchema, ToolResult, ToolUse};
pub use usage::Usage;

/// The one `max_tokens` default — config, conversation and client all read
/// it, so the configured value and the sent value cannot disagree.
pub const DEFAULT_MAX_TOKENS: u32 = 8192;
