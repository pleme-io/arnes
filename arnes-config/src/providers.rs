//! Providers as data. A provider is the facts a hosted bidama declares as
//! `provider <name> <fact> <value>` rows (arnes ships them in `teikyou`); the
//! kernel keeps only the wire. The table is read from the bidama path on first
//! use and replaced when the hosted bidamas are loaded again.

use std::sync::{Arc, RwLock};

use arnes_anthropic::catalog::Auth;
use arnes_core::EchoRule;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub name: String,
    pub endpoint: String,
    pub model: String,
    pub key_env: String,
    pub key_required: bool,
    pub auth: Auth,
    pub echo: EchoRule,
    pub catalogue: Option<(String, Auth)>,
    pub thinking_budget: bool,
    pub peak: Vec<(u32, u32)>,
}

impl Facts {
    fn named(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            endpoint: String::new(),
            model: String::new(),
            key_env: format!("{}_API_KEY", name.to_uppercase().replace('-', "_")),
            key_required: true,
            auth: Auth::XApiKey,
            echo: EchoRule::Never,
            catalogue: None,
            thinking_budget: false,
            peak: Vec::new(),
        }
    }

    fn set(&mut self, fact: &str, value: &str) -> Result<(), String> {
        let one_of = |legal: &[&str]| format!("{fact} is one of {legal:?}, not {value:?}");
        match fact {
            "endpoint" => value.clone_into(&mut self.endpoint),
            "model" => value.clone_into(&mut self.model),
            "key-env" => value.clone_into(&mut self.key_env),
            "key" => {
                self.key_required = match value {
                    "required" => true,
                    "optional" => false,
                    _ => return Err(one_of(&["required", "optional"])),
                }
            }
            "auth" => self.auth = auth(value).ok_or_else(|| one_of(&["x-api-key", "bearer"]))?,
            "echo" => self.echo = echo(value).ok_or_else(|| one_of(&["never", "always", "when-tools"]))?,
            "catalogue" => {
                let mut w = value.split_whitespace();
                let (Some(url), Some(a), None) = (w.next(), w.next().and_then(auth), w.next()) else {
                    return Err(format!("catalogue is `<url> <x-api-key|bearer>`, not {value:?}"));
                };
                self.catalogue = Some((url.to_owned(), a));
            }
            "thinking-budget" => {
                self.thinking_budget = match value {
                    "honoured" => true,
                    "ignored" => false,
                    _ => return Err(one_of(&["honoured", "ignored"])),
                }
            }
            "peak" => self.peak = value.split_whitespace().map(hours).collect::<Option<_>>().ok_or_else(|| format!("peak is `<from>-<to>` UTC hours, 0..=24, not {value:?}"))?,
            other => return Err(format!("unknown fact {other:?}")),
        }
        Ok(())
    }
}

fn auth(s: &str) -> Option<Auth> {
    match s {
        "x-api-key" => Some(Auth::XApiKey),
        "bearer" => Some(Auth::Bearer),
        _ => None,
    }
}

fn echo(s: &str) -> Option<EchoRule> {
    match s {
        "never" => Some(EchoRule::Never),
        "always" => Some(EchoRule::Always),
        "when-tools" => Some(EchoRule::WhenTools),
        _ => None,
    }
}

fn hours(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.split_once('-')?;
    let (a, b) = (a.parse().ok()?, b.parse().ok()?);
    (a < b && b <= 24).then_some((a, b))
}

/// One provider: a cheap handle over its facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider(Arc<Facts>);

impl Provider {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.0.name
    }
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.0.endpoint
    }
    #[must_use]
    pub fn default_model(&self) -> &str {
        &self.0.model
    }
    #[must_use]
    pub fn default_auth(&self) -> Auth {
        self.0.auth
    }
    #[must_use]
    pub fn echo_rule(&self) -> EchoRule {
        self.0.echo
    }
    #[must_use]
    pub fn honours_thinking_budget(&self) -> bool {
        self.0.thinking_budget
    }
    #[must_use]
    pub fn models_url(&self) -> Option<(&str, Auth)> {
        self.0.catalogue.as_ref().map(|(u, a)| (u.as_str(), *a))
    }
    #[must_use]
    pub fn key_env(&self) -> &str {
        &self.0.key_env
    }
    #[must_use]
    pub fn requires_key(&self) -> bool {
        self.0.key_required
    }
    #[must_use]
    pub fn peak_hours(&self) -> &[(u32, u32)] {
        &self.0.peak
    }

    /// # Errors
    /// Names every known provider when `name` is not one of them.
    pub fn parse(name: &str) -> Result<Self, String> {
        current().get(name).ok_or_else(|| format!("unknown provedor {name:?}; expected one of {:?}", current().names()))
    }

    /// Every provider the table holds, in declaration order.
    #[must_use]
    pub fn all() -> Vec<Self> {
        current().0.clone()
    }
}

/// Every declared provider, in declaration order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Table(Vec<Provider>);

impl Table {
    /// Build from `(provider, fact, value)` rows. A bad fact is refused alone,
    /// named; a fact declared twice keeps the first.
    #[must_use]
    pub fn from_rows(rows: &[(String, String, String)]) -> (Self, Vec<String>) {
        let mut facts: Vec<(Facts, Vec<String>)> = Vec::new();
        let mut refused = Vec::new();
        for (name, fact, value) in rows {
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
                refused.push(format!("provider {name:?}: a name is lowercase letters, digits and -"));
                continue;
            }
            let i = facts.iter().position(|(f, _)| f.name == *name).unwrap_or_else(|| {
                facts.push((Facts::named(name), Vec::new()));
                facts.len() - 1
            });
            let (f, seen) = &mut facts[i];
            if seen.contains(fact) {
                continue;
            }
            seen.push(fact.clone());
            if let Err(e) = f.set(fact, value) {
                refused.push(format!("provider {name}: {e}"));
            }
        }
        (Self(facts.into_iter().map(|(f, _)| Provider(Arc::new(f))).collect()), refused)
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<Provider> {
        self.0.iter().find(|p| p.name() == name).cloned()
    }

    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.0.iter().map(|p| p.name().to_owned()).collect()
    }
}

static CURRENT: RwLock<Option<Arc<Table>>> = RwLock::new(None);

/// The providers in use: the hosted table once installed, else read from the
/// bidama path's `teikyou`.
#[must_use]
pub fn current() -> Arc<Table> {
    if let Some(t) = CURRENT.read().ok().and_then(|g| g.clone()) {
        return t;
    }
    let (reg, _) = arnes_blue::bidamas::Registry::load(&arnes_blue::bidamas::BidamaPath::from_env(), &["teikyou".to_owned()]);
    let t = Arc::new(Table::from_rows(&reg.providers()).0);
    if let Ok(mut g) = CURRENT.write() {
        g.get_or_insert_with(|| t.clone());
    }
    t
}

/// Replace the table with the hosted bidamas' providers; the refusals are
/// returned for the notice lane. An empty table is refused whole and the
/// running one kept, since no route could resolve.
#[must_use]
pub fn install(rows: &[(String, String, String)]) -> Vec<String> {
    let (table, mut refused) = Table::from_rows(rows);
    if table.0.is_empty() {
        refused.push("no provider is declared; keeping the providers in use".into());
        return refused;
    }
    if let Ok(mut g) = CURRENT.write() {
        *g = Some(Arc::new(table));
    }
    refused
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(p: &str, f: &str, v: &str) -> (String, String, String) {
        (p.into(), f.into(), v.into())
    }

    #[test]
    fn rows_become_typed_facts_and_a_bad_fact_is_refused_alone() {
        let (t, refused) = Table::from_rows(&[
            row("zed", "endpoint", "https://z.example"),
            row("zed", "echo", "sometimes"),
            row("zed", "peak", "6-10 25-26"),
            row("zed", "catalogue", "https://z.example/v1/models bearer"),
            row("zed", "endpoint", "https://second.example"),
            row("Bad", "model", "m"),
        ]);
        let z = t.get("zed").unwrap();
        assert_eq!(z.endpoint(), "https://z.example", "the first declaration wins");
        assert_eq!(z.echo_rule(), EchoRule::Never, "a refused fact keeps its default");
        assert!(z.peak_hours().is_empty());
        assert_eq!(z.models_url(), Some(("https://z.example/v1/models", Auth::Bearer)));
        assert_eq!((z.key_env(), z.requires_key(), z.default_auth()), ("ZED_API_KEY", true, Auth::XApiKey));
        assert_eq!(refused.len(), 3, "{refused:?}");
        assert!(refused.iter().any(|r| r.contains("echo")) && refused.iter().any(|r| r.contains("peak")) && refused.iter().any(|r| r.contains("\"Bad\"")));
        assert_eq!(t.names(), ["zed"]);
    }

    #[test]
    fn the_shipped_table_is_the_five_providers_with_their_facts() {
        let t = current();
        assert_eq!(t.names(), ["deepseek", "anthropic", "glm", "local", "messages"]);
        let d = t.get("deepseek").unwrap();
        assert_eq!((d.endpoint(), d.default_model(), d.echo_rule()), ("https://api.deepseek.com/anthropic", "deepseek-flash", EchoRule::WhenTools));
        assert_eq!(d.models_url(), Some(("https://api.deepseek.com/models", Auth::Bearer)));
        assert_eq!(d.peak_hours(), [(1, 4), (6, 10)]);
        let a = t.get("anthropic").unwrap();
        assert!(a.honours_thinking_budget() && a.echo_rule() == EchoRule::Always);
        let l = t.get("local").unwrap();
        assert!(!l.requires_key() && l.models_url().is_none());
        let m = t.get("messages").unwrap();
        assert!(m.endpoint().is_empty() && m.default_model().is_empty() && m.default_auth() == Auth::Bearer);
    }

    #[test]
    fn an_empty_install_keeps_the_table_in_use() {
        let before = current();
        assert!(install(&[]).iter().any(|r| r.contains("no provider")));
        assert_eq!(current(), before);
    }
}
