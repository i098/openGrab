# Excalidraw round trip

Install the locked dependencies and run the proof from the repository root:

```sh
(cd web/excalidraw-roundtrip && bun install --frozen-lockfile)
cargo run --locked -p parley-artifacts --bin board_roundtrip
```

The Rust binary lays out a sample graph and writes an Excalidraw element array to a `yrs` document. The Bun script restores the elements, checks the bindings and layout, and moves a node and its text. Rust reads the change back and prints `PASS` or `FAIL`. The binary prints the path to the PNG export in the temporary directory.

This proof does not join a meeting or call an LLM.
