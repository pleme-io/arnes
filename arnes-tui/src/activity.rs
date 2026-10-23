//! What arnes is doing right now, for the activity line: the running
//! tools, the last one to finish, and a short note (a fallback route, an
//! approval answer). The event loop feeds it; the renderer only reads.

use std::time::{Duration, Instant};

/// Something the turn reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    ToolStart(String),
    ToolEnd { name: String, ok: bool },
}

/// A finished tool, shown briefly as a tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tick {
    pub name: String,
    pub ok: bool,
    pub took: Duration,
    pub at: Instant,
}

/// The activity line's model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    /// Tools running now, oldest first, with their start.
    pub running: Vec<(String, Instant)>,
    /// The last tool to finish.
    pub tick: Option<Tick>,
    /// A one-line note and when it arrived (fallback taken, approval
    /// answered). Shown for twice [`Self::hold`] then gone.
    pub note: Option<(String, bool, Instant)>,
    /// Which route answers, said while idle (`local llama.cpp · free`).
    pub route: String,
    /// How long a tick stays (a note stays twice this); the look spec's.
    pub hold: Duration,
}

impl Default for Activity {
    fn default() -> Self {
        Self { running: Vec::new(), tick: None, note: None, route: String::new(), hold: crate::look::LookSpec::default().hold }
    }
}

impl Activity {
    pub fn apply(&mut self, ev: Event, now: Instant) {
        match ev {
            Event::ToolStart(name) => self.running.push((name, now)),
            Event::ToolEnd { name, ok } => {
                let started = self.running.iter().position(|(n, _)| *n == name).map(|i| self.running.remove(i).1);
                let took = started.map_or(Duration::ZERO, |s| now.saturating_duration_since(s));
                self.tick = Some(Tick { name, ok, took, at: now });
            }
        }
    }

    /// Say something for a moment. `good` picks the success tone, else the
    /// warning tone.
    pub fn note(&mut self, text: impl Into<String>, good: bool, now: Instant) {
        self.note = Some((text.into(), good, now));
    }

    /// A turn ended: nothing is running any more.
    pub fn settle(&mut self) {
        self.running.clear();
    }

    /// The note, while it is still current.
    #[must_use]
    pub fn current_note(&self, now: Instant) -> Option<(&str, bool, Instant)> {
        self.note.as_ref().filter(|(_, _, t)| now.saturating_duration_since(*t) < self.hold * 2).map(|(s, g, t)| (s.as_str(), *g, *t))
    }

    /// The tick, while it is still current.
    #[must_use]
    pub fn current_tick(&self, now: Instant) -> Option<&Tick> {
        self.tick.as_ref().filter(|t| now.saturating_duration_since(t.at) < self.hold)
    }
}

/// `mcp__postgres__query` → (`query`, Some(`postgres`)).
#[must_use]
pub fn tool_label(name: &str) -> (&str, Option<&str>) {
    let mut parts = name.splitn(3, "__");
    match (parts.next(), parts.next(), parts.next()) {
        (Some("mcp"), Some(server), Some(tool)) => (tool, Some(server)),
        _ => (name, None),
    }
}

/// `1.2s`, `42s`, `3m05s`.
#[must_use]
pub fn clock(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else if s >= 10 {
        format!("{s}s")
    } else {
        format!("{}.{}s", s, d.subsec_millis() / 100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_run_then_tick_with_their_time() {
        let t0 = Instant::now();
        let mut a = Activity::default();
        a.apply(Event::ToolStart("Read".into()), t0);
        a.apply(Event::ToolStart("mcp__pg__query".into()), t0);
        assert_eq!(a.running.len(), 2);
        a.apply(Event::ToolEnd { name: "Read".into(), ok: true }, t0 + Duration::from_millis(300));
        assert_eq!(a.running.len(), 1);
        let t = a.current_tick(t0 + Duration::from_millis(400)).unwrap();
        assert_eq!((t.name.as_str(), t.ok, t.took), ("Read", true, Duration::from_millis(300)));
        assert!(a.current_tick(t0 + Duration::from_secs(5)).is_none());
        a.settle();
        assert!(a.running.is_empty());
    }

    #[test]
    fn labels_and_clock() {
        assert_eq!(tool_label("mcp__postgres__query"), ("query", Some("postgres")));
        assert_eq!(tool_label("Bash"), ("Bash", None));
        assert_eq!(clock(Duration::from_millis(1250)), "1.2s");
        assert_eq!(clock(Duration::from_secs(42)), "42s");
        assert_eq!(clock(Duration::from_secs(185)), "3m05s");
    }
}
