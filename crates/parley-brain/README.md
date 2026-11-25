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

## Write a doc

`Transcript::write_doc(llm, request, &artifact)` runs the model with two tools: `doc_create` (title, becomes a level-1 heading; only on an empty doc) and `doc_append` (one block: `paragraph` with `text`, `heading` with `level` 1-6 and `text`, or `bullets` with `items`). The model sees the current doc, so it can extend a doc across turns: on a doc with content `doc_append` is valid first and `doc_create` is refused. It applies the calls in order to the `parley_artifacts::Artifact` and returns the chat line to post (at most 500 characters). The caller posts the "Listening" state and the doc link.
A malformed call (unknown tool, `doc_append` first on an empty doc, `doc_create` on a doc with content or a second `doc_create`, bad level, empty text) or no call is an error, and the artifact is unchanged: every call is checked before the first is applied.

`doc_say` prints the chat line and the document JSON, with the same env variables as `ask_say`:

```sh
cargo run -p parley-brain --bin doc_say -- --adapter anthropic empty.txt "Parley, start a doc for the launch plan"
```

## Draw a board

`Transcript::write_board(llm, request, &board_doc)` runs the model with one tool, `board_create`: `nodes` (`id`, `label`), `edges` (`from`, `to`, optional `label` of at most 16 characters) and optional `groups` (`id`, `label`, `members`). The model never sends coordinates: `parley_artifacts::board` computes the layout and writes the Excalidraw elements to the `BoardDoc`. It returns the chat line to post (at most 500 characters).
A reply with no tool call is `BrainError::NoToolCall`. Any other rejected call is a `BrainError::BadBoardCall`. In both cases the board is unchanged. Rejected: more than one call, an unknown tool, an empty board, more than 30 nodes, duplicate ids, blank labels, an edge to an unknown node, or a board that already has content (editing is not supported).

`board_say` prints the chat line and the board elements, with the same env variables as `ask_say`:

```sh
cargo run -p parley-brain --bin board_say -- --adapter openai empty.txt "Parley, draw the signup flow"
```
