// Live whiteboard for the meeting page. Build: `bun install && bun run build` (writes ../board.js and ../board.css).
// Protocol: binary WebSocket frames both ways. Server sends the full state first, then raw Yjs updates.
// The shared doc is a Y.Map "board" with a Y.Array "elements" of plain Excalidraw element objects.
import { createElement } from "react";
import { createRoot } from "react-dom/client";
import { Excalidraw, restoreElements } from "@excalidraw/excalidraw";
import "@excalidraw/excalidraw/index.css";
import * as Y from "yjs";

type El = { id: string; version: number };
type Api = {
  updateScene(s: { elements: unknown[] }): void;
  scrollToContent(target: unknown[], o: { fitToViewport: boolean; maxZoom: number; viewportZoomFactor: number }): void;
};

const base = location.pathname.replace(/\/$/, "");
const ydoc = new Y.Doc();
const shared = ydoc.getMap("board");
let api: Api | undefined;
let ws: WebSocket | undefined;
let known = new Set<string>();

ydoc.on("update", (u: Uint8Array, origin: unknown) => {
  if (origin !== "remote" && ws?.readyState === WebSocket.OPEN) ws.send(u);
});

function array(): Y.Array<El> {
  let a = shared.get("elements") as Y.Array<El> | undefined;
  if (!a) {
    a = new Y.Array<El>();
    shared.set("elements", a);
  }
  return a;
}

// Local edit: write only the elements whose version differs from the shared copy.
function onChange(elements: readonly El[]) {
  const a = array();
  const at = new Map<string, number>();
  a.forEach((e, i) => at.set(e.id, i));
  ydoc.transact(() => {
    for (const el of elements) {
      const i = at.get(el.id);
      const copy = JSON.parse(JSON.stringify(el));
      if (i === undefined) a.push([copy]);
      else if (a.get(i).version !== el.version) {
        a.delete(i, 1);
        a.insert(i, [copy]);
      }
    }
  });
}

// Remote edit: show the shared elements. onChange then finds equal versions and writes nothing.
function show() {
  if (!api) return;
  const elements = restoreElements(array().toJSON(), null);
  api.updateScene({ elements });
  // Bring the content into view when it is new or replaced as a whole (a server edit).
  // A peer adding one shape keeps the old ids, so the view stays where the person put it.
  const ids = new Set<string>(elements.map((e) => e.id));
  const replaced = elements.length > 0 && ![...known].some((id) => ids.has(id));
  known = ids;
  if (replaced) api.scrollToContent(elements, { fitToViewport: true, maxZoom: 1, viewportZoomFactor: 0.8 });
}
shared.observeDeep((_, tx) => {
  if (tx.origin === "remote") show();
});

function connect() {
  if (document.documentElement.dataset.state === "removed") return;
  const s = new WebSocket(`${location.protocol === "https:" ? "wss" : "ws"}://${location.host}${base}/board/ws`);
  ws = s;
  s.binaryType = "arraybuffer";
  // Send local state too, so edits made while offline are not lost.
  s.onopen = () => s.send(Y.encodeStateAsUpdate(ydoc));
  s.onmessage = (e) => Y.applyUpdate(ydoc, new Uint8Array(e.data), "remote");
  s.onclose = () => setTimeout(connect, 2000);
}

createRoot(document.getElementById("board")!).render(
  createElement(Excalidraw, {
    excalidrawAPI: (a: Api) => {
      api = a;
      show();
    },
    onChange: (els: readonly El[]) => onChange(els),
  }),
);
connect();
