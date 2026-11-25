const assert = require('node:assert/strict');
const fs = require('node:fs');
const { test } = require('node:test');
const vm = require('node:vm');
const selectors = require('./selectors.json');
const script = new vm.Script(fs.readFileSync(`${__dirname}/inject.js`, 'utf8'));

function meetPage({ buttons, elements = {} }) {
  const window = {
    __PARLEY_CFG: { ws: 'ws://localhost', role: 'bot', name: 'Test', selectors },
  };
  window.top = window;
  script.runInNewContext({
    window,
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
    WebSocket: class {},
    setInterval() {},
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
