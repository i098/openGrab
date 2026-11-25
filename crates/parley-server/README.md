# parley-server

One meeting page with a live transcript. The server owns the state (running, paused, removed) and keeps only transcript text in memory. It stores no audio.

- `GET /m/<secret>` serves the page (`web/index.html`). The secret is 128 random bits from the OS, URL-safe base64. A wrong secret gives 404 on every route.
- `GET /m/<secret>/ws` is a WebSocket. It sends one `history` message (state and lines), then `line` and `state` events.
- `GET /m/<secret>/doc/ws` and `/board/ws` sync the live doc and the whiteboard: binary WebSocket frames, full Yjs state first, then raw updates both ways (1 MiB cap, 404 on a wrong secret or after remove). `Meeting::set_board` lays out a board model and replaces the whiteboard for every open page. The page bundle `web/board.js` is built from `web/board` with `bun run build`.
- `POST /m/<secret>/pause`, `/resume` and `/remove` change the state and answer `{"state": ...}`. Pause works only from running, resume only from paused. Remove is final: it drops the transcript and the page says the session was removed.
- `app(meeting)` returns the router and one `mpsc::Sender<Line>`. Every transcript source feeds lines through that channel. A line that arrives while paused or removed is dropped.

## Run

`page_demo` feeds one line about every 2 seconds from a file of `Speaker: text` lines and prints the page URL.
Set `PAGE_DEMO_ADDR` (default `127.0.0.1:8080`) and `PAGE_DEMO_BASE` (default `http://<addr>`) to change the listen address and the printed base.

```sh
cargo run -p parley-server --bin page_demo -- meeting.txt
```

## Test

`cargo test -p parley-server` checks the secret, the pause, resume and remove states, one line received over a WebSocket, and the doc and board sync (two clients and a server edit converge). No network.
