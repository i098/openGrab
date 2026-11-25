// Live doc editor for the meeting page. Build: `bun install && bun run build` (writes ../editor.js).
// Protocol: binary WebSocket frames both ways. Server sends the full state first, then raw Yjs updates.
import { Editor } from "@tiptap/core";
import StarterKit from "@tiptap/starter-kit";
import Collaboration from "@tiptap/extension-collaboration";
import * as Y from "yjs";

const base = location.pathname.replace(/\/$/, "");
const ydoc = new Y.Doc();
new Editor({
  element: document.getElementById("doc")!,
  extensions: [StarterKit.configure({ undoRedo: false, trailingNode: false }), Collaboration.configure({ document: ydoc })],
});

let ws: WebSocket | undefined;
ydoc.on("update", (u: Uint8Array, origin: unknown) => {
  if (origin !== "remote" && ws?.readyState === WebSocket.OPEN) ws.send(u);
});

function connect() {
  if (document.documentElement.dataset.state === "removed") return;
  const s = new WebSocket(`${location.protocol === "https:" ? "wss" : "ws"}://${location.host}${base}/doc/ws`);
  ws = s;
  s.binaryType = "arraybuffer";
  // Send local state too, so edits made while offline are not lost.
  s.onopen = () => s.send(Y.encodeStateAsUpdate(ydoc));
  s.onmessage = (e) => Y.applyUpdate(ydoc, new Uint8Array(e.data), "remote");
  s.onclose = () => setTimeout(connect, 2000);
}
connect();
