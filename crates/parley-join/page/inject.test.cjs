const assert = require('node:assert/strict');
const fs = require('node:fs');
const { webcrypto } = require('node:crypto');
const { test } = require('node:test');
const vm = require('node:vm');
const selectors = require('./selectors.json');
const script = new vm.Script(fs.readFileSync(`${__dirname}/inject.js`, 'utf8'));

function meetPage({
  buttons, elements = {}, attached = false, name = 'Test',
  window = new EventTarget(), tick = () => {}, send = () => {},
}) {
  window.__PARLEY_CFG = { ws: 'ws://localhost', role: 'bot', name, attached, selectors };
  window.top = window;
  script.runInNewContext({
    window,
    crypto: webcrypto,
    location: { hostname: 'meet.google.com', href: 'https://meet.google.com/abc-defg-hij' },
    document: {
      body: { innerText: '' },
      querySelector: (selector) => elements[selector] ?? null,
      querySelectorAll: (selector) => selector === 'button' ? buttons : [],
      createElement: () => ({
        getContext: () => ({ fillRect() {}, fillText() {} }),
        captureStream() {},
      }),
    },
    navigator: {},
    WebSocket: class {
      readyState = 1;
      send = send;
    },
    setInterval: tick,
  });
  return window.__parley;
}

function button(textContent) {
  return {
    textContent,
    getAttribute: () => null,
    getClientRects: () => [{}],
    getBoundingClientRect: () => ({ x: 10, y: 20, width: 40, height: 30 }),
    scrollIntoView() {},
  };
}

test('the signed-in lobby exposes a Join now control without a name input', () => {
  const page = meetPage({ buttons: [button('Join now')] });
  assert.equal(page.state().phase, 'prejoin');
  assert.equal(page.state().can_join, true);
  assert.equal(page.point('name'), null);
  const point = page.point('join');
  assert.deepEqual([point.x, point.y], [30, 35]);
});

test('the participant tile and Leave call control identify direct entry', () => {
  const leave = button('');
  const page = meetPage({
    buttons: [leave],
    elements: { '[data-self-name]': {}, 'button[aria-label="Leave call"]': leave },
  });
  assert.equal(page.state().phase, 'in_call');
  assert.equal(page.point('join'), null);
});

test('leaving and returning to a Meet document invalidates an active capture', () => {
  const window = new EventTarget();
  const page = meetPage({ buttons: [], window });
  const before = page.screenshotDocument();
  assert.equal(page.screenshotDocument(), before);

  window.dispatchEvent(new Event('pagehide'));

  const after = page.screenshotDocument();
  assert.notEqual(after, before);
  assert.equal(page.screenshotDocument(), after);
});

test('attached chat reads the host even when the host shares the tile label', () => {
  const messages = [];
  const received = [];
  let scan;
  const page = meetPage({
    buttons: [],
    attached: true,
    name: 'Sam',
    elements: {
      '[data-self-name]': { getAttribute: () => 'Alex' },
      'button[aria-label="Leave call"]': button(''),
      '[jsname="xySENc"]': {
        querySelectorAll: (selector) => selector === '[data-message-id]' ? messages : [],
      },
    },
    tick: (fn, ms) => { if (ms === 1000) scan = fn; },
    send: (raw) => {
      const msg = JSON.parse(raw);
      if (msg.t === 'chat') received.push([msg.sender, msg.text]);
    },
  });
  const message = (sender, id, textContent) => ({
    textContent,
    getAttribute: (attr) => ({ 'data-sender-name': sender, 'data-message-id': id })[attr],
    querySelector: () => null,
  });
  scan();
  messages.push(
    message('Sam', '1', 'Host question'),
    message('Alex', '2', 'Bot reply'),
    message('You', '3', 'Another bot reply'),
  );
  scan();
  assert.deepEqual(received, [['Sam', 'Host question']]);
  assert.equal(page.chatCount('Bot reply'), 1);
});
