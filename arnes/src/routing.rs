use arnes_anthropic::AnthropicClient;
use arnes_config::ArnesConfig;
use arnes_core::{ContentBlock, Conversation, EchoRule, Role};
use arnes_loop::ledger::Totals;
use serde_json::{Value, json};

pub const EVENT: &str = "choose-route";

pub struct Ask<'a> {
    pub cfg: &'a ArnesConfig,
    pub base: &'a ArnesConfig,
    pub client: &'a AnthropicClient,
    pub totals: Totals,
    pub conversation: &'a Conversation,
    pub prompt: &'a str,
    pub source: &'a str,
}

#[must_use]
pub fn enabled(cfg: &ArnesConfig) -> bool {
    cfg.routing.enabled == Some(true)
}

#[must_use]
pub fn budget(cfg: &ArnesConfig) -> std::time::Duration {
    std::time::Duration::from_millis(u64::from(arnes_config::sections::bounded("routing.budget_ms", cfg.routing.budget_ms, 100, 5000, 1000).0))
}

fn tail(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut start = text.len() - max_bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn last_turn_tool_calls(conv: &Conversation) -> usize {
    conv.messages
        .iter()
        .rev()
        .skip(1)
        .take_while(|m| m.role != Role::User || m.content.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. })))
        .flat_map(|m| m.content.iter())
        .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
        .count()
}

const fn echo_name(rule: EchoRule) -> &'static str {
    match rule {
        EchoRule::Never => "never",
        EchoRule::Always => "always",
        EchoRule::WhenTools => "when-tools",
    }
}

pub fn label_of(cfg: &ArnesConfig) -> String {
    cfg.route.label.clone().unwrap_or_else(|| cfg.provedor.clone())
}

#[must_use]
pub fn labels(base: &ArnesConfig) -> Vec<String> {
    std::iter::once(label_of(base)).chain(base.fallbacks.iter().map(|r| r.label.clone().unwrap_or_else(|| r.provedor.clone()))).collect()
}

pub fn reach_for(cfg: &ArnesConfig) -> (Option<arnes_blue::Reach>, Vec<String>) {
    let (endpoints, refused) = cfg.endpoints();
    if endpoints.is_empty() {
        return (None, refused);
    }
    let ceiling = budget(cfg).saturating_sub(std::time::Duration::from_millis(50));
    let endpoints = endpoints
        .into_iter()
        .map(|mut e| {
            e.timeout = e.timeout.min(ceiling);
            e
        })
        .collect();
    (Some(arnes_blue::Reach(arnes_anthropic::reach::reach(endpoints, tokio::runtime::Handle::current()))), refused)
}

#[must_use]
pub fn facts(ask: &Ask<'_>, conversation_text: &str, now_ms: u128) -> String {
    let cfg = ask.cfg;
    let health = ask.client.health();
    let (fallbacks, _) = cfg.fallback_routes();
    let routes: Vec<Value> = cfg
        .primary()
        .into_iter()
        .map(|r| (r, false))
        .chain(fallbacks.into_iter().enumerate().map(|(i, r)| {
            let drops = cfg.switch_to(i).drops_foreign_thinking(cfg);
            (r, drops)
        }))
        .map(|(r, drops)| {
            let f = &r.facts;
            let price = match (f.price_in, f.price_out) {
                (Some(i), Some(o)) => json!({"in": i, "out": o, "cache": f.price_cache.unwrap_or(i)}),
                _ => Value::Null,
            };
            let h = health
                .iter()
                .find(|(l, _, _)| *l == r.label)
                .map_or(json!({"verdict": "unknown", "age_ms": Value::Null}), |(_, v, age)| json!({"verdict": v, "age_ms": age}));
            json!({
                "label": r.label,
                "provider": r.provider.name(),
                "model": r.model,
                "tools": r.tools,
                "paid": r.provider.requires_key(),
                "tier": f.tier,
                "describe": f.describe,
                "price": price,
                "context_window": f.context_window,
                "echo": echo_name(r.provider.echo_rule()),
                "usable": true,
                "switch_drops_thinking": drops,
                "health": h,
            })
        })
        .collect();
    let tail_kib = arnes_config::sections::bounded("routing.tail_kib", cfg.routing.tail_kib, 1, 64, 16).0;
    let last = ask.totals.last;
    let served = ask.client.served();
    json!({
        "v": 1,
        "prompt": ask.prompt,
        "tail": tail(conversation_text, usize::try_from(tail_kib).unwrap_or(16) * 1024),
        "turn": ask.conversation.messages.iter().filter(|m| m.role == Role::User).count(),
        "last_turn_tool_calls": last_turn_tool_calls(ask.conversation),
        "context": {
            "last_prompt_tokens": last.input_tokens + last.cache_read_input_tokens.unwrap_or(0) + last.cache_creation_input_tokens.unwrap_or(0),
            "last_output_tokens": last.output_tokens,
            "prompt_chars": ask.prompt.chars().count(),
            "max_tokens": ask.conversation.max_tokens.unwrap_or(arnes_core::DEFAULT_MAX_TOKENS),
        },
        "cache_hit_ratio": ask.totals.cache_hit_ratio(),
        "current": label_of(cfg),
        "current_source": ask.source,
        "configured_primary": label_of(ask.base),
        "last_served": served.map(|s| json!({"label": s.label, "fallback": s.fallback, "latency_ms": s.latency_ms})),
        "routes": routes,
        "knobs": {
            "judge": cfg.routing.judge,
            "min_confidence": arnes_config::sections::bounded("routing.min_confidence", cfg.routing.min_confidence, 0, 100, 70).0,
            "stick_turns": arnes_config::sections::bounded("routing.stick_turns", cfg.routing.stick_turns, 0, 50, 3).0,
            "cooldown_secs": arnes_config::sections::bounded("routing.cooldown_secs", cfg.routing.cooldown_secs, 0, 86_400, 300).0,
            "allow_paid": cfg.routing.allow_paid.unwrap_or(false),
        },
        "now_ms": u64::try_from(now_ms).unwrap_or(u64::MAX),
    })
    .to_string()
}

pub struct Answer {
    pub route: Option<String>,
    pub why: Option<String>,
}

#[must_use]
pub fn answer(text: &str) -> Answer {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(m)) => Answer {
            route: m.get("route").and_then(Value::as_str).map(str::to_owned),
            why: m.get("why").and_then(Value::as_str).map(str::to_owned),
        },
        _ if text.trim().is_empty() => Answer { route: None, why: None },
        _ => Answer { route: Some(text.trim().to_owned()), why: None },
    }
}

pub fn log(cfg: &ArnesConfig, facts: &str, outcome: &str) {
    let path = match cfg.routing.log.as_deref() {
        Some("off") => return,
        Some(p) => arnes_blue::expand_home(std::path::Path::new(p)),
        None => match arnes_store::Layout::from_env() {
            Some(l) => l.usage_ledger().with_file_name("routes.jsonl"),
            None => return,
        },
    };
    let mut record: Value = serde_json::from_str(facts).unwrap_or(Value::Null);
    if let Value::Object(m) = &mut record {
        let bytes = m.remove("tail").and_then(|t| t.as_str().map(str::len));
        m.insert("tail_bytes".into(), bytes.map_or(Value::Null, Value::from));
        m.insert("outcome".into(), serde_json::from_str(outcome).unwrap_or_else(|_| Value::String(outcome.to_owned())));
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let line = format!("{record}\n");
    let written = std::fs::OpenOptions::new().create(true).append(true).open(&path).and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
    if let Err(e) = written {
        tracing::warn!(error = %e, path = %path.display(), "routing log: append failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_a_route_a_bare_label_or_an_abstention() {
        let a = answer(r#"{"route":"gpubox","why":"floor"}"#);
        assert_eq!((a.route.as_deref(), a.why.as_deref()), (Some("gpubox"), Some("floor")));
        assert_eq!(answer(r#"{"route":null,"why":"sticky"}"#).route, None);
        assert_eq!(answer("opus").route.as_deref(), Some("opus"));
        assert_eq!(answer("").route, None);
    }

    #[test]
    fn the_decision_log_keeps_the_facts_and_outcome_and_drops_the_tail() {
        let dir = std::env::temp_dir().join(format!("arnes-routes-{}", std::process::id()));
        let path = dir.join("routes.jsonl");
        let cfg = ArnesConfig { routing: arnes_config::RoutingSection { log: Some(path.display().to_string()), ..Default::default() }, ..ArnesConfig::default() };
        log(&cfg, r#"{"v":1,"tail":"secret words","current":"gpubox"}"#, r#"{"route":"opus"}"#);
        let line = std::fs::read_to_string(&path).unwrap();
        let v: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!((v["current"].as_str(), v["outcome"]["route"].as_str(), v["tail_bytes"].as_u64()), (Some("gpubox"), Some("opus"), Some(12)));
        assert!(!line.contains("secret words"));
        let off = ArnesConfig { routing: arnes_config::RoutingSection { log: Some("off".into()), ..Default::default() }, ..ArnesConfig::default() };
        log(&off, "{}", "{}");
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1, "off writes nothing");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_tail_keeps_the_newest_bytes_on_a_char_boundary() {
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("ab", 9), "ab");
        assert_eq!(tail("aé", 1), "");
        assert_eq!(tail("aéb", 2), "b");
    }

    #[test]
    fn facts_name_every_route_with_its_data_and_the_current_one() {
        let cfg = ArnesConfig {
            fallbacks: vec![arnes_config::RouteDecl {
                provedor: "local".into(),
                endpoint: Some("http://127.0.0.1:8434".into()),
                tools: Some(false),
                label: Some("on-device".into()),
                facts: arnes_config::RouteFacts { tier: Some(0), context_window: Some(4096), ..Default::default() },
                ..Default::default()
            }],
            ..ArnesConfig::default()
        };
        let client = AnthropicClient::new(None);
        let mut conv = Conversation::new(arnes_core::ConversationId(1), "local");
        conv.push(arnes_core::Message::user_text("hello"));
        let ask = Ask { cfg: &cfg, base: &cfg, client: &client, totals: Totals::default(), conversation: &conv, prompt: "hello", source: "config" };
        let v: Value = serde_json::from_str(&facts(&ask, "user: hello", 5)).unwrap();
        assert_eq!((v["v"].as_u64(), v["current"].as_str(), v["current_source"].as_str(), v["turn"].as_u64()), (Some(1), Some("local"), Some("config"), Some(1)));
        let labels: Vec<&str> = v["routes"].as_array().unwrap().iter().map(|r| r["label"].as_str().unwrap()).collect();
        assert_eq!(labels, ["local", "on-device"]);
        assert_eq!((v["routes"][1]["tools"].as_bool(), v["routes"][1]["context_window"].as_u64()), (Some(false), Some(4096)));
        assert_eq!(v["routes"][0]["health"]["verdict"].as_str(), Some("unknown"));
        assert_eq!(v["knobs"]["min_confidence"].as_u64(), Some(70));
    }
}
