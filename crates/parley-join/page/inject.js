// Injected into every Meet page by parley-join before the page's own scripts run.
// Thin on purpose (R12): it reads the page and reports; all decisions live in the Rust driver.
// window.__PARLEY_CFG = { ws, role: "bot" | "speaker", name, attached, selectors } is prepended by the driver.
// Selectors and page strings come only from selectors.json (R2).
//
// Parts are ported from Vexa-ai/vexa (Apache-2.0, https://github.com/Vexa-ai/vexa):
//   mixed-capture-core/src/webrtc-audio-hook.ts  -> RTCPeerConnection remote-audio hook
//   gmeet-capture/src/pcm-capture.ts             -> AudioWorklet 16 kHz PCM capture
//   gmeet-capture/src/gmeet-speakers.ts          -> tile and speaking-class reading
//   gmeet-capture/src/gmeet-chat.ts              -> chat panel, message list and composer lookup
//   join/src/googlemeet/{join,admission,refusal}.ts -> lobby state detection
// Not ported: Vexa's humanized pointer input and browser-hiding flags. See ../NOTICE.
(() => {
  if (window.top !== window || window.__parley) return;
  const CFG = window.__PARLEY_CFG;
  const S = CFG.selectors;
  const BLOCK = 1600; // 100 ms of 16 kHz audio per frame

  const norm = (t) => (t || "").replace(/\s+/g, " ").trim();
  const q1 = (sels, root = document) => {
    for (const s of sels) {
      try {
        const e = root.querySelector(s);
        if (e) return e;
      } catch {}
    }
    return null;
  };
  const qa = (sels, root = document) => {
    const out = new Set();
    for (const s of sels) {
      try {
        root.querySelectorAll(s).forEach((e) => out.add(e));
      } catch {}
    }
    return [...out];
  };
  const shown = (e) => !!e && e.getClientRects().length > 0 && e.getBoundingClientRect().width > 0;
  const pageText = () => norm(document.body && document.body.innerText).toLowerCase().replace(/[\u2018\u2019]/g, "'");
  const hasAny = (text, list) => list.some((x) => text.includes(x));

  // ---- link to the driver -------------------------------------------------
  let ws;
  const queue = [];
  const connect = () => {
    ws = new WebSocket(CFG.ws);
    ws.binaryType = "arraybuffer";
    ws.onopen = () => queue.splice(0).forEach((m) => ws.send(m));
    ws.onclose = () => setTimeout(connect, 1000);
  };
  // Attach mode: Chrome can refuse the WebSocket, so text and audio wait here and the driver polls them.
  const audioQueue = [];
  const b64 = (buf) => {
    const u = new Uint8Array(buf);
    let s = "";
    for (let i = 0; i < u.length; i += 0x8000) s += String.fromCharCode(...u.subarray(i, i + 0x8000));
    return btoa(s);
  };
  const send = (m) => {
    if (ws && ws.readyState === 1) ws.send(m);
    else if (CFG.attached && typeof m !== "string") {
      if (audioQueue.length < 3000) audioQueue.push(m); // about 5 minutes of one track
    } else if (queue.length < 500) queue.push(m);
  };
  const sendJson = (o) => send(JSON.stringify(o));
  const log = (msg) => sendJson({ t: "log", msg: String(msg) });
  connect();

  // ---- virtual camera (tile) and microphone ------------------------------
  const canvas = document.createElement("canvas");
  canvas.width = 640;
  canvas.height = 360;
  const g = canvas.getContext("2d");
  let tileState = "listening";
  const draw = () => {
    g.fillStyle = tileState === "paused" ? "#3b2f12" : "#10261c";
    g.fillRect(0, 0, 640, 360);
    g.fillStyle = "#ffffff";
    g.textAlign = "center";
    g.font = "bold 64px sans-serif";
    g.fillText(tileState, 320, 190);
    g.font = "28px sans-serif";
    g.fillText(CFG.name, 320, 260);
  };
  draw();
  setInterval(draw, 500);
  const tileStream = canvas.captureStream(5);

  if (navigator.mediaDevices && navigator.mediaDevices.getUserMedia) {
    const real = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
    navigator.mediaDevices.getUserMedia = async (c) => {
      const out = new MediaStream();
      if (c && c.video) out.addTrack(tileStream.getVideoTracks()[0].clone());
      if (c && c.audio) {
        if (CFG.role === "speaker") {
          (await real({ audio: true })).getAudioTracks().forEach((t) => out.addTrack(t));
        } else {
          const ctx = new AudioContext();
          const dest = ctx.createMediaStreamDestination(); // silent mic
          (window.__parleyMics = window.__parleyMics || []).push({ ctx, dest });
          out.addTrack(dest.stream.getAudioTracks()[0]);
        }
      }
      return out;
    };
  }

  // ---- incoming audio: one 16 kHz PCM stream per remote track ------------
  const WORKLET = `class P extends AudioWorkletProcessor{constructor(){super();this.b=new Float32Array(${BLOCK});this.n=0}
process(i){const c=i[0]&&i[0][0];if(!c)return true;for(let k=0;k<c.length;k++){this.b[this.n++]=c[k];
if(this.n===${BLOCK}){this.port.postMessage(this.b);this.b=new Float32Array(${BLOCK});this.n=0}}return true}}
registerProcessor("parley-pcm",P)`;
  // Frame layout (must match protocol.rs): u8 tag=1, u16 LE track, u64 LE end-time ms, i16 LE samples.
  const frame = (track, f32) => {
    const buf = new ArrayBuffer(11 + f32.length * 2);
    const v = new DataView(buf);
    v.setUint8(0, 1);
    v.setUint16(1, track, true);
    v.setBigUint64(3, BigInt(Date.now()), true);
    for (let i = 0; i < f32.length; i++) {
      const s = Math.max(-1, Math.min(1, f32[i]));
      v.setInt16(11 + 2 * i, s < 0 ? s * 0x8000 : s * 0x7fff, true);
    }
    return buf;
  };
  let trackSeq = 0;
  const seenTracks = new Set();
  const tap = async (track, stream) => {
    const id = ++trackSeq;
    sendJson({ t: "track", id, label: track.label, stream: stream.id });
    // A remote WebRTC stream only delivers samples while a media element plays it. The element stays in the
    // document, or the browser may collect it and stop the stream.
    const el = new Audio();
    el.srcObject = stream;
    el.autoplay = true;
    el.hidden = true;
    document.body.append(el);
    el.play().catch(() => {});
    const ctx = new AudioContext({ sampleRate: 16000 });
    await ctx.resume();
    const src = ctx.createMediaStreamSource(new MediaStream([track]));
    try {
      const url = URL.createObjectURL(new Blob([WORKLET], { type: "application/javascript" }));
      await ctx.audioWorklet.addModule(url);
      URL.revokeObjectURL(url);
      const node = new AudioWorkletNode(ctx, "parley-pcm", { channelCount: 1, channelCountMode: "explicit", outputChannelCount: [1] });
      node.port.onmessage = (e) => send(frame(id, e.data));
      src.connect(node);
      node.connect(ctx.destination);
    } catch (e) {
      log(`worklet failed (${e}); falling back to ScriptProcessor`);
      const sp = ctx.createScriptProcessor(4096, 1, 1);
      sp.onaudioprocess = (e) => send(frame(id, new Float32Array(e.inputBuffer.getChannelData(0))));
      src.connect(sp);
      sp.connect(ctx.destination);
    }
  };
  const Orig = window.RTCPeerConnection;
  if (Orig) {
    const Hooked = function (...a) {
      const pc = new Orig(...a);
      pc.addEventListener("track", (e) => {
        if (e.track.kind !== "audio" || seenTracks.has(e.track.id)) return;
        seenTracks.add(e.track.id);
        tap(e.track, e.streams[0] || new MediaStream([e.track])).catch((x) => log(`tap failed: ${x}`));
      });
      return pc;
    };
    Hooked.prototype = Orig.prototype;
    Object.setPrototypeOf(Hooked, Orig);
    window.RTCPeerConnection = Hooked;
  }

  // A page that got this script after Meet had made its connections (a chosen `authuser=` account) never fires the
  // hook above for them, so also tap the remote streams that Meet's own media elements already play.
  setInterval(() => {
    for (const el of document.querySelectorAll("audio, video")) {
      const s = el.srcObject;
      if (!(s instanceof MediaStream)) continue;
      for (const t of s.getAudioTracks()) {
        if (t.readyState !== "live" || seenTracks.has(t.id)) continue;
        seenTracks.add(t.id);
        tap(t, s).catch((x) => log(`tap failed: ${x}`));
      }
    }
  }, 1000);

  // ---- roster and speaking indicators ------------------------------------
  const tileName = (el) => {
    const n = q1(S.tileName, el);
    const t = norm(n && (n.getAttribute("data-self-name") || n.textContent));
    return t && t.length < 80 ? t : null;
  };
  const readTiles = () => {
    const seen = new Set();
    const out = [];
    for (const el of qa(S.participantTile)) {
      const id = el.getAttribute("data-participant-id") || "";
      if (!id || seen.has(id)) continue;
      seen.add(id);
      const attrs = {};
      for (const a of el.attributes) if (a.name !== "style") attrs[a.name] = a.value.slice(0, 200);
      const img = el.querySelector("img[src]");
      out.push({
        id,
        name: tileName(el),
        is_self: !!(el.hasAttribute("data-self-name") || q1(S.selfMarker, el)),
        speaking: S.speakingClass.some((c) => el.classList.contains(c) || el.querySelector("." + CSS.escape(c))),
        avatar: img ? img.getAttribute("src") : null,
        attrs,
        text: norm(el.innerText).slice(0, 200),
        html: el.outerHTML.replace(/ style="[^"]*"/g, "").slice(0, 3000),
      });
    }
    return out;
  };
  let lastRoster = "";
  let lastSpeaking = "";
  setInterval(() => {
    const tiles = readTiles();
    const roster = JSON.stringify(tiles.map(({ speaking, ...rest }) => rest));
    if (roster !== lastRoster) {
      lastRoster = roster;
      sendJson({ t: "roster", participants: tiles });
    }
    const names = tiles.filter((x) => x.speaking && !x.is_self && x.name).map((x) => x.name).sort();
    if (JSON.stringify(names) !== lastSpeaking) {
      lastSpeaking = JSON.stringify(names);
      sendJson({ t: "speaking", names, at: Date.now() });
    }
  }, 250);

  // ---- chat ---------------------------------------------------------------
  const chatSeen = new Set();
  const sentLines = new Set();
  let chatPrimed = false;
  const textOf = (n) => norm(n && n.textContent);
  const senderOf = (node) => {
    for (let cur = node, i = 0; cur && i < 5; cur = cur.parentElement, i++) {
      const a = norm(cur.getAttribute("data-sender-name"));
      if (a) return a;
      if (cur !== node) {
        const t = textOf(q1(S.chatSender, cur));
        if (t) return t;
      }
    }
    return "";
  };
  const chatNodes = () => {
    const root = q1(S.chatContainer) || document;
    for (const s of S.chatMessage) {
      const n = [...root.querySelectorAll(s)];
      if (n.length) return n;
    }
    return [];
  };
  const chatPanelOpen = () => {
    // Meet (Chrome 159 era) sets no aria-pressed on the toggle, so the visible message list is the signal.
    const t = q1(S.chatToggle);
    return (!!t && t.getAttribute("aria-pressed") === "true") || shown(q1(S.chatContainer));
  };
  const scanChat = () => {
    const self = CFG.attached ? q1(S.selfMarker) : null;
    const ownName = CFG.attached
      ? norm(self && (self.getAttribute("data-self-name") || self.textContent))
      : CFG.name;
    for (const node of chatNodes()) {
      const text = textOf(q1(S.chatText, node) || node);
      if (!text) continue;
      const sender = senderOf(node);
      const id = node.getAttribute("data-message-id") || `${sender}\0${text}`;
      if (chatSeen.has(id)) continue;
      chatSeen.add(id);
      if (!chatPrimed) continue; // what is on screen when the panel first opens is older than the bot
      // The host and the bot can be one person with one display name, so the own name only counts for lines the driver posted.
      if (!sender || sender.toLowerCase() === "you" || (sender === ownName && sentLines.has(text))) continue;
      sendJson({ t: "chat", id, sender, text });
    }
  };
  setInterval(() => {
    if (pageState().phase !== "in_call") return;
    if (!chatPanelOpen()) return; // the driver opens the panel with a trusted click
    scanChat();
    if (!chatPrimed) {
      chatPrimed = true;
      log(`chat primed (${chatSeen.size} older lines)`);
    }
  }, 1000);

  // ---- state and click targets -------------------------------------------
  const pageState = () => {
    const t = pageText();
    const base = { url: location.href, text: t.slice(0, 400) };
    if (hasAny(t, S.refusedText)) return { ...base, phase: "refused" };
    if (hasAny(t, S.expiredText)) return { ...base, phase: "expired" };
    if (hasAny(t, S.deniedText)) return { ...base, phase: "denied" };
    if (hasAny(t, S.notFoundText)) return { ...base, phase: "not_found" };
    if (hasAny(t, S.endedText)) return { ...base, phase: "ended" };
    if (location.hostname !== "meet.google.com") return { ...base, phase: "offsite" };
    const waiting = hasAny(t, S.waitingText);
    if (!waiting && (q1(S.selfMarker) || q1(S.participantTile)) && shown(q1(S.leaveButton))) return { ...base, phase: "in_call" };
    if (waiting) return { ...base, phase: "waiting" };
    if (shown(q1(S.nameInput)) || joinButton()) return { ...base, phase: "prejoin", can_join: !!joinButton() };
    return { ...base, phase: "loading" };
  };
  const joinButton = () =>
    [...document.querySelectorAll("button")].find(
      (b) => shown(b) && !b.disabled && b.getAttribute("aria-disabled") !== "true" && S.joinButtonText.includes(norm(b.textContent).toLowerCase()),
    );
  // "Admit" is a button for a normal guest and the menu item "Admit entry" for a guest flagged as a risk.
  const admitButton = () =>
    [...document.querySelectorAll("button, [role=menuitem], li")].find(
      (b) => shown(b) && !b.disabled && S.admitButtonText.some((t) => norm(b.textContent).toLowerCase().endsWith(t)),
    );
  const admitMore = () => {
    const deny = q1(S.admitDenyButton);
    const row = deny && deny.parentElement && deny.parentElement.parentElement && deny.parentElement.parentElement.parentElement;
    return row && q1(S.admitMoreButton, row);
  };
  // The host sees "N guest waiting" as a button that opens the popover with "Admit".
  let lastBadge = 0;
  const admitBadge = () =>
    [...document.querySelectorAll("[role=button]")].find((b) => shown(b) && S.admitBadgeText.some((t) => norm(b.textContent).toLowerCase().includes(t)));
  const centre = (el) => {
    if (!shown(el)) return null;
    el.scrollIntoView({ block: "center", inline: "center" });
    const r = el.getBoundingClientRect();
    return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
  };
  const sendButton = () => {
    const composer = q1(S.chatComposer);
    for (let cur = composer && composer.parentElement, i = 0; cur && i < 5; cur = cur.parentElement, i++) {
      const bs = [...cur.querySelectorAll("button, [role=button]")];
      const hit = bs.find((b) => [...b.querySelectorAll("i")].some((ic) => S.chatSendIcon.includes(norm(ic.textContent).toLowerCase())));
      if (hit) return hit;
      if (bs.length === 1) return bs[0];
      if (bs.length) return null;
    }
    return null;
  };
  const muteState = (btn) => {
    if (!btn) return null;
    const m = btn.getAttribute("data-is-muted");
    if (m) return m === "false";
    return /^turn off/i.test(btn.getAttribute("aria-label") || "");
  };
  const chatLines = () => chatNodes().map((n) => textOf(q1(S.chatText, n) || n));
  let screenshotDocument;

  window.__parley = {
    state: pageState,
    screenshotDocument: () => {
      if (!screenshotDocument) {
        screenshotDocument = crypto.randomUUID();
        window.addEventListener("pagehide", () => { screenshotDocument = null; }, { capture: true, once: true });
      }
      return screenshotDocument;
    },
    // The driver clicks these points with real CDP mouse events; Meet ignores script clicks on some controls.
    point: (kind) => {
      const el = {
        name: () => q1(S.nameInput),
        join: joinButton,
        // The badge toggles a popover: click it at most every 5 s so a click cannot close the popover it opened.
        admit: () => admitButton() || (Date.now() - lastBadge > 5000 && (lastBadge = Date.now(), admitMore() || admitBadge())),
        mic: () => q1(S.micButton),
        cam: () => q1(S.camButton),
        leave: () => q1(S.leaveButton),
        chatToggle: () => q1(S.chatToggle),
        chatComposer: () => q1(S.chatComposer),
        chatSend: sendButton,
      }[kind]();
      return centre(el);
    },
    media: () => ({ mic: muteState(q1(S.micButton)), cam: muteState(q1(S.camButton)) }),
    // Text messages the page could not send over the WebSocket; the driver polls them in attach mode.
    takeQueue: () => queue.splice(0).filter((m) => typeof m === "string"),
    // Audio frames the page could not send over the WebSocket, as base64; the driver polls them in attach mode.
    takeAudio: () => audioQueue.splice(0).map(b64),
    // Play a WAV (array of bytes) into the silent microphone: attach mode has no fake microphone. Returns seconds.
    say: async (bytes) => {
      // Play into every microphone the page made: Meet may be sending any of them.
      const mics = window.__parleyMics || [];
      if (!mics.length) throw new Error("no microphone yet");
      let seconds = 0;
      for (const m of mics) {
        const buf = await m.ctx.decodeAudioData(new Uint8Array(bytes).buffer);
        await m.ctx.resume();
        const src = m.ctx.createBufferSource();
        src.buffer = buf;
        src.connect(m.dest);
        src.start();
        seconds = buf.duration;
      }
      return seconds;
    },
    chatOpen: chatPanelOpen,
    chatReady: () => chatPanelOpen() && !!q1(S.chatComposer),
    noteSent: (text) => sentLines.add(norm(text)),
    chatCount: (text) => chatLines().filter((l) => l === norm(text)).length,
    setTile: (s) => {
      tileState = s;
      draw();
    },
    tileImage: () => canvas.toDataURL("image/png"),
    roster: readTiles,
  };
})();
