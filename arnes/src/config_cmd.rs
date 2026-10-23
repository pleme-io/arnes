//! `arnes config …`: the configuration surface from the terminal
//! (docs/configuration.md). `show` prints the effective config and where
//! each value came from, `schema` the JSON schema, `validate` checks a file
//! the way the running TUI would load it, and `tier` prints a shikumi tier.

use std::path::{Path, PathBuf};

use arnes_config::render::{Format, render};
use arnes_config::salvage::{is_set, leaves};
use arnes_config::{ArnesConfig, ENV_PREFIX, TIER_ENV};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// The effective config: defaults, then `ARNES_*`, then the file.
    Show {
        /// lisp | yaml | nix | json.
        #[arg(long, default_value = "lisp")]
        format: String,
        /// One line per value with where it came from (default, env, file).
        #[arg(long)]
        sources: bool,
    },
    /// The JSON schema of every key.
    Schema,
    /// Load a file as the TUI would: every refused key, entry and unknown key
    /// is listed, and the exit status is 1 when there is any.
    Validate {
        /// The file (lisp, yaml, nix or toml). Unset: the discovered one.
        file: Option<PathBuf>,
    },
    /// A shikumi tier: `bare` (no opinion), `discovered`, `default`.
    Tier {
        tier: String,
        #[arg(long, default_value = "lisp")]
        format: String,
    },
}

/// Every value that loaded but is refused where it is read (out of bounds,
/// not one of its words, an unknown action or theme), exactly as the TUI
/// would say it, one line each.
#[must_use]
pub fn typed_refusals(cfg: &ArnesConfig) -> Vec<String> {
    let mut out = cfg.knobs().1;
    out.extend(arnes_tui::look::LookSpec::from_config(cfg).1);
    out.extend(arnes_tui::look::fleet_theme(cfg.theme().as_deref()).1);
    out.extend(arnes_tui::nav::keymaps(cfg).2);
    out.extend(arnes_tui::Keymap::from_config(cfg).1);
    let mode = cfg.editor_mode_name();
    out.extend(arnes_config::sections::one_of("editor-mode", mode.as_deref(), &["normal", "vim"], "normal").1);
    for e in [
        cfg.provider().err(),
        cfg.request_params().err(),
        cfg.permission_policy().err(),
        cfg.mcp_exposure().err(),
    ]
    .into_iter()
    .flatten()
    {
        out.push(e);
    }
    out.extend(cfg.fallback_routes().1);
    out
}

fn discovered(explicit: Option<&Path>) -> Option<PathBuf> {
    explicit.map(Path::to_path_buf).or_else(ArnesConfig::discover_path)
}

/// Run one `arnes config` command; the exit status is the return value
/// (0 ok, 1 something refused, 2 the file does not parse).
pub fn run(cmd: ConfigCmd, config: Option<&Path>) -> anyhow::Result<u8> {
    match cmd {
        ConfigCmd::Schema => {
            println!("{}", serde_json::to_string_pretty(&ArnesConfig::schema_value())?);
            Ok(0)
        }
        ConfigCmd::Tier { tier, format } => {
            use shikumi::TieredConfig;
            let t = match tier.as_str() {
                "bare" => shikumi::ConfigTier::Bare,
                "discovered" => shikumi::ConfigTier::Discovered,
                "default" => shikumi::ConfigTier::Default,
                other => anyhow::bail!("unknown tier {other:?}; expected bare, discovered or default"),
            };
            let v = serde_json::to_value(ArnesConfig::resolve_tier(t))?;
            print!("{}", render(&v, Format::parse(&format).map_err(anyhow::Error::msg)?));
            Ok(0)
        }
        ConfigCmd::Validate { file } => {
            let Some(path) = file.or_else(|| discovered(config)) else {
                anyhow::bail!("no config file found (set ARNES_CONFIG, or pass a path)");
            };
            match ArnesConfig::load_report(&path) {
                Err(e) => {
                    println!("{}: does not parse: {e}", path.display());
                    Ok(2)
                }
                Ok(l) => {
                    let mut refused = l.refused;
                    refused.extend(typed_refusals(&l.config));
                    for r in &refused {
                        println!("{}: {r}", path.display());
                    }
                    if refused.is_empty() {
                        println!("{}: ok", path.display());
                        Ok(0)
                    } else {
                        println!("{} refused; every other key loads", refused.len());
                        Ok(1)
                    }
                }
            }
        }
        ConfigCmd::Show { format, sources } => {
            let format = Format::parse(&format).map_err(anyhow::Error::msg)?;
            let (cfg, file, env, refused, path) = match discovered(config) {
                Some(path) => {
                    let l = ArnesConfig::load_report(&path).map_err(anyhow::Error::msg)?;
                    (l.config, l.file, l.env, l.refused, Some(path))
                }
                None => (ArnesConfig::base_tier(), serde_json::Value::Null, serde_json::Value::Null, Vec::new(), None),
            };
            let v = serde_json::to_value(&cfg)?;
            if sources {
                let tier = std::env::var(TIER_ENV).unwrap_or_else(|_| "default".into());
                let file_name = path.as_ref().map_or_else(|| "no file".to_owned(), |p| p.display().to_string());
                println!("# tier {tier}; env {ENV_PREFIX}*; file {file_name}");
                for (key, value) in leaves(&v) {
                    let from = if refused.iter().any(|r| r.starts_with(&format!("{key}:")) || r.starts_with(&format!("{key}["))) {
                        "default (the file's value was refused)"
                    } else if is_set(&file, &key) {
                        "file"
                    } else if is_set(&env, &key) {
                        "env"
                    } else {
                        "default"
                    };
                    println!("{key} = {value}  # {from}");
                }
            } else {
                print!("{}", render(&v, format));
            }
            for r in refused.iter().chain(&typed_refusals(&cfg)) {
                eprintln!("refused: {r}");
            }
            Ok(0)
        }
    }
}
