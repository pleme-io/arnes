//! Skills (docs/profiles.md §3): Claude-Code-style directories holding a
//! `SKILL.md` whose YAML frontmatter names and describes the skill (parsed
//! with `skill-lint`, the fleet's parser) and whose markdown body is loaded
//! on demand. The names and descriptions are listed to the model; the
//! `Skill` tool returns a body.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use arnes_core::{Tool, ToolInputSchema};
use serde_json::{Value, json};

/// The tool the model loads a skill with.
pub const TOOL: &str = "Skill";

/// A description listed to the model is cut to this many characters.
const MAX_DESCRIPTION: usize = 300;

/// One skill.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

/// The skills a session can load, sorted by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Skills {
    skills: Vec<Skill>,
}

fn globs(patterns: &[String], refused: &mut Vec<String>) -> globset::GlobSet {
    let mut b = globset::GlobSetBuilder::new();
    for p in patterns {
        match globset::Glob::new(p) {
            Ok(g) => {
                b.add(g);
            }
            Err(e) => refused.push(format!("skill glob {p:?}: {e}")),
        }
    }
    b.build().unwrap_or_else(|_| globset::GlobSet::empty())
}

/// The body of a SKILL.md: what follows the frontmatter.
fn body(text: &str) -> &str {
    let t = text.trim_start();
    let Some(rest) = t.strip_prefix("---") else { return text };
    rest.find("\n---").map_or(text, |end| rest[end + 4..].trim_start_matches(['-', '\r', '\n']))
}

impl Skills {
    /// Every `<dir>/<skill>/SKILL.md` under `dirs`, in order, the first of a
    /// name winning; `only` (empty = all) and `off` select by name; at most
    /// `max` are kept. A skill whose frontmatter does not parse, or has no
    /// name or description, is skipped and named; a missing directory is not
    /// an error.
    #[must_use]
    pub fn discover(dirs: &[PathBuf], only: &[String], off: &[String], max: usize) -> (Self, Vec<String>) {
        let mut refused = Vec::new();
        let only_set = globs(only, &mut refused);
        let off_set = globs(off, &mut refused);
        let mut skills: Vec<Skill> = Vec::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir) else { continue };
            let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path().join("SKILL.md"))).filter(|p| p.is_file()).collect();
            paths.sort();
            for path in paths {
                let Ok(text) = std::fs::read_to_string(&path) else { continue };
                let fm = match skill_lint::SkillFrontmatter::parse(&text) {
                    Ok(fm) => fm,
                    Err(e) => {
                        refused.push(format!("skill {}: {e}; skipped", path.display()));
                        continue;
                    }
                };
                let (Some(name), Some(description)) = (fm.name, fm.description) else {
                    refused.push(format!("skill {}: needs a name and a description; skipped", path.display()));
                    continue;
                };
                let listed = (only.is_empty() || only_set.is_match(&name)) && !off_set.is_match(&name);
                if listed && !skills.iter().any(|s| s.name == name) {
                    skills.push(Skill { name, description: description.split_whitespace().collect::<Vec<_>>().join(" "), path });
                }
            }
        }
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        skills.truncate(max);
        (Self { skills }, refused)
    }

    /// Keep only the skills named in `names` (the `skills` hook's answer).
    pub fn keep(&mut self, names: &[&str]) {
        self.skills.retain(|s| names.contains(&s.name.as_str()));
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.skills.iter().map(|s| s.name.as_str()).collect()
    }

    /// The skills as JSON, `[{name, description, path}]`, for the hook.
    #[must_use]
    pub fn json(&self) -> String {
        serde_json::to_string(&self.skills).unwrap_or_else(|_| "[]".into())
    }

    /// The system prompt's block naming each skill, or `None` when there are
    /// none. Byte-stable for a given set, so the prompt prefix caches.
    #[must_use]
    pub fn listing(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }
        let mut out = format!("Skills: call the {TOOL} tool with a name to load its instructions before a task that matches.\n");
        for s in &self.skills {
            let mut d: String = s.description.chars().take(MAX_DESCRIPTION).collect();
            if d.len() < s.description.len() {
                d.push('…');
            }
            let _ = writeln!(out, "- {}: {d}", s.name);
        }
        Some(out)
    }

    /// The `Skill` tool's definition.
    #[must_use]
    pub fn tool() -> Tool {
        Tool {
            name: TOOL.into(),
            description: "Load a skill's full instructions by name (the skills are listed in the system prompt).".into(),
            input_schema: ToolInputSchema(json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            })),
        }
    }

    /// A skill's body, for the `Skill` tool.
    ///
    /// # Errors
    /// No skill of that name (the error lists the names), or its file cannot
    /// be read.
    pub fn call(&self, input: &Value) -> crate::Result<String> {
        let name = input.get("name").and_then(Value::as_str).unwrap_or_default();
        let Some(s) = self.skills.iter().find(|s| s.name == name) else {
            return Err(crate::ToolError::Invalid(format!("no skill {name:?}; skills: {}", self.names().join(", "))));
        };
        let text = std::fs::read_to_string(&s.path)?;
        Ok(body(&text).to_owned())
    }
}

/// The default skill directories: `~/.claude/skills`, then `./.claude/skills`.
#[must_use]
pub fn default_dirs(home: Option<&Path>, cwd: &Path) -> Vec<PathBuf> {
    home.map(|h| h.join(".claude/skills")).into_iter().chain(std::iter::once(cwd.join(".claude/skills"))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(dir: &Path, name: &str, text: &str) {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        std::fs::write(dir.join(name).join("SKILL.md"), text).unwrap();
    }

    fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
        let user = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        skill(user.path(), "arnes", "---\nname: arnes\ndescription: >-\n  Develop arnes\n  the house way.\n---\n\n# arnes\n\nBlue first.\n");
        skill(user.path(), "bidama", "---\nname: bidama\ndescription: Blue packages.\n---\nFind first.\n");
        skill(user.path(), "broken", "no frontmatter here");
        skill(user.path(), "nameless", "---\ndescription: who am I\n---\nbody");
        skill(project.path(), "arnes", "---\nname: arnes\ndescription: the project's copy\n---\nshadowed\n");
        skill(project.path(), "kit", "---\nname: kit\ndescription: Go tools.\n---\nkeyway body\n");
        (user, project)
    }

    #[test]
    fn discovery_parses_frontmatter_first_name_wins_and_refuses_bad_files_alone() {
        let (user, project) = fixture();
        let dirs = [user.path().to_path_buf(), project.path().to_path_buf(), PathBuf::from("/nonexistent")];
        let (s, refused) = Skills::discover(&dirs, &[], &[], 200);
        assert_eq!(s.names(), ["arnes", "bidama", "kit"]);
        assert_eq!(refused.len(), 2, "{refused:?}");
        let listing = s.listing().unwrap();
        assert!(listing.contains("- arnes: Develop arnes the house way."), "{listing}");
        assert_eq!(s.call(&json!({"name": "arnes"})).unwrap(), "# arnes\n\nBlue first.\n", "the user's copy, body only");
        assert!(s.call(&json!({"name": "nope"})).unwrap_err().to_string().contains("bidama"));
        let (none, _) = Skills::discover(&[], &[], &[], 200);
        assert!(none.is_empty() && none.listing().is_none());
    }

    #[test]
    fn only_off_max_and_keep_select() {
        let (user, project) = fixture();
        let dirs = [user.path().to_path_buf(), project.path().to_path_buf()];
        let (s, _) = Skills::discover(&dirs, &["b*".into(), "k*".into()], &["kit".into()], 200);
        assert_eq!(s.names(), ["bidama"]);
        let (mut s, _) = Skills::discover(&dirs, &[], &[], 2);
        assert_eq!(s.names(), ["arnes", "bidama"]);
        s.keep(&["bidama"]);
        assert_eq!(s.names(), ["bidama"]);
        assert!(s.json().contains("\"name\":\"bidama\""));
    }

    #[test]
    fn a_body_without_frontmatter_is_the_whole_text() {
        assert_eq!(body("plain"), "plain");
        assert_eq!(body("---\nname: x\n---\nrest"), "rest");
    }
}
