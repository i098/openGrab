# Phase 0: joining a Google Meet call

This document records the join part of Phase 0 (plan sections 6 and 6.1).
The results below come from runs on one Linux host on 2026-11-13.
Screenshots, logs and recordings stay outside the repository, because they show account names.
No account name, e-mail address or cookie appears here.

## Result

| Join kind | Verdict | Why |
| --- | --- | --- |
| Guest (no Google account) | **No-go, as tested** | Meet refused every guest with "You can't join this video call". The host was in the call each time and no knock reached the host. |
| Signed-in bot | **Join, Chat, Tile, Outsider pass; speakers not run** | Run on 2026-11-25 in attach mode, see F6. The speaker check is a stated gap, see F7. |

Section 6.1 checks that ran as a guest (2026-11-13): Join failed. Chat, Tile, Outsider and speakers-in-call did not run, because no run got into a call.
Section 6.1 checks that ran with signed-in accounts (2026-11-25): Join, Chat, Tile and Outsider pass. Speakers-in-call did not run (F7).
One part of Phase 0 is proven: Chromium profiles can play a known recording as a microphone (F3).

## Current usage

See the [driver README](../../crates/parley-join/README.md) for browser setup and Phase 0 check commands.

## F1. The guest refusal does not depend on the driver

All runs were headed (a real window on Xvfb or a screen, no `--headless`, no `--ozone-platform=headless`, and no "HeadlessChrome" in the user agent).
The host was in the call, with the meeting access type Trusted and later Open.
The organization setting for who may join was read: it allows users who are not signed in.

| Browser and driver | Result |
| --- | --- |
| Chrome for Testing 157.0.8103.78, driver on `chromiumoxide` (sends `Runtime.enable`) | Refused at page load, before the name screen. |
| Same Chrome, driver on a thin CDP client without `Runtime.enable` | Refused at page load. |
| Playwright Chromium 156, no `--enable-automation`, no default args, no injected script, no fake-media flags (one run each) | Refused. |
| Clean browser context on a Linux headed Chrome 156 over CDP, name typed, "Ask to join" clicked, `Page`/`Network` domains only | Refused after the click. |
| The same control in the owner's normal browser on a Mac (residential IP) | Refused for 14 polls over 40 s. |
| Headed Chrome 157 with no DevTools socket at all, input by XTest only, host in the call, access Open | Refused after "Join now". |

A CDP control on one headed Chrome showed a difference by domain: with only `Runtime.enable` and a navigation the refusal came before the name screen; without it the name screen showed.
So `Runtime.enable` moves the refusal earlier, but removing it does not let the guest in.

Not isolated: the IP range (the Linux runs use a datacenter or proxy address; the Mac run used a CDP-attached browser on a residential address).
Not tested: a human-driven guest join in a browser with no automation of any kind.
Possible causes, none proven: a Google check on guests ("System info will be sent to confirm you're not a bot"), or a rule on guests for these accounts.

## F2. Google sign-in

- A headless browser on a shared tier was rejected right after the account click: "Couldn't sign you in. This browser or app may not be secure."
- A headed browser on another shared tier reached the password step. One password attempt gave no result. No retry.
- A one-off driver sign-in (since removed from the crate) used a Workspace account. Google accepted the password and then showed "Verify it's you ... Google sent a notification to your phone. Open the Gmail app, tap Yes on the prompt" (URL path `/v3/signin/challenge/dp`).
  The driver stopped at once. This prompt reached the owner's phone as a side effect of the password step.
- No Google session existed in the shared cookie store, so none was loaded. No session was pulled from any personal browser in these attempts (F5 records a later cookie-copy attempt).

## F3. `--use-file-for-fake-audio-capture` works (plan row "not tested")

Chrome 157.0.8103.78, three Chromium profiles at once in one Xvfb, each started with
`--use-fake-ui-for-media-stream --use-fake-device-for-media-stream --use-file-for-fake-audio-capture=<n>.wav`.
Each page recorded its microphone for 13 s with `getUserMedia` and `MediaRecorder`.
The 10 ms speech-envelope correlation of each recording against each reference:

| recording \ reference | a | b | c |
| --- | --- | --- | --- |
| a | 0.965 | 0.140 | 0.171 |
| b | 0.143 | 0.958 | 0.205 |
| c | 0.173 | 0.201 | 0.992 |

Each profile plays its own recording as its microphone, and the file loops.
Not proven: that Meet shows each profile as a separate speaker with a speaking indicator. That needs a join.

## F4. A Chromium profile on a slow disk stalls Meet page loads

With `--user-data-dir` on a busy disk (89% full, load average about 95) the first navigation timed out.
On tmpfs it took 3 to 7 s.
`p0_exit` keeps temporary profiles in the temp directory and the driver tolerates a late `Page.navigate` reply. A `--profile` directory stays where the operator put it.

## Reached with no host

Launch, the pinned version, navigation to the Meet link, the injected script, the page-state read, the refusal text and a screenshot.
The guest name screen and the "ask to join" request were reached only by browsers without `Runtime.enable`, and the host never saw a knock.

## F5. Historical cookie-import attempt

The following results describe the removed cookie-import implementation.
Google session copying is no longer permitted or supported.

Measured on 2026-11-13:

| Step | Result |
| --- | --- |
| `Storage.setCookies` on the browser target | No reply for 90 s, twice, while the host disk was 94% full and the load average above 90. |
| `Network.setCookies` on the tab session | Works: 120 Google cookies sent, 32 read back for the two Meet and sign-in hosts. |
| `accounts.google.com/ListAccounts` from that Chromium | HTTP 200, same account list as the source browser. |
| `https://mail.google.com/` from that Chromium | Redirects to the sign-in account chooser. |
| The same request with the same jar through `curl` | Redirects to `accounts.google.com/CookieMismatch`. |
| `https://meet.google.com/` from that Chromium | Redirects to the Meet marketing page. |

The source browser later showed every account as signed out. These observations do not establish
when or why its sessions ended. An account list alone does not prove that a session is usable.

## F6. Signed-in run, two accounts in one browser (2026-11-25)

Setup: one shared headed Chromium 159.0.8154.6 with CDP on a local port, two signed-in Google accounts: host account A (one domain) and account C (another domain).
The run signs in and out of nothing and uses no new profile.
`p0_exit --attach <port> --host-authuser 0 --bot-authuser 1` opens two new windows in that browser and closes both at the end.
The host window (`?authuser=0`) joins the call as the meeting owner and admits the bot.
The bot window (`?authuser=1`, the other domain) is the participant under test.

| Check | Result |
| --- | --- |
| Join | PASS. The bot knocked, the host admitted it, and the bot reached the call. |
| Chat | PASS. The host posted 10 lines and the bot read 10. The bot posted 10 replies and the host read 10. |
| Tile | PASS. Both tile states were set and drawn. A person must still look at the call. |
| Outsider | PASS in 3 runs in 3 calls. The host view lists the other-domain participant with its id, display name and avatar URL. Meet gives no e-mail address or domain in the tile. |

Findings that the run needed code changes for:

- Meet no longer sets `aria-pressed` on the chat toggle. The driver now treats a visible message list as an open panel. Before this, the driver toggled the panel shut on every post.
- Chrome refuses the page's WebSocket to the driver in this attached browser (the page gets an error and no CSP report). In attach mode the driver now takes the page's queued text messages with `Runtime.evaluate`. Audio frames need the WebSocket and do not arrive in attach mode.
- A Meet page with `?authuser=N` never committed when the page script ran at document start (the tab stayed on `about:blank`). For such links the driver now injects the script after the page loads.
- The host and the bot can have the same display name, so the page script no longer skips a message only because the sender name equals its own name. It skips the lines the driver posted itself and Meet's "You".
- The host of a guest flagged "With potential risks" sees only "Deny" and a "More actions" menu. The menu item "Admit entry" admits the guest. For a normal guest the host sees the "N guest waiting" button, then an "Admit" button.
- The driver opens each attached tab in a new window. A background tab in the same window is hidden, and Meet shows no waiting-guest controls to a hidden host tab.
- Meet may not mark the viewer's own tile. The Outsider check reads the host view (the bot view without a host tab) and subtracts one tile (the viewer) only when no tile is marked as the viewer's own.

## F7. Speakers-in-call: gap

The speaker check (three guests with `--use-file-for-fake-audio-capture`, F3) did not run with signed-in accounts.
It needs three more Chromium processes with their own `--user-data-dir`, because the fake-audio flag is a launch flag and cannot change in the shared browser.
A new profile has no Google session, and these profiles would be guests, which Meet refuses (F1). Signing a new profile in is not allowed on this host.
Also, audio frames do not reach the driver in attach mode (F6), so the recorded audio part of the check cannot run there either.
Not tested: whether Meet lights one speaking indicator per fake-audio participant. The method that works for the audio itself is F3.
To close this gap, run the speaker check where a signed-in profile for each speaker can be made, or find a way to read the speaking indicator without a new profile.
