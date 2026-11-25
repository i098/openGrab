//! The model that answers meeting requests, chosen from the environment.

use parley_llm::{
    ChatRequest, DeltaStream, LlmError, LlmProvider, anthropic::Anthropic, openai::OpenAiCompatible,
};

pub enum Llm {
    OpenAi(OpenAiCompatible),
    Anthropic(Anthropic),
}

impl Llm {
    /// `PARLEY_LLM` is `openai` (any OpenAI-compatible server, see `OpenAiCompatible::from_env`) or
    /// `anthropic` (see `Anthropic::from_env`). There is no default: unset is an error.
    pub fn from_env() -> Result<Self, LlmError> {
        match std::env::var("PARLEY_LLM").as_deref() {
            Ok("openai") => Ok(Self::OpenAi(OpenAiCompatible::from_env()?)),
            Ok("anthropic") => Ok(Self::Anthropic(Anthropic::from_env()?)),
            Ok(other) => Err(LlmError::Config(format!(
                "PARLEY_LLM must be openai or anthropic, got {other}"
            ))),
            Err(_) => Err(LlmError::Config(
                "PARLEY_LLM is not set (openai or anthropic): no model, so no doc request".into(),
            )),
        }
    }
}

impl LlmProvider for Llm {
    async fn stream(&self, req: ChatRequest) -> Result<DeltaStream, LlmError> {
        match self {
            Self::OpenAi(p) => p.stream(req).await,
            Self::Anthropic(p) => p.stream(req).await,
        }
    }
}
