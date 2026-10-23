//! `arnes-tui` — terminal UI for the arnes agent, rendered through
//! [`egaku-term`](https://github.com/pleme-io/egaku-term).
//!
//! Layout:
//!
//! ```text
//! ┌─────────────────────────────────────────────┐
//! │  ConversationPane                           │
//! │  ──                                         │
//! │  user: …                                    │
//! │  arnes: …                                   │
//! │    [tool read_file ✓]                       │
//! │  arnes: …                                   │
//! │                                             │
//! ├─────────────────────────────────────────────┤
//! │  StatusLine  model · tokens · mcp · state  │
//! ├─────────────────────────────────────────────┤
//! │  Input: >                                   │
//! └─────────────────────────────────────────────┘
//! ```
//!
//! The renderer is decoupled from the event loop: callers own an
//! [`egaku_term::Terminal`] plus a back [`egaku_term::Buffer`], reset the
//! buffer and call [`draw_frame`] into it each frame, then diff + flush the
//! changed cells (`egaku_term::render_diff`). Pair with your own tokio loop
//! for full async TUIs.

extern crate self as arnes_tui;

pub mod activity;
pub mod chrome;
pub mod error;
pub mod input;
pub mod keymap;
pub mod layout;
pub mod look;
pub mod mcp_browser;
pub mod nav;
pub mod render;
pub mod state;
pub mod vim;

pub use error::{Error, Result};
pub use render::{draw_frame, draw_frame_at};
pub use input::{COMMANDS, Effect, InputState, PickerKind};
pub use keymap::{Action, Keymap, Resolution};
pub use mcp_browser::{McpBrowser, ServerEntry, ServerStatus, ToolEntry};
pub use state::{ApprovalView, MAX_TOASTS, Overlay, Pane, Toast, ToastLevel, TuiState, UsageView};
