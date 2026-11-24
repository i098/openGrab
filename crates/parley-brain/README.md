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

## Ask

`find_request(text, name)` returns the question when a line or chat message starts with the bot name ("Parley, ...", "hey parley ..."), in any case, with or without the comma. The default name is `Parley`. Otherwise it returns `None`.
`Transcript::answer()` sends the transcript and the question as plain text (no tool). The answer is at most 500 characters.

`ask_say` prints `not a request` (exit 0) when the request does not start with the name. Otherwise it prints the answer. A request that contains "recap" or "summar" (`is_recap_request`) gets `Transcript::recap()` instead: the summary and the action items with owners from `notes()`, at most 500 characters. A long summary is cut first and the action items stay. Use `--name` to change the bot name.

```sh
OPENAI_MODEL=gpt-x OPENAI_API_KEY=... cargo run -p parley-brain --bin ask_say -- --adapter openai meeting.txt "Parley, what did Tom agree to?"
ANTHROPIC_MODEL=... ANTHROPIC_API_KEY=... cargo run -p parley-brain --bin ask_say -- --adapter anthropic meeting.txt "hey parley what is next"
```
