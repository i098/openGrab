//! `parley <meet-link> --attach <port>`: join a Meet call and connect the parts of the product.
//!
//! The bot joins through the signed-in Chromium on the debugging port, serves the meeting page, and posts the page
//! link in the Meet chat. Call audio goes through `parley-stt` and each transcript line goes to the page.
//! A Meet chat line that starts with the wake name is a request: `parley-brain` answers it in the chat, or writes
//! the live doc. Pause, resume and remove on the page stop and start the transcript; remove makes the bot leave.

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, ValueEnum};
use parley_brain::{DEFAULT_NAME, Transcript, find_request, is_doc_request, is_recap_request};
use parley_join::analysis::rms;
use parley_join::protocol::{CallEvent, PageMsg};
use parley_join::{Browser, Opts, Outcome, Role, Session, with_authuser};
use parley_server::{Line, Llm, Meeting, SessionState, app, new_secret, now_ms};
use parley_stt::{Engine, SherpaTranscriber, Transcriber, VadConfig};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Clone, Copy, ValueEnum)]
enum EngineArg {
    Parakeet,
    Whisper,
}

#[derive(Parser)]
#[command(
    about = "Join a Google Meet call: transcript on a meeting page, answers in the Meet chat"
)]
struct Args {
    /// The Google Meet link, like https://meet.google.com/abc-defg-hij
    link: String,
    /// Remote-debugging port of a running, signed-in, headed Chromium. The bot opens one tab there.
    #[arg(long, env = "PARLEY_ATTACH")]
    attach: u16,
    /// Google `authuser` index of the account the bot joins as (default: the browser's default account).
    #[arg(long, env = "PARLEY_BOT_AUTHUSER")]
    bot_authuser: Option<u8>,
    /// The name a chat request starts with ("<wake>, what did we decide?"). Also shown on the bot tile.
    #[arg(long, env = "PARLEY_WAKE", default_value = DEFAULT_NAME)]
    wake: String,
    /// Where the meeting page listens.
    #[arg(long, env = "PARLEY_ADDR", default_value = "127.0.0.1:8080")]
    addr: String,
    /// The base of the page link that the bot posts (default: http://<addr>).
    #[arg(long, env = "PARLEY_BASE_URL")]
    base_url: Option<String>,
    /// Speech-to-text data: `models/` from scripts/fetch-phase0-data.sh.
    #[arg(long, env = "PARLEY_DATA")]
    data: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "parakeet")]
    engine: EngineArg,
    #[arg(long, default_value_t = 8)]
    threads: i32,
    /// Seconds to wait to be admitted to the call.
    #[arg(long, default_value_t = 300)]
    join_timeout: u64,
}

/// Speaker name for a segment: the one person Meet showed as speaking a moment ago, else "Speaker".
// shortcut: no per-track speaker match; two people who speak at once get "Speaker". Upgrade with track attribution (plan R13).
#[derive(Default)]
struct Speaking(Option<(Vec<String>, Instant)>);

impl Speaking {
    fn seen(&mut self, names: Vec<String>) {
        if !names.is_empty() {
            self.0 = Some((names, Instant::now()));
        }
    }

    fn name(&self) -> String {
        match &self.0 {
            Some((names, at)) if names.len() == 1 && at.elapsed() < Duration::from_secs(8) => {
                names[0].clone()
            }
            _ => "Speaker".into(),
        }
    }
}

/// The speech-to-text thread: one VAD and recognizer per audio track, created when the track first carries sound.
/// Returns the sender of `(track, 16 kHz samples)` and the receiver of finished segments.
// shortcut: the audio queue has no bound; the engine runs faster than real time (docs/phase0/stt.md), so it stays short.
fn spawn_stt(
    engine: Engine,
    model_dir: PathBuf,
    silero: PathBuf,
    threads: i32,
) -> (
    std::sync::mpsc::Sender<(u16, Vec<i16>)>,
    mpsc::UnboundedReceiver<String>,
) {
    let (audio_tx, audio_rx) = std::sync::mpsc::channel::<(u16, Vec<i16>)>();
    let (text_tx, text_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let vad = VadConfig::default();
        let mut tracks: BTreeMap<u16, SherpaTranscriber> = BTreeMap::new();
        for (track, pcm) in audio_rx {
            if !tracks.contains_key(&track) {
                if rms(&pcm) < 100.0 {
                    continue;
                }
                match SherpaTranscriber::new(engine, &model_dir, &silero, &vad, threads) {
                    Ok(t) => {
                        tracks.insert(track, t);
                    }
                    Err(e) => {
                        eprintln!("stt: {e}; no transcript from now on");
                        return;
                    }
                }
            }
            let Some(t) = tracks.get_mut(&track) else {
                continue;
            };
            match t.push(&pcm) {
                Ok(segments) => {
                    for s in segments {
                        let _ = text_tx.send(s.text);
                    }
                }
                Err(e) => eprintln!("stt: {e}"),
            }
        }
    });
    (audio_tx, text_rx)
}

/// What a request needs: the meeting, the names and the one channel that carries chat text back to the main loop.
struct Ctx {
    meeting: Arc<Meeting>,
    wake: String,
    page_url: String,
    out: mpsc::UnboundedSender<String>,
    /// One model call at a time: a local model serves one request at once, and doc edits are not serialized.
    turn: tokio::sync::Mutex<()>,
}

fn transcript_of(meeting: &Meeting) -> Transcript {
    let mut t = Transcript::new();
    for l in meeting.lines() {
        t.push(parley_brain::Line {
            speaker: l.speaker,
            start_ms: l.at_ms,
            text: l.text,
        });
    }
    t
}

/// The chat text for a request line: a doc edit, a recap or an answer.
async fn reply(ctx: &Ctx, text: &str) -> Result<String, String> {
    let q = find_request(text, &ctx.wake).ok_or("not a request")?;
    let llm = Llm::from_env().map_err(|e| e.to_string())?;
    if is_doc_request(q) {
        let chat = ctx
            .meeting
            .request(&llm, &ctx.wake, text)
            .await?
            .ok_or("not a request")?;
        return Ok(format!("{chat} {}", ctx.page_url));
    }
    let transcript = transcript_of(&ctx.meeting);
    if is_recap_request(q) {
        transcript.recap(&llm).await
    } else {
        transcript.answer(&llm, q).await
    }
    .map_err(|e| e.to_string())
}

async fn handle_request(ctx: Arc<Ctx>, text: String) {
    let _turn = ctx.turn.lock().await;
    let line = match reply(&ctx, &text).await {
        Ok(l) if !l.trim().is_empty() => l,
        other => {
            eprintln!(
                "request failed: {}",
                other.err().unwrap_or("empty reply".into())
            );
            "I could not answer that.".into()
        }
    };
    let _ = ctx.out.send(line);
}

fn model_paths(a: &Args) -> Result<(Engine, PathBuf, PathBuf)> {
    let data = match &a.data {
        Some(d) => d.clone(),
        None => PathBuf::from(std::env::var("HOME").context("HOME is not set")?)
            .join(".cache/parley-phase0"),
    };
    let (engine, dir) = match a.engine {
        EngineArg::Parakeet => (
            Engine::Parakeet,
            "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
        ),
        EngineArg::Whisper => (Engine::Whisper, "sherpa-onnx-whisper-turbo"),
    };
    let (dir, silero) = (
        data.join("models").join(dir),
        data.join("models/silero_vad.onnx"),
    );
    ensure!(
        dir.is_dir() && silero.is_file(),
        "no speech-to-text models under {}: run scripts/fetch-phase0-data.sh or set PARLEY_DATA",
        data.display()
    );
    Ok((engine, dir, silero))
}

#[tokio::main]
async fn main() -> Result<()> {
    let a = Args::parse();
    let (engine, model_dir, silero) = model_paths(&a)?;
    if let Err(e) = Llm::from_env() {
        eprintln!("warning: chat requests will fail: {e}");
    }

    let meeting = Meeting::new(new_secret());
    let listener = tokio::net::TcpListener::bind(&a.addr)
        .await
        .with_context(|| format!("cannot listen on {}", a.addr))?;
    let base = a
        .base_url
        .clone()
        .unwrap_or_else(|| format!("http://{}", listener.local_addr().expect("bound address")));
    let page_url = format!("{}/m/{}", base.trim_end_matches('/'), meeting.secret());
    let (router, lines_tx) = app(meeting.clone());
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            eprintln!("meeting page server stopped: {e}");
        }
    });
    println!("meeting page: {page_url}");

    let mut bot = Session::launch(Opts {
        browser: Browser::Attach(a.attach),
        name: a.wake.clone(),
        role: Role::Bot,
    })
    .await?;
    let joined = bot
        .join(
            &with_authuser(&a.link, a.bot_authuser),
            Duration::from_secs(a.join_timeout),
        )
        .await;
    match joined {
        Ok(r) if r.outcome == Outcome::Admitted => println!("joined after {} ms", r.waited_ms),
        Ok(r) => {
            bot.leave().await?;
            bail!("did not reach the call: {:?}", r.outcome);
        }
        Err(e) => {
            let _ = bot.leave().await;
            return Err(e);
        }
    }
    let result = run(
        &a, &mut bot, meeting, lines_tx, &page_url, engine, model_dir, silero,
    )
    .await;
    bot.leave().await?;
    result
}

#[allow(clippy::too_many_arguments)]
async fn run(
    a: &Args,
    bot: &mut Session,
    meeting: Arc<Meeting>,
    lines_tx: mpsc::Sender<Line>,
    page_url: &str,
    engine: Engine,
    model_dir: PathBuf,
    silero: PathBuf,
) -> Result<()> {
    bot.open_chat().await?;
    bot.set_tile("listening").await?;
    let notice = format!(
        "{0} is taking notes of this call. Transcript and live doc: {page_url} . Ask me in this chat: \"{0}, ...\"",
        a.wake
    );
    ensure!(
        bot.post_chat(&notice).await?,
        "the notice did not show in the chat"
    );

    let (audio_tx, mut texts) = spawn_stt(engine, model_dir, silero, a.threads);
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();
    let ctx = Arc::new(Ctx {
        meeting: meeting.clone(),
        wake: a.wake.clone(),
        page_url: page_url.to_owned(),
        out: out_tx,
        turn: tokio::sync::Mutex::new(()),
    });
    let mut speaking = Speaking::default();
    let mut shown = SessionState::Running;
    let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());

    let (mut frames, mut peak, mut report) = (0u32, 0f64, Instant::now());
    loop {
        // Meet can replace the side panel by itself, and the page script reads chat lines only while the chat is open.
        let _ = bot.open_chat().await;
        while let Ok(text) = out_rx.try_recv() {
            if !bot.post_chat(&text).await.unwrap_or(false) {
                eprintln!("a chat line did not show in the call");
            }
        }
        while let Ok(text) = texts.try_recv() {
            let line = Line {
                speaker: speaking.name(),
                text,
                at_ms: now_ms(),
            };
            let who = line.speaker.clone();
            if lines_tx.send(line).await.is_ok() {
                eprintln!("transcript line from {who}");
            }
        }
        let state = meeting.state();
        if state != shown {
            shown = state;
            match state {
                SessionState::Removed => {
                    println!("removed on the meeting page: leaving");
                    let _ = bot
                        .post_chat(&format!("{} left the call. The notes are deleted.", a.wake))
                        .await;
                    return Ok(());
                }
                SessionState::Paused => bot.set_tile("paused").await?,
                SessionState::Running => bot.set_tile("listening").await?,
            }
        }
        let event = tokio::select! {
            _ = &mut ctrl_c => {
                println!("interrupted: leaving");
                return Ok(());
            }
            e = bot.next_event(Duration::from_millis(500)) => e,
        };
        if report.elapsed() > Duration::from_secs(30) {
            eprintln!(
                "audio: {frames} frames in the last 30 s, loudest {peak:.0} (speech starts at 100)"
            );
            (frames, peak, report) = (0, 0.0, Instant::now());
        }
        match event {
            Some(CallEvent::Audio(f)) if state == SessionState::Running => {
                frames += 1;
                peak = peak.max(f64::from(rms(&f.pcm)));
                let _ = audio_tx.send((f.track, f.pcm));
            }
            Some(CallEvent::Page(PageMsg::Speaking { names, .. })) => speaking.seen(names),
            Some(CallEvent::Page(PageMsg::Chat { text, .. }))
                if state == SessionState::Running && find_request(&text, &a.wake).is_some() =>
            {
                tokio::spawn(handle_request(ctx.clone(), text));
            }
            Some(CallEvent::Page(PageMsg::Log { msg })) => eprintln!("page: {msg}"),
            Some(CallEvent::Page(PageMsg::Track { .. })) => eprintln!("audio track found"),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaker_is_named_only_when_one_person_spoke_recently() {
        let mut s = Speaking::default();
        assert_eq!(s.name(), "Speaker");
        s.seen(vec!["Ann".into()]);
        assert_eq!(s.name(), "Ann");
        s.seen(vec![]); // silence keeps the last speaker
        assert_eq!(s.name(), "Ann");
        s.seen(vec!["Ann".into(), "Bob".into()]);
        assert_eq!(s.name(), "Speaker");
    }

    /// The transcript path on a recorded 16 kHz mono WAV, the same `spawn_stt` the call audio uses.
    /// `PARLEY_TEST_WAV=say.wav cargo test -p parley-bot -- --ignored --nocapture`
    #[test]
    #[ignore = "needs models and a WAV"]
    fn recorded_wav_becomes_transcript_text() {
        let wav = std::fs::read(std::env::var("PARLEY_TEST_WAV").unwrap()).unwrap();
        let at = wav.windows(4).position(|w| w == b"data").unwrap() + 8;
        let mut pcm: Vec<i16> = wav[at..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        pcm.extend(vec![0; 16_000 * 2]); // trailing silence closes the last segment
        let a = Args::parse_from(["parley", "--attach", "9523", "https://meet.google.com/x"]);
        let (engine, dir, silero) = model_paths(&a).unwrap();
        let (tx, mut rx) = spawn_stt(engine, dir, silero, 8);
        for c in pcm.chunks(1600) {
            tx.send((0, c.to_vec())).unwrap();
        }
        drop(tx);
        let mut text = String::new();
        while let Some(t) = rx.blocking_recv() {
            println!("segment: {t}");
            text += &t.to_lowercase();
        }
        assert!(text.contains("budget"), "transcript: {text}");
    }
}
