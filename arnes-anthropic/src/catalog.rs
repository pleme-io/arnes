//! The provider's model catalogue — read from the provider, never restated.
//!
//! `GET /models` is the authority for which model ids exist and what they
//! can do. It is cached on disk (`$XDG_CACHE_HOME/arnes/models-<provider>.json`)
//! so a normal start costs no request; a stale or missing cache is
//! refreshed. A failed refresh is *blind*, not *empty*: the caller warns
//! and proceeds unchecked rather than refusing every model.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// How the catalogue request authenticates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    Bearer,
    XApiKey,
}

/// One model as the provider describes it. Unknown fields are ignored;
/// fields a provider omits stay `None` / empty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<EffortInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_capabilities: Option<ApiCapabilities>,
}

/// `DeepSeek`: `"effort": {"supported_levels": [...], "default_level": "high"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffortInfo {
    #[serde(default)]
    pub supported_levels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_level: Option<String>,
}

/// `DeepSeek`: `"api_capabilities": {"anthropic_messages": {...}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_messages: Option<AnthropicMessagesCaps>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnthropicMessagesCaps {
    /// `in-history` (a system update may appear mid-conversation) or
    /// `leading-only` (system prompt only at the start).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_update: Option<String>,
}

impl ModelInfo {
    #[must_use]
    pub fn supported_levels(&self) -> &[String] {
        self.effort.as_ref().map_or(&[], |e| e.supported_levels.as_slice())
    }

    #[must_use]
    pub fn system_prompt_update(&self) -> Option<&str> {
        self.api_capabilities
            .as_ref()?
            .anthropic_messages
            .as_ref()?
            .system_prompt_update
            .as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub fetched_at: u64,
    pub models: Vec<ModelInfo>,
}

impl Catalog {
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&ModelInfo> {
        self.models.iter().find(|m| m.id == id)
    }

    #[must_use]
    pub fn ids(&self) -> Vec<&str> {
        self.models.iter().map(|m| m.id.as_str()).collect()
    }

    /// # Errors
    /// Names the model and every id the catalogue does contain.
    pub fn require(&self, id: &str) -> Result<&ModelInfo, String> {
        self.get(id).ok_or_else(|| {
            format!("model {id:?} is not in the provider catalogue; available: {:?}", self.ids())
        })
    }

}

impl arnes_store::cache::Stamped for Catalog {
    fn fetched_at(&self) -> u64 {
        self.fetched_at
    }
}

#[derive(Deserialize)]
struct ListResponse {
    data: Vec<ModelInfo>,
}


/// `$XDG_CACHE_HOME/arnes/models-<provider>.json`, else `~/.cache/…`.
#[must_use]
pub fn cache_path(provider: &str) -> Option<PathBuf> {
    arnes_store::Layout::from_env().map(|l| l.models_cache(provider))
}

/// Parse a `{"data": [...]}` catalogue body.
///
/// # Errors
/// When the body is not a catalogue.
pub fn parse(body: &str) -> Result<Vec<ModelInfo>, serde_json::Error> {
    serde_json::from_str::<ListResponse>(body).map(|r| r.data)
}

/// Fetch the catalogue from the provider.
///
/// # Errors
/// Transport failure, a non-2xx status (with its body), or a bad body.
pub async fn fetch(url: &str, auth: Auth, key: &cofre_secret::Secret, timeout: Duration) -> Result<Catalog, String> {
    let client = reqwest::Client::builder().timeout(timeout).build().map_err(|e| e.to_string())?;
    let req = match auth {
        Auth::Bearer => client.get(url).bearer_auth(key.expose()),
        Auth::XApiKey => client.get(url).header("x-api-key", key.expose()).header("anthropic-version", "2023-06-01"),
    };
    let resp = req.send().await.map_err(|e| format!("GET {url}: {e}"))?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("GET {url}: {status}: {body}"));
    }
    let models = parse(&body).map_err(|e| format!("GET {url}: {e}"))?;
    Ok(Catalog { fetched_at: arnes_store::cache::now_secs(), models })
}

/// The cached catalogue if younger than `max_age`, else a fresh fetch
/// (written back to the cache). A failed fetch falls back to a stale
/// cache before giving up.
///
/// # Errors
/// Only when there is neither a usable cache nor a successful fetch.
pub async fn load_or_refresh(
    provider: &str,
    url: &str,
    auth: Auth,
    key: &cofre_secret::Secret,
    max_age: Duration,
) -> Result<Catalog, String> {
    let path = cache_path(provider);
    arnes_store::cache::load_or_refresh(path.as_deref(), max_age, || fetch(url, auth, key, Duration::from_secs(5))).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // DeepSeek's live /models body, verbatim, 2026-09-26. (An earlier
    // fixture was copied from a DuckDB `unnest(recursive:=true)` view, which
    // flattened `effort.supported_levels` — the parser matched a shape the
    // provider never sends. Fixtures come from the raw body only.)
    const DEEPSEEK: &str = r#"{"object":"list","data":[
      {"id":"deepseek-flash","object":"model","owned_by":"deepseek","name":"DeepSeek-V4.1-Flash",
       "context_window":1048576,"max_output_tokens":393216,"input_modalities":["text","image"],
       "output_modalities":["text"],"effort":{"supported_levels":["low","high","max"],"default_level":"high"},
       "api_capabilities":{"anthropic_messages":{"system_prompt_update":"in-history"}}},
      {"id":"deepseek-v4-pro","object":"model","owned_by":"deepseek","name":"DeepSeek-V4-Pro",
       "context_window":1048576,"max_output_tokens":393216,"input_modalities":["text"],
       "output_modalities":["text"],"effort":{"supported_levels":["low","high","max"],"default_level":"high"},
       "api_capabilities":{"anthropic_messages":{"system_prompt_update":"leading-only"}}}]}"#;

    // Anthropic's shape: no window fields.
    const ANTHROPIC: &str = r#"{"data":[{"id":"claude-opus-5-5","type":"model","display_name":"Opus 5.5"}],"has_more":false}"#;

    #[test]
    fn parses_both_provider_shapes() {
        let d = parse(DEEPSEEK).unwrap();
        assert_eq!(d[0].context_window, Some(1_048_576));
        assert_eq!(d[0].supported_levels(), ["low", "high", "max"]);
        assert_eq!(d[0].system_prompt_update(), Some("in-history"));
        assert_eq!(d[1].system_prompt_update(), Some("leading-only"));
        let a = parse(ANTHROPIC).unwrap();
        assert_eq!(a[0].id, "claude-opus-5-5");
        assert_eq!(a[0].context_window, None);
    }

    #[test]
    fn require_names_what_exists() {
        let c = Catalog { fetched_at: 0, models: parse(DEEPSEEK).unwrap() };
        assert!(c.require("deepseek-flash").is_ok());
        let err = c.require("deepseek-chat").unwrap_err();
        assert!(err.contains("deepseek-chat") && err.contains("deepseek-v4-pro"), "{err}");
    }
}
