# parley-bot

The `parley` binary. It joins a Google Meet call and connects the parts of the product:

- `parley-join` joins the call through a running, signed-in Chromium (attach mode) and reads and posts Meet chat.
- `parley-server` serves the meeting page. The bot posts one notice line with the page link in the Meet chat.
- `parley-stt` turns call audio into transcript lines. Each line goes to the page. No audio is stored.
- A Meet chat line that starts with the wake name is a request (`parley-brain` `find_request`):
  - a recap or summary request gets the recap in the Meet chat;
  - a request that starts with a writing verb ("start a doc for the launch plan", "add an agenda") changes the live doc through `Meeting::request`, and the chat line carries the page link;
  - anything else is a question, answered from the transcript in the Meet chat.
- Pause, resume and remove on the page stop and start the transcript (the bot tile shows `paused` or `listening`). Remove makes the bot post a goodbye line and leave. Ctrl-C leaves too.

Not here: voice wake word, calendar, sign-in for the page, storage, board requests.

## Run

Needs a headed Chromium that runs with `--remote-debugging-port=<port>`, signed in, with the English Meet UI
(see the [driver README](../parley-join/README.md)), the speech models from `scripts/fetch-phase0-data.sh`, and one model.
The bot opens one tab in that browser and closes it on leave.

```sh
PARLEY_LLM=openai OPENAI_BASE_URL=http://127.0.0.1:11434/v1 OPENAI_MODEL=qwen2.5-7b-t8 \
  cargo run --release -p parley-bot -- 'https://meet.google.com/abc-defg-hij' --attach 9523 --bot-authuser 1
```

It prints `meeting page: <url>`. The URL holds a random secret; anyone who has it can read the page.
With no `--bot-authuser` the bot joins as the browser's default account. A signed-in bot goes through the lobby until the host admits it.

| Setting | Flag | Variable | Default |
| --- | --- | --- | --- |
| Wake name | `--wake` | `PARLEY_WAKE` | `Parley` |
| Page listen address | `--addr` | `PARLEY_ADDR` | `127.0.0.1:8080` |
| Base of the posted page link | `--base-url` | `PARLEY_BASE_URL` | `http://<addr>` |
| Speech models directory | `--data` | `PARLEY_DATA` | `~/.cache/parley-phase0` |
| Speech engine | `--engine parakeet\|whisper` | | `parakeet` |
| Speech threads | `--threads` | | `8` |
| Browser debugging port | `--attach` | `PARLEY_ATTACH` | required |
| Seconds to wait for admission | `--join-timeout` | | `300` |
| Bot Google account index | `--bot-authuser` | `PARLEY_BOT_AUTHUSER` | the browser default |

The model comes from `PARLEY_LLM` (`openai` or `anthropic`) and the `parley-llm` variables. With none set the bot still writes the transcript; every request gets "I could not answer that."

## Limits

- Speaker names: a line gets the name of the one person whom Meet showed as speaking in the last 8 seconds, else `Speaker`.
- One model call at a time. A request that arrives while paused is ignored.
- The bot does not notice that the call ended by itself: stop it with Remove or Ctrl-C.
- Attach mode reads audio and chat by polling the page every 0.5 s.

## Live check

`live_check` runs the whole product on a real call with two signed-in accounts in one browser (the setup of `p0_exit --host-authuser`).
It opens the host tab, starts `parley`, admits it, and prints one `PASS` or `FAIL` line each for the notice, a transcript line from audio, a chat answer, a live doc change in an open page tab, and Remove.
The host plays a recorded WAV into the call: attach mode has no fake microphone, so the page script plays the file into the host's silent microphone track.

```sh
espeak-ng -w /tmp/say.raw.wav "The launch is on the first of March. Tom will send the budget deck by Friday."
ffmpeg -y -i /tmp/say.raw.wav -ar 16000 -ac 1 -c:a pcm_s16le /tmp/say.wav
cargo build --release -p parley-bot
target/release/live_check 'https://meet.google.com/abc-defg-hij' --attach 9523 --host-authuser 0 --bot-authuser 1 --say /tmp/say.wav --keyword march
```

## Test

`cargo test -p parley-bot -p parley-brain` checks the speaker choice and the request classes. The call itself needs the live check.
