# parley-brain

A rolling transcript (`Transcript`, lines of speaker, start time in ms, text) and `notes()`.
`notes()` sends the transcript through any `parley_llm::LlmProvider`. The model answers with one `write_notes` tool call, and the request requires that tool.
The result is a summary and action items (text, optional owner).
A reply with no such tool call is an error, not empty notes.

## Run

Put one utterance on each line of a file, as `Speaker: text`. Then run `notes_say`.
It picks the adapter with the same environment variables as `llm_say` (see `crates/parley-llm/README.md`).

```sh
OPENAI_MODEL=gpt-x OPENAI_API_KEY=... cargo run -p parley-brain --bin notes_say -- --adapter openai meeting.txt
ANTHROPIC_MODEL=... ANTHROPIC_API_KEY=... cargo run -p parley-brain --bin notes_say -- --adapter anthropic meeting.txt
```

## Test

`cargo test -p parley-brain` replays recorded SSE fixtures from a local server. No network, no key.
