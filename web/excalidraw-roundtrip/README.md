# Excalidraw round trip

Install the locked dependencies and run the proof from the repository root:

```sh
(cd web/excalidraw-roundtrip && bun install --frozen-lockfile)
cargo run --locked -p parley-artifacts --bin board_roundtrip
```

The Rust binary computes a layout for a sample graph.
The Bun script restores the scene with Excalidraw and moves Store, its label, and the endpoints of its incoming and outgoing arrows.
The script checks the edited scene and exports an SVG with Excalidraw.
Resvg uses a packaged font instead of system fonts.
Before PNG export, the script checks for at least one glyph outline in each non-blank label line.
The Bun script writes the checked scene back as a Yjs update.
Rust reads the update back, checks the fixture labels and the expected move, and prints `PASS` or `FAIL`.
The binary prints the path to `board.png` in the temporary directory.

This proof does not join a meeting, call an LLM, or use a paid API.
