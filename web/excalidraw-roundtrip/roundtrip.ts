// Usage: bun run roundtrip.ts <in.bin> <out.bin> <board.png>
//
// Loads a Yjs update whose root array "elements" holds Excalidraw elements written by Rust.
// Restores them with @excalidraw/excalidraw, checks validity and bindings, moves one node,
// exports a PNG with Excalidraw's own export helper, and writes the full update back.
// Prints a JSON report on stdout. The Rust bin `board_roundtrip` reads it.
import { Window } from "happy-dom";
import { createCanvas } from "@napi-rs/canvas";
import { Resvg } from "@resvg/resvg-js";
import type { ExcalidrawElement } from "@excalidraw/excalidraw/element/types";

/** The fields of an Excalidraw element this check reads. */
type El = {
  id: string;
  type: string;
  x: number;
  y: number;
  width: number;
  height: number;
  groupIds?: string[] | null;
  boundElements?: { id: string; type: string }[] | null;
  containerId?: string | null;
  startBinding?: { elementId: string } | null;
  endBinding?: { elementId: string } | null;
};
type Binding = { id: string; type: string };

// Excalidraw reads browser globals at module load, so it cannot be a static import:
// the globals must exist first.
const win = new Window();
const globals = win as unknown as Record<string, unknown>;
const names = [
  "window", "document", "navigator", "HTMLElement", "HTMLCanvasElement", "HTMLImageElement",
  "Image", "Element", "Node", "Text", "DocumentFragment", "MutationObserver", "ResizeObserver",
  "DOMParser", "XMLSerializer", "getComputedStyle", "requestAnimationFrame",
  "cancelAnimationFrame", "matchMedia", "DOMRect", "Path2D", "Blob", "File", "FileReader",
  "CustomEvent", "Event", "KeyboardEvent", "MouseEvent", "PointerEvent", "FocusEvent",
  "ClipboardEvent", "InputEvent", "WheelEvent", "DragEvent", "ImageData", "CSS",
];
for (const name of names) {
  const value = globals[name];
  if (value !== undefined) {
    try { (globalThis as Record<string, unknown>)[name] = value; } catch {}
  }
}
(globalThis as Record<string, unknown>).devicePixelRatio = 1;
class FontFaceStub {
  constructor(public family: string, public source: unknown, public descriptors?: unknown) {}
  async load() { return this; }
}
(globalThis as Record<string, unknown>).FontFace = FontFaceStub;
// happy-dom has no FontFace; Excalidraw only asks whether a font is loaded.
const documentStub = globals.document as Record<string, unknown>;
documentStub.fonts = { has: () => false, add() {}, delete() {}, ready: Promise.resolve() };
// Do not copy win.eval: it breaks bare global lookup in Bun.
const canvasPrototype = (globals.HTMLCanvasElement as { prototype: Record<string, unknown> }).prototype;
// happy-dom canvases have no 2d context; back each one with a real canvas for measurement.
canvasPrototype.getContext = function (this: Record<string, unknown>, type: string, opts?: unknown) {
  if (type !== "2d") return null;
  if (!this.__canvas) this.__canvas = createCanvas(Number(this.width) || 300, Number(this.height) || 150);
  const backing = this.__canvas as { getContext: (contextId: "2d", options?: unknown) => unknown };
  return backing.getContext("2d", opts);
};

const { restoreElements, exportToSvg } = await import("@excalidraw/excalidraw");
const Y = await import("yjs");

const [inPath, outPath, pngPath] = process.argv.slice(2);
if (!inPath || !outPath || !pngPath) throw new Error("usage: roundtrip.ts <in.bin> <out.bin> <board.png>");
const MOVE = { x: 40, y: 20 };

const ydoc = new Y.Doc();
Y.applyUpdate(ydoc, new Uint8Array(await Bun.file(inPath).arrayBuffer()));
const array = ydoc.getArray("elements");
// The Y.Array holds plain Excalidraw element objects (ContentAny); restore checks their shape.
const raw = structuredClone(array.toArray()) as El[];
// restoreElements has no input/output type for a plain array of objects read from Yjs.
const restored = restoreElements(structuredClone(raw) as unknown as ExcalidrawElement[], null, {
  repairBindings: true,
}) as unknown as El[];

const map = new Map(restored.map((el) => [el.id, el]));
const rects = restored.filter((el) => el.type === "rectangle");
const arrows = restored.filter((el) => el.type === "arrow");
const texts = restored.filter((el) => el.type === "text");
const checks: Record<string, boolean> = {};
const reasons: string[] = [];

/** Used six times so every failed check reports one reason line. */
function check(name: string, ok: boolean, reason: string) {
  checks[name] = ok;
  if (!ok) reasons.push(reason);
}

check(
  "ids",
  restored.length === raw.length &&
    restored.every((el) => typeof el.id === "string" && el.id.length > 0) &&
    new Set(restored.map((el) => el.id)).size === restored.length,
  "restore lost, merged or duplicated an element",
);
check(
  "shapes",
  restored.every((el) =>
    ["rectangle", "arrow", "text"].includes(el.type) &&
    [el.x, el.y, el.width, el.height].every((n) => typeof n === "number" && Number.isFinite(n)) &&
    el.width >= 0 && el.height >= 0
  ),
  "an element has a bad type or geometry",
);

const bound = (el: El | undefined, id: string, type: string) =>
  (el?.boundElements ?? []).some((b: Binding) => b.id === id && b.type === type);
check(
  "arrowsBound",
  arrows.every((arrow) =>
    [arrow.startBinding, arrow.endBinding].every((binding) => {
      const target = binding ? map.get(binding.elementId) : undefined;
      return target?.type === "rectangle" && bound(target, arrow.id, "arrow");
    })
  ),
  "an arrow is not bound to a rectangle both ways",
);

check(
  "textsBound",
  texts.every((text) => {
    if (text.containerId == null) return true;
    const container = map.get(text.containerId);
    return container !== undefined && bound(container, text.id, "text");
  }),
  "a bound text has no container, or the container does not name it",
);

check(
  "noOverlap",
  !rects.some((a, i) =>
    rects.slice(i + 1).some((b) =>
      a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
    )
  ),
  "two node rectangles overlap",
);

const groupIds = [...new Set(restored.flatMap((el) => el.groupIds ?? []))];
check(
  "groups",
  groupIds.length > 0 &&
    groupIds.every((groupId) => {
      const members = rects.filter((el) => (el.groupIds ?? []).includes(groupId));
      return members.length >= 2 && new Set(members.map((m) => m.x)).size === 1;
    }),
  "a group has fewer than two nodes, or its nodes are not in one column",
);

// Move the first node, with its bound text, inside the Y.Array.
const node = rects[0];
const text = texts.find((t) => t.containerId === node.id);
const moved = {
  id: node.id,
  text_id: text?.id,
  from: { x: node.x, y: node.y },
  to: { x: node.x + MOVE.x, y: node.y + MOVE.y },
};
for (const id of [node.id, text?.id]) {
  const index = id ? raw.findIndex((el) => el.id === id) : -1;
  if (index === -1) throw new Error(`element ${id} missing from the Y.Array`);
  const el = raw[index];
  array.delete(index, 1);
  array.insert(index, [{ ...el, x: el.x + MOVE.x, y: el.y + MOVE.y }]);
}

// Same move on the restored copy, then export with Excalidraw's helper.
for (const el of restored) {
  if (el.id === node.id || el.id === text?.id) {
    el.x += MOVE.x;
    el.y += MOVE.y;
  }
}
const svg = await exportToSvg({
  elements: restored as unknown as ExcalidrawElement[],
  appState: { exportBackground: true, viewBackgroundColor: "#ffffff" },
  exportPadding: 40,
  skipInliningFonts: true,
});
const png = new Uint8Array(new Resvg(svg.outerHTML, { fitTo: { mode: "original" } }).render().asPng());
check("png", png.length > 0 && png[0] === 0x89 && png[1] === 0x50, "PNG export is empty or not a PNG");

await Bun.write(outPath, Y.encodeStateAsUpdate(ydoc));
await Bun.write(pngPath, png);

const report = {
  ok: Object.values(checks).every(Boolean),
  checks,
  reasons,
  moved,
  counts: {
    elements: restored.length,
    nodes: rects.length,
    edges: arrows.length,
    groups: groupIds.length,
    png_bytes: png.length,
  },
};
console.log(JSON.stringify(report));
if (!report.ok) process.exit(1);
