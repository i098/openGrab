//! Live check of the `parley` binary on a real Meet call. One PASS or FAIL line per check.
//!
//! `live_check <meet-link> --attach 9523 --host-authuser 0 --bot-authuser 1 --say speech.wav`
//!
//! It needs a headed Chromium with two signed-in accounts (the setup of `p0_exit --host-authuser`).
//! The host account is a tab this program opens; the bot account is the tab that `parley` opens.
//! The program starts `parley`, admits it, reads the notice in the Meet chat, opens the meeting page in
//! a new window, plays `speech.wav` into the host microphone, asks a question and a doc request in the
//! Meet chat, clicks Remove on the page, and checks that `parley` leaves. It closes every tab it opened.

use anyhow::{Context, Result, bail};
use clap::Parser;
use parley_join::cdp::{Cdp, browser_socket};
use parley_join::protocol::{CallEvent, PageMsg};
use parley_join::{Browser, Opts, Outcome, Role, Session, with_authuser};
use serde_json::{Value, json};
use std::path::PathBuf;
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

    /// Click an element of the meeting page (our own page, so a script click is a real one).
    /// Click a button that asks `confirm()`: a dialog would block every page command, so the page answers yes itself.
    async fn click_and_confirm(&self, selector: &str) -> Result<()> {
        self.eval("window.confirm = () => true").await?;
        self.click(selector).await
    }

    async fn click(&self, selector: &str) -> Result<()> {
        let js = format!("document.querySelector({}).click()", Value::from(selector));
        self.eval(&js).await?;
        Ok(())
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
        !page.text("#lines li").await.is_empty()
    })
    .await;
    let lines = page.text("#lines li").await;
    ok &= verdict(
        heard,
        "transcript",
        format!("page lines: {}", lines.replace('\n', " | ")),
    );
    host.set_mic(false).await?;

    let name = &a.wake;
    host.post_chat(&format!("{name}, {}", a.question)).await?;
    let notice_text = notice.clone().unwrap_or_default();
    let mine = format!("{name}, {}", a.question);
    let answer = wait_chat(&mut host, a.answer_timeout, |t| {
        t != notice_text && t != mine
    })
    .await;
    ok &= verdict(
        answer.is_some(),
        "answer",
        format!("chat answer: {}", answer.as_deref().unwrap_or("none")),
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
        reply.is_some() && !doc.trim().is_empty() && doc != before,
        "doc",
        format!(
            "chat line: {}; live doc in the open tab: {}",
            reply.as_deref().unwrap_or("none"),
            doc.replace('\n', " | ")
        ),
    );

    page.click_and_confirm("#remove").await?;
    let left = tokio::time::timeout(Duration::from_secs(90), child.wait()).await;
    ok &= verdict(
        matches!(&left, Ok(Ok(s)) if s.success()),
        "remove",
        format!("Remove clicked on the page; parley exit: {left:?}"),
    );
    let ended = page.text("#ended").await;
    println!("page after remove: {}", ended.trim());

    page.close().await;
    host.leave().await?;
    if !ok {
        bail!("a check failed");
    }
    Ok(())
}
