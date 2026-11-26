# parley-join

Phase 0 spike: a Rust driver that opens Google Meet and gets audio, chat, roster data, and a camera tile through one page script.
The driver talks to Chromium through a thin CDP client (`src/cdp.rs`) and does not enable the CDP `Runtime`
domain, because Meet refuses a guest whose tab has that domain on (see the findings). The driver does not hide that it is automated.
To keep a sign-in between browser launches, use `--profile <directory>` outside the repository.
Use a new or empty directory, or an existing Chromium profile.
The driver rejects other paths without changing them.
Sign in locally in that profile before running the checks.
The driver keeps the persistent profile at mode `0700` and deletes temporary profiles on exit.
Google cookie import is not supported.

## Attach to a running browser

Use `--attach <port>` with a signed-in, headed Chromium that already runs with `--remote-debugging-port=<port>`.
Use the English Google Meet UI.
The driver opens a new tab and requests closure of only that tab on leave, setup failure, or drop.
It does not send a browser shutdown command.
If the tab is the only page, the driver first opens a blank spare tab, so the browser stays open.
The driver refuses a link that is not a call link (`https://meet.google.com/abc-defg-hij`), so the Meet home page text cannot enter the saved page state.
The attached bot ignores `--profile` and does not need a local Chromium binary or display.
The bot skips guest name entry and can join from the lobby or enter the call directly.
The driver uses the Meet call controls to detect admission.
The join report omits the URL and text for pages outside the `https://meet.google.com` origin.
The driver saves a screenshot only if the Meet document has the same ID before and after capture.
If the tab leaves Meet between the check and the capture, the driver drops the pixels from memory and does not save them.
The driver creates a new owner-only evidence directory for each attached run and prints its path.
To use `--out`, give a new directory path under an existing parent that you trust.

    scripts/p0-exit.sh <meet-link> --attach 9523 --checks join

Two accounts in one browser: add `--host-authuser 0 --bot-authuser 1` (Google account indexes; the driver drops any query or fragment from the link and adds `?authuser=N`) and the exit test
opens a second window as the host, admits the bot, and checks chat in both directions:

    scripts/p0-exit.sh <meet-link> --attach 9523 --checks join,chat,tile,outsider --host-authuser 0 --bot-authuser 1

The host account must own the meeting. In attach mode the page script queues its text messages and its audio frames, and the driver
polls them (`next_event`, every 0.5 s), because Chrome can refuse the page's WebSocket. The `speakers` check still needs a launched Chromium:
attach mode has no fake microphone. `Session::say` plays a WAV into the bot's silent microphone instead.

The attached tab uses Chromium's signed-in profile, not `--name`, for the Meet identity.
Do not use another person's private session.

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
- `scripts/p0-exit.sh` uses Xvfb for runs that start Chromium, including attached runs that start test speakers.
  Install `xvfb-run` and `xauth` for those runs.

## Files

| File | Role |
| --- | --- |
| `src/lib.rs` | Manages the browser session, injects the page script, and tracks the join state |
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

Build and test with `blaze` (`blaze test -p parley-join`). `scripts/p0-exit.sh` uses cargo only, because blaze cannot build `p0_exit`.
Run the page state tests with `node --test crates/parley-join/page/inject.test.cjs`.

## Daily canary

`scripts/canary.sh` runs the exit check (`join,chat,tile,outsider`) on the attached Chromium and appends one `PASS` or `FAIL` line per run to `~/.local/state/parley/canary.log`.
A `FAIL` runs `PARLEY_CANARY_ALERT_CMD` with that line as its only argument, then the script exits 1.
A lost session is a `FAIL` line. The script never signs in. The speakers check does not run.

Install on the host where the signed-in, headed Chromium runs with `--remote-debugging-port=9523`:

```sh
cargo build --release -p parley-join --bin p0_exit
mkdir -p ~/.config/parley ~/.config/systemd/user
sed "s|@REPO@|$PWD|" deploy/systemd/parley-canary.service > ~/.config/systemd/user/parley-canary.service
cp deploy/systemd/parley-canary.timer ~/.config/systemd/user/
```

Write `~/.config/parley/canary.env` outside the repository, mode `0600`:

```sh
PARLEY_CANARY_LINK=https://meet.google.com/abc-defg-hij
PARLEY_CANARY_HOST_AUTHUSER=1
PARLEY_CANARY_BOT_AUTHUSER=0
PARLEY_CANARY_ALERT_CMD=/home/you/.config/parley/canary-alert.sh
```

The alert command is an executable that takes the result line as `$1`. Optional keys: `PARLEY_CANARY_CDP_PORT` (9523), `PARLEY_CANARY_BIN` (`target/release/p0_exit`), `PARLEY_CANARY_TIMEOUT` (seconds, 1800), `PARLEY_CANARY_STATE`.

Start the timer and run it once:

```sh
systemctl --user daemon-reload
systemctl --user enable --now parley-canary.timer
systemctl --user start parley-canary.service
tail -n 1 ~/.local/state/parley/canary.log
```

The timer fires at 06:00 local time. Run `loginctl enable-linger "$USER"` if the user manager must run without a login session.

## Phase 0 findings

See the [Phase 0 run results](../../docs/phase0/join.md).

## Limits

- Attach mode: use one Chromium profile. CDP cannot select a profile, so the new tab opens in the last-used profile.
- Attach mode: if Chromium creates the tab but the CDP reply is lost, the driver closes the one new blank tab it finds. With two or more new blank tabs, it closes none, because it cannot know which is its own.
- Attach mode: the screenshot check and the capture are two CDP commands. CDP has no single command that does both, so a redirect between them can put non-Meet pixels in driver memory. The driver never saves them.
- The driver records Meet's refusal text as the outcome and does not try to hide that it is automated.
- The driver rejects headless Chromium.
