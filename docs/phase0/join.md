# Phase 0: joining a Google Meet call

This document records the join part of Phase 0 (plan sections 6 and 6.1).
Every statement comes from a run on one Linux host on 2026-11-13.
Screenshots, logs and recordings stay outside the repository, because they show account names.
No account name, e-mail address or cookie appears here.

## Result

| Join kind | Verdict | Why |
| --- | --- | --- |
| Guest (no Google account) | **No-go, as tested** | Meet refused every guest with "You can't join this video call". The host was in the call each time and no knock reached the host. |
| Signed-in bot | **Not tested yet** | The one sign-in attempt stopped at a Google phone prompt. The live checks come later as a new task. |

Section 6.1 checks that ran: Join (guest) failed. Chat, Tile, Outsider and speakers-in-call did not run, because no run got into a call.
One part of Phase 0 is proven: Chromium profiles can play a known recording as a microphone (F3).

## What the code does

`crates/parley-join` starts a pinned Chromium (not headless, inside Xvfb), injects one page script, and drives the Meet page through a thin CDP client over one WebSocket.
The driver never sends `Runtime.enable` and does not try to hide that it is automated.
`scripts/p0-exit.sh <meet-link>` runs the section 6.1 checks and prints one PASS or FAIL line each.
`--signin-email` signs in once with the password in `GOOGLE_ACCOUNT_PASSWORD`: no retry, no code entry, no recovery flow.
See `crates/parley-join/README.md` for the Chromium version and the install command.

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
- The driver signed in once with `--signin-email` for a Workspace account. Google accepted the password and then showed "Verify it's you ... Google sent a notification to your phone. Open the Gmail app, tap Yes on the prompt" (URL path `/v3/signin/challenge/dp`).
  The driver stopped at once. This prompt reached the owner's phone as a side effect of the password step.
- No Google session existed in the shared cookie store, so none was loaded. No session was pulled from any personal browser.

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
`p0_exit` keeps the profiles in the temp directory and the driver tolerates a late `Page.navigate` reply.

## Reached with no host

Launch, the pinned version, navigation to the Meet link, the injected script, the page-state read, the refusal text and a screenshot.
The guest name screen and the "ask to join" request were reached only by browsers without `Runtime.enable`, and the host never saw a knock.

## Next

The signed-in bot needs a session that Google accepts without a phone prompt.
Then run `scripts/p0-exit.sh` with `--signin-email` for Join, Chat, Tile, Outsider and the three fake-audio speakers.
