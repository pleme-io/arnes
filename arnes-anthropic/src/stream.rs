//! Server-sent events from the Messages API, assembled into the same
//! [`MessagesResponse`] a non-streaming call returns, with each text and
//! thinking delta handed to a [`Sink`] as it arrives.

use serde_json::{Value, json};

use crate::{AnthropicError, MessagesResponse, Result};

/// One piece of a reply as it streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delta {
    Text(String),
    Thinking(String),
    /// A new attempt began (a retry or a fallback route): drop what streamed.
    Restart,
}

/// Where deltas go: the UI's live view of the reply.
#[derive(Clone)]
pub struct Sink(std::sync::Arc<dyn Fn(Delta) + Send + Sync>);

impl Sink {
    pub fn new(f: impl Fn(Delta) + Send + Sync + 'static) -> Self {
        Self(std::sync::Arc::new(f))
    }

    pub fn send(&self, d: Delta) {
        (self.0)(d);
    }
}

impl std::fmt::Debug for Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sink")
    }
}

/// The reply being assembled: the message head, each content block as the
/// JSON `content_block_start` gave it with its deltas applied, and the tool
/// input JSON still arriving in pieces.
#[derive(Debug, Default)]
pub struct Assembler {
    buffer: String,
    message: Value,
    blocks: Vec<Value>,
    partial_json: Vec<String>,
    done: bool,
}

impl Assembler {
    /// Feed raw bytes; every complete event is applied and its delta, if
    /// any, sent to `sink`.
    ///
    /// # Errors
    /// An `error` event, or an event whose JSON does not parse.
    pub fn feed(&mut self, bytes: &[u8], sink: &Sink) -> Result<()> {
        self.buffer.push_str(&String::from_utf8_lossy(bytes));
        while let Some(end) = self.buffer.find("\n\n") {
            let event: String = self.buffer.drain(..end + 2).collect();
            let data: String = event.lines().filter_map(|l| l.strip_prefix("data:")).map(str::trim_start).collect::<Vec<_>>().join("\n");
            if data.is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(&data)?;
            if let Some(d) = self.apply(&v)? {
                sink.send(d);
            }
        }
        Ok(())
    }

    fn apply(&mut self, v: &Value) -> Result<Option<Delta>> {
        let idx = v.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).unwrap_or(0);
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => self.message = v.get("message").cloned().unwrap_or_default(),
            "content_block_start" => {
                if self.blocks.len() <= idx {
                    self.blocks.resize(idx + 1, Value::Null);
                    self.partial_json.resize(idx + 1, String::new());
                }
                self.blocks[idx] = v.get("content_block").cloned().unwrap_or_default();
            }
            "content_block_delta" => {
                let d = v.get("delta").cloned().unwrap_or_default();
                let Some(block) = self.blocks.get_mut(idx) else { return Ok(None) };
                let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
                match d.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text_delta" => {
                        let t = s("text");
                        append(block, "text", &t);
                        return Ok(Some(Delta::Text(t)));
                    }
                    "thinking_delta" => {
                        let t = s("thinking");
                        append(block, "thinking", &t);
                        return Ok(Some(Delta::Thinking(t)));
                    }
                    "signature_delta" => block["signature"] = Value::String(s("signature")),
                    "input_json_delta" => self.partial_json[idx].push_str(&s("partial_json")),
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let (Some(block), Some(raw)) = (self.blocks.get_mut(idx), self.partial_json.get(idx))
                    && block.get("type").and_then(Value::as_str) == Some("tool_use")
                    && !raw.is_empty()
                {
                    block["input"] = serde_json::from_str(raw)?;
                }
            }
            "message_delta" => {
                if let Some(d) = v.get("delta").and_then(Value::as_object) {
                    for (k, val) in d {
                        self.message[k] = val.clone();
                    }
                }
                if let Some(u) = v.get("usage").and_then(Value::as_object) {
                    for (k, val) in u {
                        self.message["usage"][k] = val.clone();
                    }
                }
            }
            "message_stop" => self.done = true,
            "error" => return Err(AnthropicError::Api { status: 529, body: v.to_string() }),
            _ => {}
        }
        Ok(None)
    }

    /// The assembled reply.
    ///
    /// # Errors
    /// The stream ended before `message_stop`, or the result is not a message.
    pub fn finish(mut self) -> Result<MessagesResponse> {
        if !self.done {
            return Err(AnthropicError::Api { status: 529, body: "the stream ended before message_stop".into() });
        }
        self.message["content"] = Value::Array(self.blocks.into_iter().filter(|b| !b.is_null()).collect());
        if self.message.get("type").is_none() {
            self.message["type"] = json!("message");
        }
        Ok(serde_json::from_value(self.message)?)
    }
}

fn append(block: &mut Value, key: &str, more: &str) {
    let now = block.get(key).and_then(Value::as_str).unwrap_or("").to_owned();
    block[key] = Value::String(now + more);
}

#[cfg(test)]
mod tests {
    use super::*;
    use arnes_core::{ContentBlock, StopReason};

    fn events(es: &[Value]) -> String {
        es.iter().map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap())).collect()
    }

    #[test]
    fn a_stream_assembles_the_message_a_plain_call_returns() {
        let raw = events(&[
            json!({"type":"message_start","message":{"id":"m","type":"message","role":"assistant","model":"glm-5.3","content":[],"usage":{"input_tokens":5,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"let me "}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"see"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hel"}}),
            json!({"type":"ping"}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"lo"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t1","name":"Read","input":{}}}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"file_"}}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"path\":\"a.rs\"}"}}),
            json!({"type":"content_block_stop","index":2}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":42}}),
            json!({"type":"message_stop"}),
        ]);
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = {
            let seen = seen.clone();
            Sink::new(move |d| seen.lock().unwrap().push(d))
        };
        let mut a = Assembler::default();
        // Fed in uneven pieces, as a network delivers it.
        for chunk in raw.as_bytes().chunks(37) {
            a.feed(chunk, &sink).unwrap();
        }
        let r = a.finish().unwrap();
        assert_eq!(r.stop_reason, Some(StopReason::ToolUse));
        assert_eq!((r.usage.input_tokens, r.usage.output_tokens), (5, 42));
        assert!(matches!(&r.content[0], ContentBlock::Thinking { thinking, signature } if thinking == "let me see" && signature.as_deref() == Some("sig")));
        assert!(matches!(&r.content[1], ContentBlock::Text { text } if text == "Hello"));
        assert!(matches!(&r.content[2], ContentBlock::ToolUse { input, .. } if input["file_path"] == "a.rs"));
        assert_eq!(*seen.lock().unwrap(), [Delta::Thinking("let me ".into()), Delta::Thinking("see".into()), Delta::Text("Hel".into()), Delta::Text("lo".into())]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_streaming_client_draws_deltas_and_returns_the_whole_message() {
        const SSE: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"local\",\"content\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":0}}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi \"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"there\"}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let (url, seen) = crate::testing::one_shot_server("200 OK", SSE);
        let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut client = crate::AnthropicClient::new(None).with_base_url(url);
        client.stream = Some({
            let got = got.clone();
            Sink::new(move |d| got.lock().unwrap().push(d))
        });
        let mut conv = arnes_core::Conversation::default();
        conv.push(arnes_core::Message::user_text("hello"));
        let r = client.send(&conv, &[]).await.unwrap();
        assert!(seen.join().unwrap().contains("\"stream\":true"), "the request asks for a stream");
        assert!(matches!(&r.content[0], ContentBlock::Text { text } if text == "hi there"));
        assert_eq!(*got.lock().unwrap(), [Delta::Restart, Delta::Text("hi ".into()), Delta::Text("there".into())]);
        let (url, seen) = crate::testing::one_shot_server(
            "200 OK",
            r#"{"id":"m","type":"message","role":"assistant","model":"m","content":[{"type":"text","text":"whole"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#,
        );
        let plain = crate::AnthropicClient::new(None).with_base_url(url);
        assert!(matches!(&plain.send(&conv, &[]).await.unwrap().content[0], ContentBlock::Text { text } if text == "whole"));
        assert!(!seen.join().unwrap().contains("\"stream\""), "no sink, no stream asked");
    }

    #[test]
    fn an_error_event_or_a_cut_stream_is_an_error() {
        let sink = Sink::new(|_| {});
        let mut a = Assembler::default();
        let err = a.feed(events(&[json!({"type":"error","error":{"type":"overloaded_error","message":"busy"}})]).as_bytes(), &sink).unwrap_err();
        assert!(err.to_string().contains("overloaded"), "{err}");
        let mut cut = Assembler::default();
        cut.feed(events(&[json!({"type":"message_start","message":{"id":"m","role":"assistant","model":"m","content":[],"usage":{"input_tokens":1,"output_tokens":0}}})]).as_bytes(), &sink).unwrap();
        assert!(cut.finish().is_err(), "no message_stop, no message");
    }
}
