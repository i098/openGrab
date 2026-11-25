# Excalidraw round trip

Install the locked dependencies and run the proof from the repository root:

```sh
(cd web/excalidraw-roundtrip && bun install --frozen-lockfile)
cargo run --locked -p parley-artifacts --bin board_roundtrip
```

The Rust binary lays out a sample graph and writes the Excalidraw elements to the `elements` array of the `board` map in a `yrs` document. The Bun script restores the elements, moves a node with its text and arrow ends, checks that edited scene, and writes it back. Rust reads the change back and prints `PASS` or `FAIL`. The binary prints the path to the PNG export in the temporary directory.

This proof does not join a meeting or call an LLM.
