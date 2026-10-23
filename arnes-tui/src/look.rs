//! How arnes looks and moves: the fleet theme chosen by `tema`, the
//! terminal's colour depth, and the clock every animation reads.
//!
//! Colours come from `ishou_tokens::FleetTheme` through egaku-term's
//! `Palette::from_fleet`, so arnes paints what mado, escriba and banken
//! paint. Motion comes from `egaku_term::anim` (the fleet's curves and
//! durations); nothing here keeps its own easing maths.

use std::time::{Duration, Instant};

use egaku_term::Buffer;
use egaku_term::anim::{self, Curve, EasingKind};
use egaku_term::theme::{ColorDepth, Palette};
use ishou_tokens::FleetTheme;

/// Resolve the config's `tema` to a fleet theme. Accepts the fleet's wire
/// names (`pleme_dark`, `vellum`, `polar_veil`, `bare`) and preset names
/// (`nord`); the historical `dark` is the prescribed theme. Anything else
/// is refused alone: the prescribed theme is used and the reason returned
/// so the caller can toast it.
#[must_use]
pub fn fleet_theme(tema: Option<&str>) -> (FleetTheme, Option<String>) {
    let Some(raw) = tema.map(str::trim).filter(|s| !s.is_empty()) else {
        return (FleetTheme::prescribed_default(), None);
    };
    let name = raw.to_ascii_lowercase().replace('-', "_");
    if name == "dark" {
        return (FleetTheme::prescribed_default(), None);
    }
    FleetTheme::all()
        .iter()
        .find(|t| t.name() == name || t.preset_name() == name)
        .map_or_else(
            || {
                let known: Vec<&str> = FleetTheme::all().iter().map(FleetTheme::name).collect();
                (FleetTheme::prescribed_default(), Some(format!("tema {raw:?} is not a fleet theme ({}); using {}", known.join(", "), FleetTheme::prescribed_default().name())))
            },
            |t| (*t, None),
        )
}

/// The palette for `tema` at `depth`.
#[must_use]
pub fn palette(tema: Option<&str>, depth: ColorDepth) -> (Palette, Option<String>) {
    let (theme, warn) = fleet_theme(tema);
    (Palette::from_fleet(theme).for_depth(depth), warn)
}

/// The palette slots a theme can set, by the name a `color` row uses.
pub const PALETTE_SLOTS: [&str; 9] =
    ["background", "foreground", "accent", "error", "warning", "success", "selection", "muted", "border"];

/// `palette` with `(slot, #RRGGBB)` rows applied in order (a theme bidama's
/// `color` exports). A row naming no slot or no colour is refused alone.
#[must_use]
pub fn with_colors(mut palette: Palette, rows: &[(String, String)]) -> (Palette, Vec<String>) {
    let mut refused = Vec::new();
    for (slot, hex) in rows {
        let Some(c) = egaku_term::theme::hex_color(hex) else {
            refused.push(format!("color {slot}: {hex:?} is not #RRGGBB"));
            continue;
        };
        let target = match slot.as_str() {
            "background" => &mut palette.background,
            "foreground" => &mut palette.foreground,
            "accent" => &mut palette.accent,
            "error" => &mut palette.error,
            "warning" => &mut palette.warning,
            "success" => &mut palette.success,
            "selection" => &mut palette.selection,
            "muted" => &mut palette.muted,
            "border" => &mut palette.border,
            other => {
                refused.push(format!("color {other:?}: expected one of {PALETTE_SLOTS:?}"));
                continue;
            }
        };
        *target = c;
    }
    (palette, refused)
}

/// Project every cell's colours onto `depth`. Run on the finished frame,
/// so colours that did not come from the palette (syntax highlighting,
/// markdown) are downsampled too. A no-op at true colour.
pub fn project(buf: &mut Buffer, depth: ColorDepth) {
    if depth == ColorDepth::TrueColor {
        return;
    }
    for y in 0..buf.height() {
        for x in 0..buf.width() {
            if let Some(c) = buf.get_mut(x, y) {
                let st = c.style();
                c.set_fg(depth.project(st.fg));
                c.set_bg(depth.project(st.bg));
            }
        }
    }
}

/// Which overlay is in front. A change of surface restarts its opening
/// animation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Surface {
    #[default]
    Conversation,
    Help,
    Mcp,
    Panel,
    Picker,
    Approval,
    /// Scrolled back through the conversation, off the live tail.
    Scrollback,
}

/// Animation durations, from the fleet tokens.
pub struct Timing;

impl Timing {
    /// A panel or modal opening.
    #[must_use]
    pub fn open() -> Duration {
        anim::duration(|d| d.base_ms)
    }
    /// A toast sliding in.
    #[must_use]
    pub fn toast_in() -> Duration {
        anim::duration(|d| d.fast_ms)
    }
}

/// Eased 0..=1 progress of something that started at `since`.
#[must_use]
pub fn opened(since: Instant, now: Instant, over: Duration) -> f32 {
    anim::progress(now.saturating_duration_since(since), over, Curve::named(EasingKind::Decelerate))
}

#[cfg(test)]
mod tests {
    #[test]
    fn color_rows_set_slots_and_bad_rows_are_refused_alone() {
        let base = egaku_term::theme::Palette::default();
        let rows = vec![
            ("accent".to_owned(), "#00aaff".to_owned()),
            ("sky".to_owned(), "#ffffff".to_owned()),
            ("muted".to_owned(), "blue".to_owned()),
        ];
        let (p, refused) = with_colors(base, &rows);
        assert_eq!(p.accent, egaku_term::theme::hex_color("#00aaff").unwrap());
        assert_eq!(p.muted, base.muted);
        assert_eq!(refused.len(), 2);
        assert_eq!(with_colors(base, &[]).0.accent, base.accent);
    }

    use super::*;
    use egaku_term::crossterm::style::Color;

    #[test]
    fn tema_names_resolve_to_fleet_themes() {
        assert_eq!(fleet_theme(None).0, FleetTheme::prescribed_default());
        assert_eq!(fleet_theme(Some("dark")).0, FleetTheme::prescribed_default());
        assert_eq!(fleet_theme(Some("vellum")), (FleetTheme::Vellum, None));
        assert_eq!(fleet_theme(Some("polar-veil")).0, FleetTheme::PolarVeil);
        assert_eq!(fleet_theme(Some("nord")).0, FleetTheme::PlemeDark);
        let (t, warn) = fleet_theme(Some("solarized"));
        assert_eq!(t, FleetTheme::prescribed_default());
        assert!(warn.unwrap().contains("solarized"));
    }

    #[test]
    fn projection_downsamples_every_cell_at_256() {
        let mut b = Buffer::empty(3, 1);
        b.set_string(0, 0, "abc", egaku_term::Style::default().fg(Color::Rgb { r: 255, g: 0, b: 0 }));
        project(&mut b, ColorDepth::TrueColor);
        assert_eq!(b.get(0, 0).unwrap().style().fg, Color::Rgb { r: 255, g: 0, b: 0 });
        project(&mut b, ColorDepth::Ansi256);
        assert_eq!(b.get(2, 0).unwrap().style().fg, Color::AnsiValue(196));
    }

    #[test]
    fn opening_is_eased_over_the_token_duration() {
        let t0 = Instant::now();
        assert!(opened(t0, t0, Timing::open()) < 1e-6);
        assert!((opened(t0, t0 + Timing::open(), Timing::open()) - 1.0).abs() < 1e-6);
        let spec = LookSpec::default();
        assert!(spec.reveal(1) >= Duration::from_millis(150) && spec.reveal(10_000) <= Duration::from_millis(450));
    }
}

/// The screen's declared knobs (docs/design.md), read from the shikumi
/// config. Every field has the built-in value when unset; a bad value is
/// refused alone, named, and the built-in kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LookSpec {
    pub motion: bool,
    pub notice_rows: u16,
    pub picker_rows: u16,
    pub info: Duration,
    pub warn: Duration,
    pub error: Duration,
    /// One sweep of the thinking shimmer.
    pub shimmer: Duration,
    /// How long a transient indicator (a tool tick, a scroll position)
    /// stays lit before settling.
    pub hold: Duration,
    /// A finished reply unrolls this long per row.
    pub reveal_row: Duration,
}

impl Default for LookSpec {
    fn default() -> Self {
        Self {
            motion: true,
            notice_rows: 3,
            picker_rows: 12,
            info: Duration::from_secs(4),
            warn: Duration::from_secs(8),
            error: Duration::from_secs(12),
            shimmer: Duration::from_millis(1800),
            hold: Duration::from_millis(1200),
            reveal_row: Duration::from_millis(12),
        }
    }
}

impl LookSpec {
    /// Read the screen fields of a config: each `ui` / `activity` key over
    /// its flat twin, each bad value refused alone with the built-in kept.
    #[must_use]
    pub fn from_config(cfg: &arnes_config::ArnesConfig) -> (Self, Vec<String>) {
        use arnes_config::sections::{bounded, one_of};
        let mut s = Self::default();
        let mut refused = Vec::new();
        let motion = cfg.motion_mode();
        let (m, r) = one_of("motion", motion.as_deref(), &["on", "off"], "on");
        s.motion = m == "on";
        refused.extend(r);
        let mut rows = |name: &str, v: Option<u32>, lo: u32, hi: u32, slot: &mut u16| {
            let (v, r) = bounded(name, v, lo, hi, u32::from(*slot));
            *slot = u16::try_from(v).unwrap_or(*slot);
            refused.extend(r);
        };
        rows("notice-rows", cfg.notice_rows_setting(), 0, 10, &mut s.notice_rows);
        rows("picker-rows", cfg.picker_rows_setting(), 3, 40, &mut s.picker_rows);
        let mut ms = |name: &str, v: Option<u32>, lo: u32, hi: u32, per: u64, slot: &mut Duration| {
            let now = u32::try_from(slot.as_millis() / u128::from(per)).unwrap_or(u32::MAX);
            let (v, r) = bounded(name, v, lo, hi, now);
            *slot = Duration::from_millis(u64::from(v) * per);
            refused.extend(r);
        };
        ms("notice-info-secs", cfg.notice_info_setting(), 1, 600, 1000, &mut s.info);
        ms("notice-warn-secs", cfg.notice_warn_setting(), 1, 600, 1000, &mut s.warn);
        ms("notice-error-secs", cfg.notice_error_setting(), 1, 600, 1000, &mut s.error);
        ms("activity.shimmer-ms", cfg.activity.shimmer_ms, 200, 10_000, 1, &mut s.shimmer);
        ms("activity.hold-ms", cfg.activity.hold_ms, 100, 10_000, 1, &mut s.hold);
        ms("ui.timing.reveal-row-ms", cfg.ui.timing.reveal_row_ms, 0, 200, 1, &mut s.reveal_row);
        (s, refused)
    }

    /// A finished reply unrolling `rows` rows, clamped by the fleet's fast
    /// and slow tokens.
    #[must_use]
    pub fn reveal(&self, rows: usize) -> Duration {
        let rows = u32::try_from(rows).unwrap_or(u32::MAX);
        self.reveal_row.saturating_mul(rows).clamp(anim::duration(|d| d.fast_ms), anim::duration(|d| d.slow_ms))
    }

    /// How long a notice of `level` stays in the lane.
    #[must_use]
    pub const fn lifetime(&self, level: crate::state::ToastLevel) -> Duration {
        match level {
            crate::state::ToastLevel::Info => self.info,
            crate::state::ToastLevel::Warn => self.warn,
            crate::state::ToastLevel::Error => self.error,
        }
    }
}

/// A micro-interaction: a state change acknowledged for the fleet's fast
/// duration (150 ms). `flash` is 1 at the change and eases to 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Blip {
    /// Editor mode (INSERT / NORMAL / command line).
    Mode,
    Permission,
    Effort,
    /// Model or route.
    Model,
    /// The status hint (copied, attached, reloaded…).
    Hint,
    /// A prompt was sent.
    Submit,
    /// A picker's selection moved.
    Pick,
    /// A list scrolled.
    Scroll,
    /// A tool finished (the activity tick).
    Tick,
    /// An approval was answered.
    Verdict,
}

/// When each [`Blip`] last fired.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Blips(std::collections::HashMap<Blip, Instant>);

impl Blips {
    pub fn fire(&mut self, b: Blip, now: Instant) {
        self.0.insert(b, now);
    }

    #[must_use]
    pub fn at(&self, b: Blip) -> Option<Instant> {
        self.0.get(&b).copied()
    }

    /// 1.0 at the change, easing to 0 over `fast`; 0 with motion off or
    /// when it never fired.
    #[must_use]
    pub fn flash(&self, b: Blip, now: Instant, motion: bool) -> f32 {
        if !motion {
            return 0.0;
        }
        self.at(b).map_or(0.0, |t| 1.0 - opened(t, now, Timing::fast()))
    }

    /// Whether any blip is still easing at `now`, for indicators that stay
    /// lit `hold`.
    #[must_use]
    pub fn live(&self, now: Instant, hold: Duration) -> bool {
        self.0.values().any(|t| now.saturating_duration_since(*t) < hold)
    }
}

impl Timing {
    /// The micro-interaction duration.
    #[must_use]
    pub fn fast() -> Duration {
        anim::duration(|d| d.fast_ms)
    }
}

#[cfg(test)]
mod spec_tests {
    use super::*;

    #[test]
    fn spec_reads_config_and_refuses_bad_values_alone() {
        let cfg = arnes_config::ArnesConfig {
            motion: Some("off".into()),
            notice_rows: Some(99),
            notice_warn_secs: Some(20),
            picker_rows: Some(8),
            ..arnes_config::ArnesConfig::default()
        };
        let (s, refused) = LookSpec::from_config(&cfg);
        assert!(!s.motion);
        assert_eq!(s.notice_rows, 3, "refused, the built-in kept");
        assert_eq!((s.picker_rows, s.warn), (8, Duration::from_secs(20)));
        assert_eq!(refused.len(), 1);
        assert!(refused[0].contains("notice-rows 99"));
    }

    #[test]
    fn a_blip_flashes_for_the_fast_duration() {
        let t0 = Instant::now();
        let mut b = Blips::default();
        assert!(b.flash(Blip::Mode, t0, true).abs() < 1e-6, "never fired");
        b.fire(Blip::Mode, t0);
        assert!((b.flash(Blip::Mode, t0, true) - 1.0).abs() < 1e-6);
        assert!(b.flash(Blip::Mode, t0 + Duration::from_millis(75), true) > 0.0);
        assert!(b.flash(Blip::Mode, t0 + Duration::from_millis(150), true).abs() < 1e-6);
        assert!(b.flash(Blip::Mode, t0, false).abs() < 1e-6, "reduced motion");
        let hold = LookSpec::default().hold;
        assert!(b.live(t0, hold) && !b.live(t0 + Duration::from_secs(2), hold));
    }
}
