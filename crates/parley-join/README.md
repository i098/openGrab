# parley-join

Phase 0 spike: a Rust driver that starts a pinned Chromium, opens a Google Meet
link as a guest, and gets audio, chat, roster and a camera tile through one injected page script.
The driver talks to Chromium through a thin CDP client (`src/cdp.rs`) and does not enable the CDP `Runtime`
domain, because Meet refuses a guest whose tab has that domain on (see the findings). The driver does not hide that it is automated.
The signed-in join is not part of this crate: how a signed-in session gets loaded is not decided yet.

## Pinned Chromium

- Build: Google Chrome for Testing **157.0.8103.78** (linux64).
- Install path (outside the repo): `~/.local/share/opengrab/chromium/chrome/linux-157.0.8103.78/chrome-linux64/chrome`
- Install command (no `sudo`):

  ```sh
  mkdir -p ~/.local/share/opengrab/chromium && cd ~/.local/share/opengrab/chromium \
    && bunx @puppeteer/browsers install chrome@157.0.8103.78
  ```

  (`chrome@stable` resolved to this build on 2026-11-13. Pin the number, not the channel.)
- The driver uses the newest build under that directory, or `--chrome <path>` / `PARLEY_CHROME`.
- The browser is never headless. `scripts/p0-exit.sh` runs it inside Xvfb (plan risk R1).
  It needs `xvfb-run` and `xauth`.

## Files

| File | Role |
| --- | --- |
| `src/lib.rs` | Starts Chromium, injects the page script, runs the guest join state machine |
| `src/cdp.rs` | Thin CDP client over one WebSocket: only the commands the driver sends, never `Runtime.enable` |
| `page/selectors.json` | Every Meet selector and page text, in one file (plan risk R2) |
| `page/inject.js` | Page script: 16 kHz PCM frames, chat, roster, speaking, tile canvas |
| `src/protocol.rs` | Binary and JSON frames from the page to the driver (tested) |
| `src/analysis.rs` | Audio levels, speaking-indicator match, WAV output (tested) |
| `src/bin/p0_exit.rs` | The exit test |
| `NOTICE` | Vexa attribution (Apache-2.0) |

## Exit test

```sh
scripts/make-speakers.sh /tmp/speakers   # three known recordings (espeak-ng, ffmpeg)
scripts/p0-exit.sh 'https://meet.google.com/abc-defg-hij' --out /tmp/p0 --speakers /tmp/speakers
```

The host admits every guest. The script prints one `PASS` or `FAIL` line per check: `join`,
`chat` (10 host messages, one reply for each), `tile` (two states), `outsider` (writes
`participants.json`, the per-participant fields the page gives the bot), `speakers` (three more
Chromium profiles, each with `--use-file-for-fake-audio-capture`).

Build and test with `blaze` (`blaze test -p parley-join`). `scripts/p0-exit.sh` falls back to cargo only when blaze is not installed.

## Phase 0 findings

See `docs/phase0/join.md`: the guest join is a no-go as tested (Meet refuses every guest), the signed-in join is not tested yet, and `--use-file-for-fake-audio-capture` works.

## Limits

- The driver records Meet's refusal text as the outcome and does not try to hide that it is automated.
- Every launch fails when the browser product string or `navigator.userAgent` says headless.
