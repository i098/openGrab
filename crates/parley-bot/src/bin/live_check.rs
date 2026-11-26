//! Live check of the `parley` binary on a real Meet call. One PASS or FAIL line per check.
//!
//! `live_check <meet-link> --attach 9523 --host-authuser 0 --bot-authuser 1 --say speech.wav --voice-dir voice`
//!
//! It needs a headed Chromium with two signed-in accounts (the setup of `p0_exit --host-authuser`).
//! The host account is a tab this program opens; the bot account is the tab that `parley` opens.
//! The program starts `parley`, admits it, reads the notice in the Meet chat, opens the meeting page in
//! a new window, plays `speech.wav` into the host microphone, asks a question and a doc request in the
//! Meet chat, then speaks a request, "pause", "resume" and "leave" (the WAV files in `--voice-dir`) and checks the
//! chat answer, the page state, the bot tile and that `parley` leaves. It prints how long the chat answer took
//! for the spoken request and for the chat request. It closes every tab it opened.

use anyhow::{Context, Result, bail};
use clap::Parser;
use parley_join::cdp::{Cdp, browser_socket};
use parley_join::protocol::{CallEvent, PageMsg};
use parley_join::{Browser, Opts, Outcome, Role, Session, with_authuser};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Parser)]
struct Args {
    link: String,
    #[arg(long)]
    attach: u16,
    #[arg(long)]
    host_authuser: u8,
    #[arg(long)]
    bot_authuser: u8,
    /// A WAV file with speech, played into the host microphone (16 kHz mono 16-bit is fine).
    #[arg(long)]
    say: PathBuf,
    /// Spoken requests as WAV files: `request.wav` ("<wake>, when is the launch?"), `pause.wav`, `resume.wav`
    /// and `leave.wav` (each "<wake>, pause", ...). Played into the host microphone like `--say`.
    #[arg(long)]
    voice_dir: PathBuf,
    /// Save a screenshot of the host's Meet window here after pause and after resume (the bot tile is in it).
    #[arg(long)]
    shots: Option<PathBuf>,
    /// The `parley` binary (default: next to this program).
    #[arg(long)]
    parley: Option<PathBuf>,
    /// The wake name the bot answers to.
    #[arg(long, default_value = parley_brain::DEFAULT_NAME)]
    wake: String,
    /// The question to ask in the chat, after the wake name. It must be answerable from the `--say` audio.
    #[arg(long, default_value = "when is the launch?")]
    question: String,
    #[arg(long, default_value = "start a doc for the launch plan")]
    doc_request: String,
    /// Seconds to wait for the first transcript line on the page (the first speech model load is slow).
    #[arg(long, default_value_t = 1500)]
    transcript_timeout: u64,
    /// Seconds to wait for each answer (a local model can be slow).
    #[arg(long, default_value_t = 850)]
    answer_timeout: u64,
    /// A word that is in the `--say` audio, and in the right answer to `--question`. The transcript and answer checks need it.
    #[arg(long, default_value = "march")]
    keyword: String,
}

fn mentions(text: &str, word: &str) -> bool {
    text.to_lowercase().contains(&word.to_lowercase())
}

fn verdict(ok: bool, check: &str, msg: impl std::fmt::Display) -> bool {
    println!("{} {check}: {msg}", if ok { "PASS" } else { "FAIL" });
    ok
}

/// A page tab in its own window, driven over CDP.
struct PageTab {
    cdp: Cdp,
    target: String,
    session: String,
}

impl PageTab {
    async fn open(port: u16, url: &str) -> Result<Self> {
        let cdp = Cdp::connect(&browser_socket(port, Duration::from_secs(30)).await?).await?;
        let t = cdp
            .call(
                None,
                "Target.createTarget",
                json!({"url": url, "newWindow": true}),
            )
            .await?;
        let target = t["targetId"].as_str().context("no target id")?.to_owned();
        let s = cdp
            .call(
                None,
                "Target.attachToTarget",
                json!({"targetId": target, "flatten": true}),
            )
            .await?;
        let session = s["sessionId"].as_str().context("no session id")?.to_owned();
        Ok(Self {
            cdp,
            target,
            session,
        })
    }

    async fn eval(&self, js: &str) -> Result<Value> {
        let r = self
            .cdp
            .call(
                Some(&self.session),
                "Runtime.evaluate",
                json!({"expression": js, "returnByValue": true, "awaitPromise": true}),
            )
            .await?;
        Ok(r["result"]["value"].clone())
    }

    async fn text(&self, selector: &str) -> String {
        let js = format!(
            "[...document.querySelectorAll({})].map(e => e.innerText).join('\\n')",
            Value::from(selector)
        );
        self.eval(&js)
            .await
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    async fn close(self) {
        let _ = self
            .cdp
            .call(None, "Target.closeTarget", json!({"targetId": self.target}))
            .await;
    }
}

/// Read the host tab's events until a chat line passes `want`, or `secs` pass. Returns the line.
async fn wait_chat(host: &mut Session, secs: u64, want: impl Fn(&str) -> bool) -> Option<String> {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        // Meet can replace the side panel by itself; the page script reads chat lines only while the chat is open.
        let _ = host.open_chat().await;
        if let Some(CallEvent::Page(PageMsg::Chat { text, .. })) =
            host.next_event(Duration::from_secs(1)).await
        {
            println!(
                "host sees chat: {}",
                text.chars().take(160).collect::<String>()
            );
            if want(&text) {
                return Some(text);
            }
        }
    }
    None
}

/// Play a WAV into the host microphone. Returns when the speech ends.
async fn speak(host: &Session, wav: &[u8]) -> Result<Instant> {
    let started = Instant::now();
    Ok(started + Duration::from_secs_f64(host.say(wav).await?))
}

async fn shot(host: &Session, dir: &Option<PathBuf>, name: &str) {
    if let Some(dir) = dir {
        let saved = host.screenshot(&dir.join(format!("{name}.png"))).await;
        println!("host screenshot {name}: {saved:?}");
    }
}

async fn poll<F: Future<Output = bool>>(secs: u64, mut f: impl FnMut() -> F) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f().await {
            return true;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    false
}

#[tokio::main]
async fn main() -> Result<()> {
    let a = Args::parse();
    let wav = std::fs::read(&a.say).context("reading --say")?;
    let voice = |name: &str| {
        std::fs::read(a.voice_dir.join(name)).with_context(|| format!("reading {name}"))
    };
    let (say_request, say_pause, say_resume, say_leave) = (
        voice("request.wav")?,
        voice("pause.wav")?,
        voice("resume.wav")?,
        voice("leave.wav")?,
    );
    let parley = match &a.parley {
        Some(p) => p.clone(),
        None => std::env::current_exe()?.with_file_name("parley"),
    };
    let mut ok = true;

    let mut host = Session::launch(Opts {
        browser: Browser::Attach(a.attach),
        name: "Host".into(),
        role: Role::Bot,
    })
    .await?;
    let r = host
        .join(
            &with_authuser(&a.link, Some(a.host_authuser)),
            Duration::from_secs(300),
        )
        .await?;
    if r.outcome != Outcome::Admitted {
        host.leave().await?;
        bail!(
            "the host tab did not reach the call: {:?}; page: {:?}",
            r.outcome,
            r.last_state
        );
    }
    host.open_chat().await?;
    println!("host is in the call");

    let mut child = tokio::process::Command::new(&parley)
        .arg(&a.link)
        .args(["--attach", &a.attach.to_string()])
        .args(["--bot-authuser", &a.bot_authuser.to_string()])
        .args(["--wake", &a.wake])
        .args(["--addr", "127.0.0.1:0"])
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {}", parley.display()))?;
    let mut out = BufReader::new(child.stdout.take().context("no stdout")?).lines();
    let mut page_url = None;
    let mut joined = false;
    let started = Instant::now();
    while !joined && started.elapsed() < Duration::from_secs(400) {
        tokio::select! {
            line = out.next_line() => match line? {
                Some(l) => {
                    println!("parley: {l}");
                    if let Some(u) = l.strip_prefix("meeting page: ") { page_url = Some(u.to_owned()); }
                    joined |= l.starts_with("joined");
                }
                None => bail!("parley exited before it joined"),
            },
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                if host.admit_waiting().await.unwrap_or(false) {
                    println!("host admitted a waiting guest");
                }
            }
        }
    }
    let page_url = page_url.context("parley printed no page link")?;
    let printed = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = printed.clone();
    tokio::spawn(async move {
        while let Ok(Some(l)) = out.next_line().await {
            println!("parley: {l}");
            sink.lock().unwrap().push(l);
        }
    });
    let printed_line = |want: &str| printed.lock().unwrap().iter().any(|l| l == want);
    ok &= verdict(joined, "join", "the bot reached the call");

    let notice = wait_chat(&mut host, 120, |t| t.contains(&page_url)).await;
    if notice.is_none() {
        let dump: Value = host
            .eval("({phase: __parley.state().phase, chatOpen: __parley.chatOpen(), media: __parley.media(), chat: __parley.chatText ? 1 : 0, body: document.body.innerText.slice(-600)})")
            .await?;
        println!("host page: {dump}");
    }
    ok &= verdict(
        notice.is_some(),
        "notice",
        "the Meet chat shows a line with the meeting page link",
    );

    let page = PageTab::open(a.attach, &page_url).await?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    host.set_mic(true).await?;
    // The call asks for the microphone a moment after the unmute click.
    let mut tries = 0;
    let secs = loop {
        match host.say(&wav).await {
            Ok(s) => break s,
            Err(e) if tries < 15 => {
                tries += 1;
                eprintln!("say: {e}; retry {tries}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) => return Err(e),
        }
    };
    println!("host plays {secs:.1} s of speech into the call");
    let heard = poll(a.transcript_timeout + secs as u64, || async {
        mentions(&page.text("#lines li").await, &a.keyword)
    })
    .await;
    let lines = page.text("#lines li").await;
    ok &= verdict(
        heard,
        "transcript",
        format!("page lines: {}", lines.replace('\n', " | ")),
    );

    let name = &a.wake;
    host.post_chat(&format!("{name}, {}", a.question)).await?;
    let asked = Instant::now();
    let notice_text = notice.clone().unwrap_or_default();
    let mine = format!("{name}, {}", a.question);
    let answer = wait_chat(&mut host, a.answer_timeout, |t| {
        t != notice_text && t != mine
    })
    .await;
    ok &= verdict(
        answer
            .as_deref()
            .is_some_and(|t| t != parley_brain::REQUEST_FAILED && mentions(t, &a.keyword)),
        "answer",
        format!(
            "chat answer after {:.1} s: {}",
            asked.elapsed().as_secs_f64(),
            answer.as_deref().unwrap_or("none")
        ),
    );

    let before = page.text("#doc").await;
    let request = format!("{name}, {}", a.doc_request);
    host.post_chat(&request).await?;
    let reply = wait_chat(&mut host, a.answer_timeout, |t| {
        t != notice_text && t != request && Some(t) != answer.as_deref()
    })
    .await;
    let doc = page.text("#doc").await;
    ok &= verdict(
        reply
            .as_deref()
            .is_some_and(|t| t != parley_brain::REQUEST_FAILED)
            && !doc.trim().is_empty()
            && doc != before,
        "doc",
        format!(
            "chat line: {}; live doc in the open tab: {}",
            reply.as_deref().unwrap_or("none"),
            doc.replace('\n', " | ")
        ),
    );

    let seen: Vec<String> = [
        Some(notice_text.clone()),
        Some(mine),
        Some(request),
        answer,
        reply,
    ]
    .into_iter()
    .flatten()
    .collect();
    let new_chat = |t: &str| !seen.iter().any(|s| s == t);

    let ends = speak(&host, &say_request).await?;
    let spoken = wait_chat(&mut host, a.answer_timeout, new_chat).await;
    let took = Instant::now().saturating_duration_since(ends).as_secs_f64();
    ok &= verdict(
        spoken
            .as_deref()
            .is_some_and(|t| t != parley_brain::REQUEST_FAILED && mentions(t, &a.keyword)),
        "spoken request",
        format!(
            "chat answer {took:.1} s after the end of speech: {}",
            spoken.as_deref().unwrap_or("none")
        ),
    );

    speak(&host, &say_pause).await?;
    let paused = poll(120, || async {
        page.text("#status").await.trim() == "Paused" && printed_line("tile: paused")
    })
    .await;
    ok &= verdict(
        paused,
        "spoken pause",
        format!(
            "page status: {}; bot tile paused",
            page.text("#status").await.trim()
        ),
    );
    shot(&host, &a.shots, "paused").await;

    speak(&host, &say_resume).await?;
    let resumed = poll(120, || async {
        page.text("#status").await.trim() == "Live" && printed_line("tile: listening")
    })
    .await;
    ok &= verdict(
        resumed,
        "spoken resume",
        format!(
            "page status: {}; bot tile listening",
            page.text("#status").await.trim()
        ),
    );
    shot(&host, &a.shots, "listening").await;

    speak(&host, &say_leave).await?;
    let bye = wait_chat(&mut host, 120, |t| mentions(t, "left the call")).await;
    ok &= verdict(
        bye.is_some(),
        "goodbye",
        bye.as_deref().unwrap_or("no goodbye line in the Meet chat"),
    );
    let left = tokio::time::timeout(Duration::from_secs(90), child.wait()).await;
    ok &= verdict(
        matches!(&left, Ok(Ok(s)) if s.success()),
        "spoken leave",
        format!("parley exit: {left:?}"),
    );
    let ended = page.text("#ended").await;
    println!("page after leave: {}", ended.trim());

    page.close().await;
    host.leave().await?;
    if !ok {
        bail!("a check failed");
    }
    Ok(())
}
