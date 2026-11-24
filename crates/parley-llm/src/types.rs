use futures_util::Stream;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("configuration: {0}")]
    Config(String),
    #[error("transport: {0}")]
    Http(#[from] reqwest::Error),
    #[error("server returned HTTP {status}: {body}")]
    Api { status: u16, body: String },
    #[error("undecodable stream data: {0}")]
    Decode(String),
    #[error("stream ended before the server signalled an end reason")]
    StreamCut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Text(String),
    Image {
        media_type: String,
        data: Vec<u8>,
    },
    /// A tool call the model made earlier (assistant turns).
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
    /// The result of a tool call (user turns).
    ToolResult {
        id: String,
        content: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            parts: vec![Part::Text(text.into())],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments object.
    pub parameters: Value,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatRequest {
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    Text(String),
    /// A complete tool call: the adapter joins streamed argument fragments.
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
    /// Always the last item of a complete stream.
    End(FinishReason),
}

pub type DeltaStream = Pin<Box<dyn Stream<Item = Result<Delta, LlmError>> + Send>>;

pub trait LlmProvider {
    fn stream(
        &self,
        req: ChatRequest,
    ) -> impl Future<Output = Result<DeltaStream, LlmError>> + Send;
}

pub(crate) fn env_required(name: &str) -> Result<String, LlmError> {
    std::env::var(name).map_err(|_| LlmError::Config(format!("{name} is not set")))
}
