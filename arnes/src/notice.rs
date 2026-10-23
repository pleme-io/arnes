//! Where arnes's diagnostics go — one subscriber, three outputs.
//!
//! Nothing writes to the terminal behind the TUI's back: a `tracing` line or
//! a child's stderr painted over the frame corrupts it (seen 2026-09-28: a
//! failover WARN printed across the prompt and scrolled the screen a row).
//!
//! - **The log file** (`$XDG_STATE_HOME/arnes/arnes.log`) gets every event at
//!   the filter's level (`RUST_LOG`, default `info`). It is where detail
//!   lives: request ids, full error bodies, library chatter.
//! - **Toasts** get WARN and ERROR, plus INFO events on the [`TARGET`]
//!   target (arnes's own notes, such as "starting on the local model").
//!   They reach the TUI through a channel and fade there.
//! - **stderr** is used only without a TUI (`--message`), and only for what
//!   would have been a toast, one short line each.

use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender, channel};

use arnes_tui::ToastLevel;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// The target arnes's own operator-facing notes use:
/// `tracing::info!(target: notice::TARGET, "…")`.
pub const TARGET: &str = "arnes::notice";

/// How arnes is being run, which decides whether anything may reach the
/// terminal directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// The TUI owns the terminal: notices become toasts.
    Tui,
    /// A one-shot `--message`: notices go to stderr, one line each.
    Plain,
}

static SINK: OnceLock<Mutex<Sender<(ToastLevel, String)>>> = OnceLock::new();

/// Collects an event's message and its other fields as `k=v`.
#[derive(Default)]
struct Text {
    message: String,
    fields: Vec<String>,
}

impl Visit for Text {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            value.clone_into(&mut self.message);
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields.push(format!("{}={value:?}", field.name()));
        }
    }
}

impl Text {
    fn line(self) -> String {
        if self.fields.is_empty() { self.message } else { format!("{} ({})", self.message, self.fields.join(", ")) }
    }
}

/// Forwards what an operator should see to the toast channel (or stderr).
struct NoticeLayer {
    surface: Surface,
}

fn level_of(meta: &tracing::Metadata<'_>) -> Option<ToastLevel> {
    match *meta.level() {
        Level::ERROR => Some(ToastLevel::Error),
        Level::WARN => Some(ToastLevel::Warn),
        Level::INFO if meta.target() == TARGET => Some(ToastLevel::Info),
        _ => None,
    }
}

impl<S: Subscriber> Layer<S> for NoticeLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let Some(level) = level_of(event.metadata()) else { return };
        let mut text = Text::default();
        event.record(&mut text);
        let line = text.line();
        match self.surface {
            Surface::Plain => eprintln!("arnes: {line}"),
            Surface::Tui => {
                if let Some(tx) = SINK.get().and_then(|m| m.lock().ok()) {
                    let _ = tx.send((level, line));
                }
            }
        }
    }
}

/// Where the log file lives: `$XDG_STATE_HOME/arnes/arnes.log`.
#[must_use]
pub fn log_path() -> Option<std::path::PathBuf> {
    arnes_store::Layout::from_env().map(|l| l.log())
}

/// Where an MCP server's own stderr goes: `…/arnes/mcp/<server>.log`.
#[must_use]
pub fn mcp_log_dir() -> Option<std::path::PathBuf> {
    arnes_store::Layout::from_env().map(|l| l.mcp_log_dir())
}

/// Install the subscriber. For [`Surface::Tui`] the returned receiver
/// delivers toasts; drain it each frame.
pub fn init(surface: Surface) -> Receiver<(ToastLevel, String)> {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};
    let (tx, rx) = channel();
    let _ = SINK.set(Mutex::new(tx));
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let file = log_path().and_then(|p| {
        std::fs::create_dir_all(p.parent()?).ok()?;
        std::fs::OpenOptions::new().create(true).append(true).open(p).ok()
    });
    let file_layer = file.map(|f| fmt::layer().with_ansi(false).with_writer(Mutex::new(f)));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(NoticeLayer { surface })
        .try_init();
    rx
}
