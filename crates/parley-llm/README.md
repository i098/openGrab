# parley-llm

One streaming trait, `LlmProvider`, with two adapters. The trait names no provider.

- `ChatRequest`: system text, messages (text, images, tool calls, tool results) tool definitions, and an optional `required_tool` (the name of one tool the model must call).
  The OpenAI adapter sends it as `tool_choice` `{"type":"function","function":{"name":...}}`; the Anthropic adapter sends `{"type":"tool","name":...}`. Unset, the body has no `tool_choice`.
- `stream()` returns `Delta` items: `Text`, a complete `ToolCall`, and a last `End(FinishReason)`.
  A body that stops before the end reason gives `LlmError::StreamCut`. A non-2xx answer gives `LlmError::Api`.
  The OpenAI adapter ends with `End(ToolCalls)` when it emitted tool calls, even if the server reports `stop` or sends only `[DONE]`.
  When the model stops on its token limit in the middle of a tool call, the adapter drops that incomplete call and ends with `End(Length)`.
  A cut tool call with any other end reason gives `LlmError::Decode`.

## Configure

| Adapter | Variables |
|---|---|
| `openai::OpenAiCompatible` (chat completions; also Ollama, vLLM, OpenRouter) | `OPENAI_MODEL` (required), `OPENAI_BASE_URL` (default `https://api.openai.com/v1`), `OPENAI_API_KEY` (optional for local servers) |
| `anthropic::Anthropic` (messages API) | `ANTHROPIC_API_KEY`, `ANTHROPIC_MODEL` (required), `ANTHROPIC_BASE_URL` (default `https://api.anthropic.com`) |

## Run

```sh
OPENAI_MODEL=gpt-x OPENAI_API_KEY=... cargo run -p parley-llm --bin llm_say -- --adapter openai "Say hello"
ANTHROPIC_MODEL=... ANTHROPIC_API_KEY=... cargo run -p parley-llm --bin llm_say -- --adapter anthropic "Say hello"
```

`llm_say` prints text as it arrives, then tool calls and the end reason.

## Test

`cargo test -p parley-llm` replays recorded SSE fixtures (`tests/fixtures`) from a local server. No network, no key.
