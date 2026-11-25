// Usage: bun run roundtrip.ts <in.bin> <out.bin>
// Loads a Rust-made Yjs update into a headless Tiptap editor (Collaboration, fragment "default"),
// edits it through the editor, writes the full update back, and prints the editor JSON on stdout.
import { Window } from "happy-dom";

const win = new Window();
Object.assign(globalThis, {
  window: win,
  document: win.document,
  navigator: win.navigator,
  Node: win.Node,
  HTMLElement: win.HTMLElement,
  MutationObserver: win.MutationObserver,
  getComputedStyle: win.getComputedStyle.bind(win),
});

const { Editor } = await import("@tiptap/core");
const { default: StarterKit } = await import("@tiptap/starter-kit");
const { default: Collaboration } = await import("@tiptap/extension-collaboration");
const Y = await import("yjs");

const [inPath, outPath] = process.argv.slice(2);
if (!inPath || !outPath) throw new Error("usage: roundtrip.ts <in.bin> <out.bin>");

const ydoc = new Y.Doc();
Y.applyUpdate(ydoc, new Uint8Array(await Bun.file(inPath).arrayBuffer()));

const editor = new Editor({
  element: document.createElement("div"),
  extensions: [StarterKit.configure({ undoRedo: false, trailingNode: false }), Collaboration.configure({ document: ydoc })],
});

const before = editor.getJSON();

// Edit: heading at the top, paragraph with a bold run at the end.
editor.commands.insertContentAt(0, { type: "heading", attrs: { level: 1 }, content: [{ type: "text", text: "Edited in Tiptap" }] });
editor.commands.insertContentAt(editor.state.doc.content.size, {
  type: "paragraph",
  content: [{ type: "text", text: "from the editor", marks: [{ type: "bold" }] }],
});

const after = editor.getJSON();
await Bun.write(outPath, Y.encodeStateAsUpdate(ydoc));
console.log(JSON.stringify({ before, after }));
editor.destroy();
