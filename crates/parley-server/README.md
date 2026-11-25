# parley-server

One meeting page with a live transcript. The server owns the state (running, paused, removed) and keeps only transcript text in memory. It stores no audio.

- `GET /m/<secret>` serves the page (`web/index.html`). The secret is 128 random bits from the OS, URL-safe base64. A wrong secret gives 404 on every route.
- `GET /m/<secret>/ws` is a WebSocket. It sends one `history` message (state and lines), then `line` and `state` events.
- `GET /m/<secret>/doc/ws` and `/board/ws` sync the live doc and the whiteboard: binary WebSocket frames, full Yjs state first, then raw updates both ways (1 MiB cap, 404 on a wrong secret or after remove). `Meeting::set_board` lays out a board model and replaces the whiteboard for every open page. The page bundle `web/board.js` is built from `web/board` with `bun run build`.
- `GET /m/<secret>/editor.js` and `/board.js` are served gzip-compressed (built once, for clients that send `accept-encoding: gzip`) with `cache-control: private, max-age=3600`. `GET /m/<secret>/fonts/<family>/<file>` serves the Excalidraw fonts embedded from `web/fonts` (the board sets `EXCALIDRAW_ASSET_PATH` to the page URL), so the page calls no other host. `bun run build` in `web/board` also copies the fonts; the CJK family Xiaolai (13 MB) is left out, so CJK text uses a system font. The build points Excalidraw's last-resort font URL at the page origin and fails if `board.js` still names `esm.sh`, so a missing font ends at the server's 404.
- Transferred size (gzip) before and after: `board.js` 8,096,777 B -> 2,546,772 B; `editor.js` 520,407 B -> 162,745 B.
- `POST /m/<secret>/pause`, `/resume` and `/remove` change the state and answer `{"state": ...}`. Pause works only from running, resume only from paused. Remove is final: it drops the transcript and the page says the session was removed.
- `app(meeting)` returns the router and one `mpsc::Sender<Line>`. Every transcript source feeds lines through that channel. A line that arrives while paused or removed is dropped.
- `Meeting::request(llm, name, text)` answers a request line ("Parley, start a doc for the launch plan"). It runs `parley_brain` `write_doc` on a copy of the live doc with the transcript so far, then applies the result as one update, so every open page shows it with no reload. It returns the chat line. A failed model call or a bad tool call returns an error and leaves the doc unchanged. Text that is no request returns `None`.
- `Llm::from_env()` picks the model from `PARLEY_LLM` (`openai` or `anthropic`, then the `parley-llm` variables). There is no default: unset is an error and the doc stays unchanged.

## Run

`page_demo` feeds one line about every 2 seconds from a file of `Speaker: text` lines and prints the page URL.
Set `PAGE_DEMO_ADDR` (default `127.0.0.1:8080`) and `PAGE_DEMO_BASE` (default `http://<addr>`) to change the listen address and the printed base.

```sh
cargo run -p parley-server --bin page_demo -- meeting.txt
```

A stdin line that starts with the bot name is a request. With a local Ollama:

```sh
PARLEY_LLM=openai OPENAI_BASE_URL=http://127.0.0.1:11434/v1 OPENAI_MODEL=qwen2.5:7b \
  cargo run -p parley-server --bin page_demo -- meeting.txt
# then type: Parley, start a doc for the launch plan
```

## Test

`cargo test -p parley-server` checks the secret, the pause, resume and remove states, one line received over a WebSocket, the doc and board sync (two clients and a server edit converge), and a request that creates a doc, extends it, and leaves it unchanged on a failed call (stub model). No network.
