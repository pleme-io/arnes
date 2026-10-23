//! Reasoning controls — the typed vocabulary config and client share.

/// Reasoning effort. `DeepSeek` honours all three; the wire value is the
/// lowercase name (`output_config.effort`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effort {
    Low,
    High,
    Max,
}

impl Effort {
    pub const ALL: [Self; 3] = [Self::Low, Self::High, Self::Max];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    /// # Errors
    /// Names every accepted level when `s` is not one of them.
    pub fn parse(s: &str) -> Result<Self, String> {
        parse_named(s, &Self::ALL, Self::name, "effort")
    }
}

/// Whether and how the model reasons before answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThinkingMode {
    /// Send nothing — the provider's default (`DeepSeek`: on, effort high).
    #[default]
    ProviderDefault,
    /// `thinking: {type: disabled}` — cheapest; no reasoning tokens.
    Off,
    /// `thinking: {type: enabled}` plus optional effort and budget. The
    /// budget is emitted only for providers that honour it.
    On { effort: Option<Effort>, budget: Option<u32> },
}

/// When prior thinking blocks are re-sent in history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EchoRule {
    Never,
    Always,
    /// Only when the request carries tools — `DeepSeek`'s rule: without
    /// tools reasoning is ignored, with tools omitting it is a 400.
    WhenTools,
}

impl EchoRule {
    pub const ALL: [Self; 3] = [Self::Never, Self::Always, Self::WhenTools];

    #[must_use]
    pub const fn echoes(self, has_tools: bool) -> bool {
        match self {
            Self::Never => false,
            Self::Always => true,
            Self::WhenTools => has_tools,
        }
    }
}

/// Parse `s` against a closed set, naming the legal values on refusal.
///
/// # Errors
/// When `s` names none of `all`.
pub fn parse_named<T: Copy>(
    s: &str,
    all: &[T],
    name: fn(T) -> &'static str,
    what: &str,
) -> Result<T, String> {
    all.iter().copied().find(|v| name(*v) == s).ok_or_else(|| {
        let legal: Vec<_> = all.iter().map(|v| name(*v)).collect();
        format!("unknown {what} {s:?}; expected one of {legal:?}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_rule_table_covers_every_variant() {
        for rule in EchoRule::ALL {
            let (without, with) = (rule.echoes(false), rule.echoes(true));
            let expected = match rule {
                EchoRule::Never => (false, false),
                EchoRule::Always => (true, true),
                EchoRule::WhenTools => (false, true),
            };
            assert_eq!((without, with), expected, "{rule:?}");
        }
    }

    #[test]
    fn effort_round_trips_and_refuses_by_name() {
        for e in Effort::ALL {
            assert_eq!(Effort::parse(e.name()), Ok(e));
        }
        assert!(Effort::parse("medium").unwrap_err().contains("low"));
    }
}
