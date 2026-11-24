//! OpenAI-compatible chat completions adapter (any base URL: OpenAI, Ollama, vLLM, OpenRouter).

use crate::sse::{Decoder, SseEvent, delta_stream};
use crate::{
    ChatRequest, Delta, DeltaStream, FinishReason, LlmError, LlmProvider, Part, Role, env_required,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub struct OpenAiCompatible {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
}

impl OpenAiCompatible {
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key,
            model: model.into(),
        }
    }

    /// `OPENAI_BASE_URL` (default `https://api.openai.com/v1`), `OPENAI_API_KEY` (optional, local
    /// servers need none), `OPENAI_MODEL` (required).
    pub fn from_env() -> Result<Self, LlmError> {
        let base =
            std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into());
        Ok(Self::new(
            base,
            std::env::var("OPENAI_API_KEY").ok(),
            env_required("OPENAI_MODEL")?,
        ))
    }

    fn body(&self, req: &ChatRequest) -> Value {
        let mut msgs = Vec::new();
        if let Some(s) = &req.system {
            msgs.push(json!({"role": "system", "content": s}));
        }
        for m in &req.messages {
            let role = if m.role == Role::User {
                "user"
            } else {
                "assistant"
            };
            let mut content = Vec::new();
            let mut calls = Vec::new();
            for p in &m.parts {
                match p {
                    Part::Text(t) => content.push(json!({"type": "text", "text": t})),
                    Part::Image { media_type, data } => content.push(json!({
                        "type": "image_url",
                        "image_url": {"url": format!("data:{media_type};base64,{}", STANDARD.encode(data))}
                    })),
                    Part::ToolCall { id, name, arguments } => calls.push(json!({
                        "id": id, "type": "function",
                        "function": {"name": name, "arguments": arguments.to_string()}
                    })),
                    Part::ToolResult { id, content } => {
                        msgs.push(json!({"role": "tool", "tool_call_id": id, "content": content}))
                    }
                }
            }
            if content.is_empty() && calls.is_empty() {
                continue;
            }
            let mut msg = json!({"role": role});
            if !content.is_empty() {
                msg["content"] = Value::Array(content);
            }
            if !calls.is_empty() {
                msg["tool_calls"] = Value::Array(calls);
            }
            msgs.push(msg);
        }
        let mut body = json!({"model": self.model, "messages": msgs, "stream": true});
        if let Some(n) = req.max_tokens {
            body["max_tokens"] = json!(n);
        }
        if !req.tools.is_empty() {
            body["tools"] = req
                .tools
                .iter()
                .map(|t| {
                    json!({"type": "function", "function": {
                    "name": t.name, "description": t.description, "parameters": t.parameters}})
                })
                .collect();
        }
        if let Some(n) = &req.required_tool {
            body["tool_choice"] = json!({"type": "function", "function": {"name": n}});
        }
        body
    }
}

impl LlmProvider for OpenAiCompatible {
    async fn stream(&self, req: ChatRequest) -> Result<DeltaStream, LlmError> {
        let mut rb = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .json(&self.body(&req));
        if let Some(k) = &self.api_key {
            rb = rb.bearer_auth(k);
        }
        let resp = rb.send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            return Err(LlmError::Api {
                status,
                body: resp.text().await.unwrap_or_default(),
            });
        }
        Ok(delta_stream(
            Box::pin(resp.bytes_stream()),
            ChatDecoder::default(),
        ))
    }
}

#[derive(Default)]
struct ChatDecoder {
    /// index -> (id, name, argument fragments)
    calls: BTreeMap<u64, (String, String, String)>,
    done: bool,
}

impl ChatDecoder {
    fn flush_calls(&mut self, drop_cut: bool) -> Result<Vec<Delta>, LlmError> {
        let mut out = Vec::new();
        for (id, name, args) in std::mem::take(&mut self.calls).into_values() {
            let arguments = if args.is_empty() {
                json!({})
            } else {
                match serde_json::from_str(&args) {
                    Ok(v) => v,
                    Err(_) if drop_cut => continue,
                    Err(e) => return Err(LlmError::Decode(format!("tool arguments: {e}"))),
                }
            };
            out.push(Delta::ToolCall {
                id,
                name,
                arguments,
            });
        }
        Ok(out)
    }
}

impl Decoder for ChatDecoder {
    fn event(&mut self, ev: SseEvent) -> Result<Vec<Delta>, LlmError> {
        if ev.data.trim() == "[DONE]" {
            // Servers that omit finish_reason still end cleanly here.
            self.done = true;
            let mut out = self.flush_calls(false)?;
            let reason = if out.is_empty() {
                FinishReason::Stop
            } else {
                FinishReason::ToolCalls
            };
            out.push(Delta::End(reason));
            return Ok(out);
        }
        let v: Value =
            serde_json::from_str(&ev.data).map_err(|e| LlmError::Decode(e.to_string()))?;
        if let Some(err) = v.get("error") {
            return Err(LlmError::Decode(err.to_string()));
        }
        let mut out = Vec::new();
        let Some(choice) = v["choices"].get(0) else {
            return Ok(out);
        };
        if let Some(t) = choice["delta"]["content"]
            .as_str()
            .filter(|t| !t.is_empty())
        {
            out.push(Delta::Text(t.to_owned()));
        }
        for c in choice["delta"]["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let e = self
                .calls
                .entry(c["index"].as_u64().unwrap_or(0))
                .or_default();
            if let Some(id) = c["id"].as_str() {
                e.0 = id.to_owned();
            }
            if let Some(n) = c["function"]["name"].as_str() {
                e.1.push_str(n);
            }
            if let Some(a) = c["function"]["arguments"].as_str() {
                e.2.push_str(a);
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            let calls = self.flush_calls(reason == "length")?;
            let has_calls = !calls.is_empty();
            out.extend(calls);
            out.push(Delta::End(match reason {
                "stop" if has_calls => FinishReason::ToolCalls,
                "stop" => FinishReason::Stop,
                "length" => FinishReason::Length,
                "tool_calls" | "function_call" => FinishReason::ToolCalls,
                o => FinishReason::Other(o.to_owned()),
            }));
            self.done = true;
        }
        Ok(out)
    }

    fn done(&self) -> bool {
        self.done
    }
}
