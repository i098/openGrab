//! LLM layer (plan 5.1 row 4, 5.2): one streaming trait, no provider name in it.
//!
//! Adapters: [`openai::OpenAiCompatible`] (chat completions, any base URL) and
//! [`anthropic::Anthropic`] (messages API). Each reads key, base URL and model from
//! the environment via its `from_env`.

pub mod anthropic;
pub mod openai;
mod sse;
mod types;

pub use types::*;
