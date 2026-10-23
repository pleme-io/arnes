//! A config value written back out in each authoring format shikumi reads:
//! `(defarnes …)` lisp, YAML and a Nix attrset. `arnes config show` prints
//! through these, and the round-trip tests load what they write through the
//! product path, so a key the renderers and the loader spell differently is
//! a red test, never a setting silently dropped.

use std::fmt::Write as _;

use serde_json::Value;

/// Output formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Lisp,
    Yaml,
    Nix,
    Json,
}

impl Format {
    pub const ALL: [Self; 4] = [Self::Lisp, Self::Yaml, Self::Nix, Self::Json];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Lisp => "lisp",
            Self::Yaml => "yaml",
            Self::Nix => "nix",
            Self::Json => "json",
        }
    }

    /// # Errors
    /// Names the accepted formats.
    pub fn parse(s: &str) -> Result<Self, String> {
        Self::ALL.into_iter().find(|f| f.name() == s).ok_or_else(|| {
            format!("unknown format {s:?}; expected one of {:?}", Self::ALL.map(Self::name))
        })
    }
}

/// `v` (an object) in `format`.
#[must_use]
pub fn render(v: &Value, format: Format) -> String {
    match format {
        Format::Lisp => format!("(defarnes{})\n", lisp_pairs(v, 1)),
        Format::Yaml => serde_yaml::to_string(v).unwrap_or_default(),
        Format::Nix => format!("{}\n", nix(v, 0)),
        Format::Json => format!("{}\n", serde_json::to_string_pretty(v).unwrap_or_default()),
    }
}

fn kebab(k: &str) -> String {
    k.replace('_', "-")
}

fn lisp_pairs(v: &Value, depth: usize) -> String {
    let Value::Object(m) = v else { return String::new() };
    let pad = "  ".repeat(depth);
    // An empty map has no lisp spelling (`()` reads as an empty list), and
    // omitted it is the same default.
    let mut out = String::new();
    for (k, x) in m.iter().filter(|(_, x)| !matches!(x, Value::Object(o) if o.is_empty())) {
        let _ = write!(out, "\n{pad}:{} {}", kebab(k), lisp(x, depth + 1));
    }
    out
}

fn lisp(v: &Value, depth: usize) -> String {
    match v {
        Value::Null => "nil".into(),
        Value::Bool(b) => if *b { "#t" } else { "#f" }.into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => serde_json::to_string(s).unwrap_or_default(),
        Value::Array(items) => format!("({})", items.iter().map(|i| lisp(i, depth)).collect::<Vec<_>>().join(" ")),
        Value::Object(m) if m.is_empty() => "()".into(),
        Value::Object(_) => format!("({})", lisp_pairs(v, depth).trim_start()),
    }
}

fn nix_key(k: &str) -> String {
    if k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') && !k.starts_with(|c: char| c.is_ascii_digit()) {
        k.to_owned()
    } else {
        serde_json::to_string(k).unwrap_or_default()
    }
}

fn nix(v: &Value, depth: usize) -> String {
    let pad = "  ".repeat(depth + 1);
    let end = "  ".repeat(depth);
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        // A JSON string is a Nix string once `${` cannot interpolate.
        Value::String(s) => serde_json::to_string(s).unwrap_or_default().replace("${", "\\${"),
        Value::Array(items) => format!("[ {} ]", items.iter().map(|i| nix(i, depth + 1)).collect::<Vec<_>>().join(" ")),
        Value::Object(m) if m.is_empty() => "{ }".into(),
        Value::Object(m) => {
            let mut body = String::new();
            for (k, x) in m {
                let _ = write!(body, "\n{pad}{} = {};", nix_key(k), nix(x, depth + 1));
            }
            format!("{{{body}\n{end}}}")
        }
    }
}
