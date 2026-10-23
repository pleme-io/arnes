//! `arnes-anthropic` — Anthropic Messages API client.
//!
//! Thin, typed wrapper over `POST https://api.anthropic.com/v1/messages`.
//! Uses arnes-core types for Conversation / Message / Tool. Non-streaming
//! `send()` returns a parsed `Message` + `StopReason` + `Usage`. Streaming
//! support is interface-ready; the SSE parser lands in phase 2.

pub mod catalog;
pub mod reach;

use arnes_core::{
    ContentBlock, Conversation, EchoRule, Message, Role, StopReason, ThinkingMode, Tool, Usage,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AnthropicError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error ({status}): {body}")]
    Api { status: u16, body: String },
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("missing api key — set ANTHROPIC_API_KEY or pass explicitly")]
    MissingKey,
}

impl AnthropicError {
    /// Whether this failure says the *provider* cannot serve right now, so
    /// the next route should be tried: unreachable, timed out, out of
    /// credit, unauthorised, rate limited or failing. A malformed request
    /// (400/404/422) or an unparseable success is not: another provider
    /// would be handed the same bad request.
    #[must_use]
    pub fn warrants_failover(&self) -> bool {
        match self {
            Self::Http(_) | Self::MissingKey => true,
            Self::Api { status, .. } => {
                *status == 402
                    || is_transient(*status)
                    || matches!(
                        handan::judge(*status, None).verdict(),
                        handan::Verdict::Unauthorized | handan::Verdict::Throttled | handan::Verdict::ServerError
                    )
            }
            Self::Json(_) => false,
        }
    }

    #[must_use]
    pub fn verdict(&self) -> String {
        match self {
            Self::Http(e) if e.is_timeout() => "timed-out".into(),
            Self::Http(_) => "unreachable".into(),
            Self::MissingKey => "unauthorized".into(),
            Self::Api { status, .. } => handan::judge(*status, None).verdict().to_string(),
            Self::Json(_) => "other".into(),
        }
    }

    /// One short line for an operator: `402 Insufficient Balance`,
    /// `unreachable`, `timed out`. The full error (request id, body) is in
    /// the log file; a toast needs only this.
    #[must_use]
    pub fn brief(&self) -> String {
        match self {
            Self::Api { status, body } => {
                let message = serde_json::from_str::<serde_json::Value>(body).ok().and_then(|v| {
                    v.pointer("/error/message").and_then(serde_json::Value::as_str).map(str::to_owned)
                });
                // DeepSeek appends "(request_id: …)" to the message; keep the words.
                let message = message.map_or_else(|| body.chars().take(60).collect(), |m| m.split(" (request_id").next().unwrap_or(&m).trim().to_owned());
                format!("{status} {message}")
            }
            Self::Http(e) if e.is_timeout() => "timed out".to_owned(),
            Self::Http(e) if e.is_connect() => "unreachable".to_owned(),
            Self::Http(e) => e.to_string().chars().take(60).collect(),
            Self::Json(_) => "unreadable response".to_owned(),
            Self::MissingKey => "no API key".to_owned(),
        }
    }
}

pub type Result<T> = std::result::Result<T, AnthropicError>;

/// How long a connection may take before the route counts as unreachable.
/// Separate from the whole-request timeout: a model may legitimately think
/// for minutes, but a host that does not accept a connection in this long
/// is down, and the next route should get the turn.
pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn http_client(timeout: Option<std::time::Duration>, connect: Option<std::time::Duration>) -> reqwest::Client {
    let b = reqwest::Client::builder().connect_timeout(connect.unwrap_or(CONNECT_TIMEOUT));
    let b = match timeout {
        Some(t) => b.timeout(t),
        None => b,
    };
    b.build().unwrap_or_default()
}

/// A route tried when the one before it fails with
/// [`AnthropicError::warrants_failover`]: its own endpoint, key and
/// request params, and the model it serves.
#[derive(Debug, Clone)]
pub struct Fallback {
    /// Names the route in logs (`gpubox`, `on-device`, `anthropic`).
    pub label: String,
    pub model: String,
    pub client: AnthropicClient,
    /// Whether this route is sent the tool list (off for a small-context model).
    pub tools: bool,
}

/// Every per-request knob beyond `model/messages/system/max_tokens`.
/// `None` / empty means "leave it off the wire" — the provider's default.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestParams {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub stop_sequences: Vec<String>,
    /// Reasoning mode — rendered as `thinking` + `output_config.effort`.
    pub thinking: ThinkingMode,
    /// When prior `thinking` blocks are re-sent in history.
    pub echo: EchoRule,
    /// Whole-request timeout. `None` = reqwest's default (none).
    pub timeout: Option<std::time::Duration>,
    /// How long a connection may take before the route counts as
    /// unreachable. `None` = [`CONNECT_TIMEOUT`].
    pub connect_timeout: Option<std::time::Duration>,
    /// Retries on 408/409/429/5xx/529. 0 = fail on the first error.
    pub max_retries: u32,
    /// First backoff; doubles per attempt unless `retry-after` says otherwise.
    pub retry_base: std::time::Duration,
    pub extra_headers: Vec<(String, String)>,
}

impl Default for RequestParams {
    fn default() -> Self {
        Self {
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: Vec::new(),
            thinking: ThinkingMode::ProviderDefault,
            echo: EchoRule::Always,
            timeout: None,
            connect_timeout: None,
            max_retries: 0,
            retry_base: std::time::Duration::from_millis(500),
            extra_headers: Vec::new(),
        }
    }
}

/// Client configuration — endpoint + auth + version + request params.
#[derive(Debug, Clone)]
pub struct AnthropicClient {
    pub base_url: String,
    /// The credential, as cofre's `Secret`: its `Debug` is redacted, it
    /// cannot be formatted into a string, and it is scrubbed on drop. Shared
    /// by `Arc` because every turn clones the client and `Secret` is
    /// deliberately not `Clone`. `expose()` is called only where the key
    /// becomes an HTTP header — here and in the catalogue fetch — so
    /// `rg 'expose\(\)'` lists every place it can leave the process.
    /// `None` for a server that takes no key (a local llama.cpp): no
    /// `x-api-key` header is sent at all.
    pub api_key: Option<std::sync::Arc<cofre_secret::Secret>>,
    pub api_version: String,
    pub params: RequestParams,
    pub client: reqwest::Client,
    /// Tried in order when this route fails with a failover-class error.
    pub fallbacks: Vec<Fallback>,
    /// This route's name in notices (`deepseek`, `anthropic`, `local`).
    pub label: String,
    /// Appended to the system prompt on requests THIS route sends.
    pub system_note: Option<String>,
    /// Which route answered last — shared by every clone of this client
    /// (each turn clones it), so the status line can say who replied.
    pub served: std::sync::Arc<std::sync::Mutex<Option<Served>>>,
    pub auth: catalog::Auth,
    pub tools: bool,
    pub provider: String,
    pub health: Health,
}

pub type Health = std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, (String, std::time::Instant)>>>;

/// The route that answered a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Served {
    pub label: String,
    pub model: String,
    /// True when it was not this client's own route.
    pub fallback: bool,
    pub provider: String,
    pub latency_ms: u64,
}

impl AnthropicClient {
    /// Build from `ANTHROPIC_API_KEY`, failing if unset. The endpoint
    /// defaults to the public Anthropic API; override via `with_base_url`
    /// for staging / self-hosted bedrock / etc.
    ///
    /// # Errors
    /// [`AnthropicError::MissingKey`] when the variable is unset or empty.
    pub fn from_env() -> Result<Self> {
        let api_key = cofre_secret::Secret::from_env("ANTHROPIC_API_KEY").map_err(|_| AnthropicError::MissingKey)?;
        Ok(Self::with_key(api_key))
    }

    #[must_use]
    pub fn with_key(api_key: cofre_secret::Secret) -> Self {
        Self::new(Some(api_key))
    }

    /// A client whose key may be absent (a keyless local server).
    #[must_use]
    pub fn new(api_key: Option<cofre_secret::Secret>) -> Self {
        Self {
            base_url: "https://api.anthropic.com".to_string(),
            api_key: api_key.map(std::sync::Arc::new),
            api_version: "2023-06-01".to_string(),
            params: RequestParams::default(),
            client: http_client(None, None),
            fallbacks: Vec::new(),
            label: "primary".to_owned(),
            system_note: None,
            served: std::sync::Arc::default(),
            auth: catalog::Auth::XApiKey,
            tools: true,
            provider: "anthropic".to_owned(),
            health: Health::default(),
        }
    }

    #[must_use]
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = provider.into();
        self
    }

    #[must_use]
    pub fn health(&self) -> Vec<(String, String, u64)> {
        self.health.lock().map_or_else(
            |_| Vec::new(),
            |h| {
                h.iter()
                    .map(|(label, (verdict, at))| (label.clone(), verdict.clone(), u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX)))
                    .collect()
            },
        )
    }

    #[must_use]
    pub fn with_auth(mut self, auth: catalog::Auth) -> Self {
        self.auth = auth;
        self
    }

    #[must_use]
    pub fn with_tools(mut self, tools: bool) -> Self {
        self.tools = tools;
        self
    }

    #[must_use]
    pub fn with_system_note(mut self, note: Option<String>) -> Self {
        self.system_note = note;
        self
    }

    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    #[must_use]
    pub fn with_fallbacks(mut self, fallbacks: Vec<Fallback>) -> Self {
        self.fallbacks = fallbacks;
        self
    }

    #[must_use]
    pub fn with_api_version(mut self, v: impl Into<String>) -> Self {
        self.api_version = v.into();
        self
    }

    /// Apply request params; rebuilds the HTTP client when a timeout is set.
    #[must_use]
    pub fn with_params(mut self, params: RequestParams) -> Self {
        if params.timeout.is_some() || params.connect_timeout.is_some() {
            self.client = http_client(params.timeout, params.connect_timeout);
        }
        self.params = params;
        self
    }

    fn body(&self, conv: &Conversation, tools: &[Tool], model: &str) -> MessagesRequest {
        let p = &self.params;
        let messages = if p.echo.echoes(!tools.is_empty()) {
            conv.messages.clone()
        } else {
            strip_thinking(&conv.messages)
        };
        MessagesRequest {
            model: model.to_string(),
            messages,
            system: match (&conv.system, &self.system_note) {
                (s, None) => s.clone(),
                (None, Some(n)) => Some(n.clone()),
                (Some(s), Some(n)) => Some(format!("{s}\n\n{n}")),
            },
            max_tokens: conv.max_tokens.unwrap_or(arnes_core::DEFAULT_MAX_TOKENS),
            tools: if tools.is_empty() { None } else { Some(tools.to_vec()) },
            temperature: p.temperature,
            top_p: p.top_p,
            top_k: p.top_k,
            stop_sequences: p.stop_sequences.clone(),
            thinking: match p.thinking {
                ThinkingMode::ProviderDefault => None,
                ThinkingMode::Off => Some(ThinkingParam { kind: "disabled", budget_tokens: None }),
                ThinkingMode::On { budget, .. } => {
                    Some(ThinkingParam { kind: "enabled", budget_tokens: budget })
                }
            },
            output_config: match p.thinking {
                ThinkingMode::On { effort: Some(e), .. } => Some(OutputConfig { effort: e.name() }),
                _ => None,
            },
        }
    }

    #[must_use]
    pub fn with_base_url(mut self, base: impl Into<String>) -> Self {
        self.base_url = base.into();
        self
    }

    /// Send a non-streaming request — the most common path for agent loops.
    /// Retries transient statuses per `params.max_retries`, then, on a
    /// failover-class error, gives the turn to each fallback in order. The
    /// last failover-class error is returned when every route fails.
    ///
    /// # Errors
    /// A non-failover error from the route that answered, or the last
    /// failover-class error when no route could serve the turn.
    pub async fn send(&self, conv: &Conversation, tools: &[Tool]) -> Result<MessagesResponse> {
        let own = if self.tools { tools } else { &[] };
        let started = std::time::Instant::now();
        let mut last = match self.send_as(conv, own, &conv.model).await {
            Err(e) if e.warrants_failover() => {
                self.note(&self.label, e.verdict());
                e
            }
            done => {
                self.settle(&self.label, &self.provider, &conv.model, started, &done);
                return done;
            }
        };
        let mut from = self.label.as_str();
        for fb in &self.fallbacks {
            // The one line a toast shows; the full error goes to the log.
            tracing::warn!("{from} {} → {}", last.brief(), fb.label);
            tracing::debug!(route = %fb.label, error = %last, "provider failed; trying the next route");
            let tools = if fb.tools { tools } else { &[] };
            let started = std::time::Instant::now();
            last = match fb.client.send_as(conv, tools, &fb.model).await {
                Err(e) if e.warrants_failover() => {
                    self.note(&fb.label, e.verdict());
                    e
                }
                done => {
                    self.settle(&fb.label, &fb.client.provider, &fb.model, started, &done);
                    return done;
                }
            };
            from = fb.label.as_str();
        }
        Err(last)
    }

    fn note(&self, label: &str, verdict: String) {
        if let Ok(mut h) = self.health.lock() {
            h.insert(label.to_owned(), (verdict, std::time::Instant::now()));
        }
    }

    fn settle(&self, label: &str, provider: &str, model: &str, started: std::time::Instant, done: &Result<MessagesResponse>) {
        match done {
            Ok(_) => {
                self.note(label, "success".to_owned());
                if let Ok(mut s) = self.served.lock() {
                    *s = Some(Served {
                        label: label.to_owned(),
                        model: model.to_owned(),
                        fallback: label != self.label,
                        provider: provider.to_owned(),
                        latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    });
                }
            }
            Err(e) => self.note(label, e.verdict()),
        }
    }

    /// Which route answered the most recent successful turn.
    #[must_use]
    pub fn served(&self) -> Option<Served> {
        self.served.lock().ok().and_then(|s| s.clone())
    }

    /// One route, with its retries — no fallbacks.
    async fn send_as(&self, conv: &Conversation, tools: &[Tool], model: &str) -> Result<MessagesResponse> {
        let body = self.body(conv, tools, model);
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let mut attempt = 0u32;
        loop {
            let mut req = self
                .client
                .post(&url)
                .header("anthropic-version", &self.api_version)
                .header("content-type", "application/json");
            if let Some(key) = &self.api_key {
                req = match self.auth {
                    catalog::Auth::XApiKey => req.header("x-api-key", key.expose()),
                    catalog::Auth::Bearer => req.bearer_auth(key.expose()),
                };
            }
            for (k, v) in &self.params.extra_headers {
                req = req.header(k.as_str(), v.as_str());
            }
            let resp = req.json(&body).send().await?;
            let status = resp.status();
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(handan::parse_retry_after)
                .map(handan::RetryAdvice::delay_now);
            let text = resp.text().await?;
            if status.is_success() {
                return Ok(serde_json::from_str(&text)?);
            }
            if attempt < self.params.max_retries && is_transient(status.as_u16()) {
                let backoff = retry_after
                    .unwrap_or_else(|| self.params.retry_base.saturating_mul(1 << attempt.min(16)));
                tracing::warn!(status = status.as_u16(), attempt, ?backoff, "retrying");
                tokio::time::sleep(backoff).await;
                attempt += 1;
                continue;
            }
            return Err(AnthropicError::Api {
                status: status.as_u16(),
                body: text,
            });
        }
    }
}

/// Statuses worth retrying: timeout, conflict, rate limit, server/overload.
#[must_use]
pub const fn is_transient(status: u16) -> bool {
    handan::Status::new(status).is_transient() || matches!(status, 409 | 529)
}

fn strip_thinking(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .map(|m| Message {
            role: m.role,
            content: m
                .content
                .iter()
                .filter(|b| {
                    !matches!(b, ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. })
                })
                .cloned()
                .collect(),
        })
        .filter(|m| !m.content.is_empty())
        .collect()
}

#[derive(Debug, Serialize)]
struct ThinkingParam {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_tokens: Option<u32>,
}

#[derive(Debug, Serialize)]
struct OutputConfig {
    effort: &'static str,
}

#[derive(Debug, Serialize)]
struct MessagesRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingParam>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<OutputConfig>,
}

/// The API's response shape — normalized into our types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagesResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub role: Role,
    pub model: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<StopReason>,
    #[serde(default)]
    pub stop_sequence: Option<String>,
    #[serde(default)]
    pub usage: Usage,
}

impl MessagesResponse {
    /// Lift the response into a full `Message` the conversation can append.
    #[must_use]
    pub fn to_message(&self) -> Message {
        Message {
            role: self.role,
            content: self.content.clone(),
        }
    }

    /// Return every tool use the assistant requested this turn.
    #[must_use]
    pub fn tool_uses(&self) -> Vec<(String, String, serde_json::Value)> {
        self.content
            .iter()
            .filter_map(|c| match c {
                ContentBlock::ToolUse { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
            .collect()
    }
}

/// Test support shared by this crate's tests and its callers' (a turn run
/// against a scripted provider). Not part of the API.
#[doc(hidden)]
pub mod testing {
    /// A one-shot HTTP server on a free loopback port: answers the first
    /// request with `status` and `body`, and hands back the request head.
    ///
    /// # Panics
    /// When loopback cannot be bound or the request is malformed (a test
    /// failure either way).
    #[must_use]
    pub fn one_shot_server(status: &'static str, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
                if line == "\r\n" {
                    break;
                }
                head.push_str(&line);
            }
            let mut sink = vec![0u8; length];
            reader.read_exact(&mut sink).unwrap();
            let reply = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
            // The request's head, a blank line, then its body.
            format!("{head}\r\n{}", String::from_utf8_lossy(&sink))
        });
        (url, handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> cofre_secret::Secret {
        cofre_secret::Secret::new(s).unwrap()
    }

    #[test]
    fn the_key_never_appears_in_debug_output() {
        let c = AnthropicClient::with_key(key("sk-super-secret-value"));
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("sk-super-secret-value"), "{dbg}");
        assert!(dbg.contains("Secret(***)"));
    }

    #[test]
    fn client_defaults_to_public_api() {
        let c = AnthropicClient::with_key(key("sk-test"));
        assert_eq!(c.base_url, "https://api.anthropic.com");
        assert_eq!(c.api_version, "2023-06-01");
    }

    #[test]
    fn with_base_url_overrides() {
        let c = AnthropicClient::with_key(key("sk-test")).with_base_url("https://proxy.example.com/v1");
        assert_eq!(c.base_url, "https://proxy.example.com/v1");
    }

    #[test]
    fn missing_env_errors_explicitly() {
        // SAFETY: we only remove within this test's scope.
        let had = std::env::var_os("ANTHROPIC_API_KEY");
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
        let err = AnthropicClient::from_env().unwrap_err();
        assert!(matches!(err, AnthropicError::MissingKey));
        if let Some(v) = had {
            unsafe { std::env::set_var("ANTHROPIC_API_KEY", v) };
        }
    }

    #[test]
    fn tool_uses_extractor() {
        let resp = MessagesResponse {
            id: "msg_1".into(),
            kind: "message".into(),
            role: Role::Assistant,
            model: "claude-sonnet-4-6".into(),
            content: vec![
                ContentBlock::Text {
                    text: "I'll read that.".into(),
                },
                ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({ "path": "/tmp/x" }),
                },
            ],
            stop_reason: Some(StopReason::ToolUse),
            stop_sequence: None,
            usage: Usage::default(),
        };
        let tuses = resp.tool_uses();
        assert_eq!(tuses.len(), 1);
        assert_eq!(tuses[0].1, "read_file");
    }

    #[test]
    fn unset_params_stay_off_the_wire() {
        let c = AnthropicClient::with_key(key("k"));
        let conv = Conversation::new(arnes_core::ConversationId(1), "deepseek-chat");
        let v = serde_json::to_value(c.body(&conv, &[], &conv.model)).unwrap();
        for k in ["temperature", "top_p", "top_k", "stop_sequences", "thinking", "tools"] {
            assert!(v.get(k).is_none(), "{k} leaked onto the wire");
        }
    }

    #[test]
    fn a_route_note_joins_the_system_prompt_on_that_route_only() {
        let mut conv = Conversation::new(arnes_core::ConversationId(1), "local");
        conv.system = Some("Be brief.".into());
        let noted = AnthropicClient::new(None).with_system_note(Some("You are gpt-oss-20b on gpubox.".into()));
        let v = serde_json::to_value(noted.body(&conv, &[], "local")).unwrap();
        assert_eq!(v["system"], "Be brief.\n\nYou are gpt-oss-20b on gpubox.");
        let plain = AnthropicClient::with_key(key("k"));
        let v = serde_json::to_value(plain.body(&conv, &[], "deepseek-flash")).unwrap();
        assert_eq!(v["system"], "Be brief.", "another route's note never leaks here");
    }

    #[test]
    fn set_params_reach_the_wire() {
        let c = AnthropicClient::with_key(key("k")).with_params(RequestParams {
            temperature: Some(0.2),
            top_p: Some(0.9),
            stop_sequences: vec!["END".into()],
            thinking: ThinkingMode::On { effort: Some(arnes_core::Effort::Max), budget: Some(2048) },
            ..RequestParams::default()
        });
        let conv = Conversation::new(arnes_core::ConversationId(1), "deepseek-reasoner");
        let v = serde_json::to_value(c.body(&conv, &[], &conv.model)).unwrap();
        assert_eq!(v["top_p"], serde_json::json!(0.9_f32));
        assert_eq!(v["stop_sequences"][0], "END");
        assert_eq!(v["thinking"]["type"], "enabled");
        assert_eq!(v["thinking"]["budget_tokens"], 2048);
        assert_eq!(v["output_config"]["effort"], "max");
    }

    #[test]
    fn thinking_echo_follows_rule_and_tool_presence() {
        let mut conv = Conversation::new(arnes_core::ConversationId(1), "deepseek-flash");
        conv.messages.push(Message {
            role: arnes_core::Role::Assistant,
            content: vec![
                ContentBlock::Thinking { thinking: "t".into(), signature: None },
                ContentBlock::Text { text: "a".into() },
            ],
        });
        let tool = Tool {
            name: "read".into(),
            description: "d".into(),
            input_schema: arnes_core::ToolInputSchema(serde_json::json!({"type": "object"})),
        };
        for rule in EchoRule::ALL {
            let c = AnthropicClient::with_key(key("k"))
                .with_params(RequestParams { echo: rule, ..RequestParams::default() });
            for tools in [vec![], vec![tool.clone()]] {
                let kept = c.body(&conv, &tools, &conv.model).messages[0].content.len() == 2;
                assert_eq!(kept, rule.echoes(!tools.is_empty()), "{rule:?} tools={}", tools.len());
            }
        }
    }

    #[test]
    fn thinking_off_is_explicitly_disabled_on_the_wire() {
        let c = AnthropicClient::with_key(key("k"))
            .with_params(RequestParams { thinking: ThinkingMode::Off, ..RequestParams::default() });
        let conv = Conversation::new(arnes_core::ConversationId(1), "deepseek-flash");
        let v = serde_json::to_value(c.body(&conv, &[], &conv.model)).unwrap();
        assert_eq!(v["thinking"]["type"], "disabled");
        assert!(v["thinking"].get("budget_tokens").is_none());
        assert!(v.get("output_config").is_none());
    }

    #[test]
    fn failover_class() {
        let api = |status| AnthropicError::Api { status, body: String::new() };
        for s in [401, 402, 403, 408, 429, 500, 503, 529] {
            assert!(api(s).warrants_failover(), "{s}");
        }
        for s in [400, 404, 422] {
            assert!(!api(s).warrants_failover(), "{s}: another provider gets the same bad request");
        }
        assert!(AnthropicError::MissingKey.warrants_failover());
        let json = serde_json::from_str::<u8>("x").unwrap_err();
        assert!(!AnthropicError::Json(json).warrants_failover());
    }

    use crate::testing::one_shot_server;

    const LOCAL_REPLY: &str = r#"{"id":"m","type":"message","role":"assistant","model":"local","content":[{"type":"text","text":"pong"}],"stop_reason":"end_turn"}"#;

    /// A port nothing listens on: bound, then released.
    fn refused_url() -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    }

    #[tokio::test]
    async fn an_unreachable_primary_hands_the_turn_to_the_fallback() {
        let (local, served) = one_shot_server("200 OK", LOCAL_REPLY);
        let client = AnthropicClient::with_key(key("sk-primary"))
            .with_base_url(refused_url())
            .with_label("deepseek")
            .with_provider("deepseek")
            .with_fallbacks(vec![Fallback {
                label: "local".into(),
                model: "local".into(),
                client: AnthropicClient::new(None).with_base_url(local).with_provider("local"),
                tools: true,
            }]);
        let conv = Conversation::new(arnes_core::ConversationId(1), "deepseek-flash");
        let resp = client.send(&conv, &[]).await.unwrap();
        assert_eq!(resp.model, "local");
        let head = served.join().unwrap().to_ascii_lowercase();
        assert!(!head.contains("x-api-key"), "a keyless route sends no key: {head}");
        let s = client.served().expect("the status line must be able to say who answered");
        assert_eq!((s.label.as_str(), s.model.as_str(), s.fallback, s.provider.as_str()), ("local", "local", true, "local"));
        let mut health = client.health();
        health.sort();
        assert_eq!(
            health.iter().map(|(l, v, _)| (l.as_str(), v.as_str())).collect::<Vec<_>>(),
            [("deepseek", "unreachable"), ("local", "success")],
            "every route tried carries its verdict"
        );
    }

    #[tokio::test]
    async fn a_route_with_tools_off_is_sent_none() {
        let (local, served) = one_shot_server("200 OK", LOCAL_REPLY);
        let client = AnthropicClient::with_key(key("k")).with_base_url(refused_url()).with_fallbacks(vec![Fallback {
            label: "on-device".into(),
            model: "apple-on-device".into(),
            client: AnthropicClient::new(None).with_base_url(local),
            tools: false,
        }]);
        let tool = Tool {
            name: "read".into(),
            description: "d".into(),
            input_schema: arnes_core::ToolInputSchema(serde_json::json!({"type": "object"})),
        };
        let conv = Conversation::new(arnes_core::ConversationId(1), "deepseek-flash");
        client.send(&conv, &[tool]).await.unwrap();
        let request = served.join().unwrap();
        assert!(!request.contains("\"tools\""), "a 4k-context route must not be sent the tool list: {request}");
        assert!(request.contains("apple-on-device"), "the route's own model is requested");
    }

    #[tokio::test]
    async fn a_bearer_route_sends_authorization_and_no_x_api_key() {
        let (url, served) = one_shot_server("200 OK", LOCAL_REPLY);
        let client = AnthropicClient::with_key(key("sk-or")).with_base_url(url).with_auth(catalog::Auth::Bearer);
        let conv = Conversation::new(arnes_core::ConversationId(1), "example/judge-router");
        client.send(&conv, &[]).await.unwrap();
        let head = served.join().unwrap().to_ascii_lowercase();
        assert!(head.contains("authorization: bearer sk-or"), "{head}");
        assert!(!head.contains("x-api-key"), "{head}");
        let (url, served) = one_shot_server("200 OK", LOCAL_REPLY);
        AnthropicClient::with_key(key("sk-ant")).with_base_url(url).send(&conv, &[]).await.unwrap();
        let head = served.join().unwrap().to_ascii_lowercase();
        assert!(head.contains("x-api-key: sk-ant") && !head.contains("authorization"), "{head}");
    }

    #[tokio::test]
    async fn a_primary_with_tools_off_is_sent_none() {
        let tool = || Tool {
            name: "read".into(),
            description: "d".into(),
            input_schema: arnes_core::ToolInputSchema(serde_json::json!({"type": "object"})),
        };
        let conv = Conversation::new(arnes_core::ConversationId(1), "apple-on-device");
        let (url, served) = one_shot_server("200 OK", LOCAL_REPLY);
        AnthropicClient::new(None).with_base_url(url).with_tools(false).send(&conv, &[tool()]).await.unwrap();
        assert!(!served.join().unwrap().contains("\"tools\""));
        let (url, served) = one_shot_server("200 OK", LOCAL_REPLY);
        AnthropicClient::new(None).with_base_url(url).send(&conv, &[tool()]).await.unwrap();
        assert!(served.join().unwrap().contains("\"tools\""), "control: a primary with tools on is sent them");
    }

    #[test]
    fn brief_is_one_line_an_operator_can_read() {
        // DeepSeek's real 402 body, 2026-09-28.
        let body = r#"{"error":{"message":"Insufficient Balance (request_id: 3c368656-d8fa-4359-95f4-132f13e5f6c7)","type":"unknown_error","param":null,"code":"invalid_request_error"}}"#;
        let e = AnthropicError::Api { status: 402, body: body.into() };
        assert_eq!(e.brief(), "402 Insufficient Balance");
        assert_eq!(AnthropicError::Api { status: 500, body: "oops".into() }.brief(), "500 oops");
        assert_eq!(AnthropicError::MissingKey.brief(), "no API key");
    }

    #[tokio::test]
    async fn a_bad_request_is_not_retried_elsewhere() {
        let (primary, _served) = one_shot_server("400 Bad Request", r#"{"error":"bad"}"#);
        let client = AnthropicClient::with_key(key("k")).with_base_url(primary).with_fallbacks(vec![Fallback {
            label: "local".into(),
            model: "local".into(),
            client: AnthropicClient::new(None).with_base_url(refused_url()),
            tools: true,
        }]);
        let conv = Conversation::new(arnes_core::ConversationId(1), "deepseek-flash");
        let err = client.send(&conv, &[]).await.unwrap_err();
        assert!(matches!(err, AnthropicError::Api { status: 400, .. }), "{err}");
    }

    #[tokio::test]
    async fn every_route_failing_returns_the_last_failure() {
        let client = AnthropicClient::with_key(key("k")).with_base_url(refused_url()).with_fallbacks(vec![Fallback {
            label: "local".into(),
            model: "local".into(),
            client: AnthropicClient::new(None).with_base_url(refused_url()),
            tools: true,
        }]);
        let conv = Conversation::new(arnes_core::ConversationId(1), "deepseek-flash");
        assert!(matches!(client.send(&conv, &[]).await, Err(AnthropicError::Http(_))));
    }

    #[test]
    fn transient_statuses() {
        for s in [408, 409, 421, 429, 500, 502, 503, 504, 529] {
            assert!(is_transient(s), "{s}");
        }
        for s in [400, 401, 403, 404, 422, 501, 505] {
            assert!(!is_transient(s), "{s}: a permanent refusal is not retried");
        }
    }

    #[test]
    fn a_permanent_server_error_fails_over_without_a_retry_and_every_failure_has_a_verdict() {
        let api = |status| AnthropicError::Api { status, body: String::new() };
        assert!(api(501).warrants_failover() && !is_transient(501));
        assert_eq!(api(401).verdict(), "unauthorized");
        assert_eq!(api(429).verdict(), "throttled");
        assert_eq!(api(503).verdict(), "server-error");
        assert_eq!(api(400).verdict(), "other");
        assert_eq!(AnthropicError::MissingKey.verdict(), "unauthorized");
    }

    #[test]
    fn retry_after_is_read_in_both_forms() {
        let delay = |v: &str| handan::parse_retry_after(v).map(handan::RetryAdvice::delay_now);
        assert_eq!(delay("2"), Some(std::time::Duration::from_secs(2)));
        assert_eq!(delay("Wed, 21 Oct 2015 07:28:00 GMT"), Some(std::time::Duration::ZERO));
        assert_eq!(delay("soon"), None);
    }
}
