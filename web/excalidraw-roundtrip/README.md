# Excalidraw round trip

Install the locked dependencies and run the proof from the repository root:

```sh
(cd web/excalidraw-roundtrip && bun install --frozen-lockfile)
cargo run --locked -p parley-artifacts --bin board_roundtrip
```

The Rust binary lays out a sample graph and writes the Excalidraw elements to the `elements` array of the `board` map.
The Bun script restores the elements and moves a node, its label, and its arrow endpoints.
The script validates the scene and renders its labels with a packaged font, without system fonts.
Only a scene that passes the checks produces an update and a PNG.
Rust reads the update back and prints `PASS` or `FAIL`.
The binary prints the path to the PNG in the temporary directory.

This proof does not join a meeting or call an LLM.
