//! Anthropic messages API adapter.

use crate::sse::{Decoder, SseEvent, delta_stream};
use crate::{
    ChatRequest, Delta, DeltaStream, FinishReason, LlmError, LlmProvider, Part, Role, env_required,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const API_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 4096;

pub struct Anthropic {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl Anthropic {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key: api_key.into(),
            model: model.into(),
        }
    }

    /// `ANTHROPIC_API_KEY` and `ANTHROPIC_MODEL` (required), `ANTHROPIC_BASE_URL`
    /// (default `https://api.anthropic.com`).
    pub fn from_env() -> Result<Self, LlmError> {
        let base = std::env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".into());
        Ok(Self::new(
            base,
            env_required("ANTHROPIC_API_KEY")?,
            env_required("ANTHROPIC_MODEL")?,
        ))
    }

    fn body(&self, req: &ChatRequest) -> Value {
        let msgs: Vec<Value> = req
            .messages
            .iter()
            .map(|m| {
                let content: Vec<Value> = m
                    .parts
                    .iter()
                    .map(|p| match p {
                        Part::Text(t) => json!({"type": "text", "text": t}),
                        Part::Image { media_type, data } => json!({"type": "image", "source": {
                            "type": "base64", "media_type": media_type, "data": STANDARD.encode(data)}}),
                        Part::ToolCall { id, name, arguments } => {
                            json!({"type": "tool_use", "id": id, "name": name, "input": arguments})
                        }
                        Part::ToolResult { id, content } => {
                            json!({"type": "tool_result", "tool_use_id": id, "content": content})
                        }
                    })
                    .collect();
                json!({"role": if m.role == Role::User { "user" } else { "assistant" }, "content": content})
            })
            .collect();
        let mut body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            "messages": msgs,
            "stream": true,
        });
        if let Some(s) = &req.system {
            body["system"] = json!(s);
        }
        if !req.tools.is_empty() {
            body["tools"] = req
                .tools
                .iter()
                .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.parameters}))
                .collect();
        }
        body
    }
}

impl LlmProvider for Anthropic {
    async fn stream(&self, req: ChatRequest) -> Result<DeltaStream, LlmError> {
        let resp = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&self.body(&req))
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            return Err(LlmError::Api {
                status,
                body: resp.text().await.unwrap_or_default(),
            });
        }
        Ok(delta_stream(
            Box::pin(resp.bytes_stream()),
            MessagesDecoder::default(),
        ))
    }
}

#[derive(Default)]
struct MessagesDecoder {
    /// content block index -> (id, name, argument fragments) for tool_use blocks
    tools: BTreeMap<u64, (String, String, String)>,
    stop: Option<String>,
    cut: Option<String>,
    done: bool,
}

impl Decoder for MessagesDecoder {
    fn event(&mut self, ev: SseEvent) -> Result<Vec<Delta>, LlmError> {
        let v: Value =
            serde_json::from_str(&ev.data).map_err(|e| LlmError::Decode(e.to_string()))?;
        let idx = v["index"].as_u64().unwrap_or(0);
        let mut out = Vec::new();
        match v["type"].as_str().or(ev.event.as_deref()).unwrap_or("") {
            "content_block_start" if v["content_block"]["type"] == "tool_use" => {
                let b = &v["content_block"];
                self.tools.insert(
                    idx,
                    (
                        b["id"].as_str().unwrap_or("").into(),
                        b["name"].as_str().unwrap_or("").into(),
                        String::new(),
                    ),
                );
            }
            "content_block_delta" => match v["delta"]["type"].as_str() {
                Some("text_delta") => {
                    if let Some(t) = v["delta"]["text"].as_str().filter(|t| !t.is_empty()) {
                        out.push(Delta::Text(t.to_owned()));
                    }
                }
                Some("input_json_delta") => {
                    if let (Some(e), Some(p)) = (
                        self.tools.get_mut(&idx),
                        v["delta"]["partial_json"].as_str(),
                    ) {
                        e.2.push_str(p);
                    }
                }
                _ => {}
            },
            "content_block_stop" => {
                if let Some((id, name, args)) = self.tools.remove(&idx) {
                    let arguments = if args.is_empty() {
                        json!({})
                    } else {
                        match serde_json::from_str(&args) {
                            Ok(v) => v,
                            Err(e) => {
                                self.cut = Some(e.to_string());
                                return Ok(out);
                            }
                        }
                    };
                    out.push(Delta::ToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
            }
            "message_delta" => {
                if let Some(r) = v["delta"]["stop_reason"].as_str() {
                    self.stop = Some(r.to_owned());
                }
            }
            "message_stop" => {
                if let Some(e) = self
                    .cut
                    .take()
                    .filter(|_| self.stop.as_deref() != Some("max_tokens"))
                {
                    return Err(LlmError::Decode(format!("tool input: {e}")));
                }
                self.done = true;
                out.push(Delta::End(match self.stop.as_deref() {
                    Some("end_turn") | Some("stop_sequence") | None => FinishReason::Stop,
                    Some("max_tokens") => FinishReason::Length,
                    Some("tool_use") => FinishReason::ToolCalls,
                    Some(o) => FinishReason::Other(o.to_owned()),
                }));
            }
            "error" => return Err(LlmError::Decode(v["error"].to_string())),
            _ => {}
        }
        Ok(out)
    }

    fn done(&self) -> bool {
        self.done
    }
}
