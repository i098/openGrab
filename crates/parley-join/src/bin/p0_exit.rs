//! Phase 0 exit test for the guest join (plan 6.1): Join, Chat, Tile, Outsider input, fake-audio speakers.
//! Run it through `scripts/p0-exit.sh`, which wraps it in Xvfb. One PASS or FAIL line per check.

use anyhow::{Context, Result, bail};
use clap::Parser;
use parley_join::analysis::{rms, speaking_match, wav_bytes};
use parley_join::protocol::{CallEvent, PageMsg, Participant};
use parley_join::{Opts, Outcome, Role, Session};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(about = "Phase 0 exit test: guest join of a Google Meet call")]
struct Args {
    /// The Meet link. The host must admit every guest that asks to join.
    link: String,
    /// Where logs, screenshots, audio and JSON go (default: ./p0-out/<epoch>).
    #[arg(long)]
    out: Option<PathBuf>,
    /// Pinned Chromium binary (default: the newest under ~/.local/share/opengrab/chromium).
    #[arg(long, env = "PARLEY_CHROME")]
    chrome: Option<PathBuf>,
    #[arg(long, default_value = "Parley Notetaker")]
    name: String,
    /// Sign the bot in as this Google account first (bring-your-own-login). The password comes from
    /// the environment variable GOOGLE_ACCOUNT_PASSWORD; one attempt, no code entry, no recovery.
    #[arg(long)]
    signin_email: Option<String>,
    /// Join attempts. The last one stays in the call for the other checks.
    #[arg(long, default_value_t = 1)]
    joins: u32,
    #[arg(long, default_value_t = 300)]
    join_timeout: u64,
    #[arg(long, default_value_t = 10)]
    chat_count: u32,
    #[arg(long, default_value_t = 600)]
    chat_timeout: u64,
    /// Directory with a.wav, b.wav, c.wav (see scripts/make-speakers.sh): enables the speakers check.
    #[arg(long)]
    speakers: Option<PathBuf>,
    /// Checks to run, comma separated: join,tile,outsider,speakers,chat.
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "join,tile,outsider,speakers,chat"
    )]
    checks: Vec<String>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[derive(Default)]
struct Collected {
    chat: VecDeque<(String, String)>,
    frames: BTreeMap<u16, Vec<(u64, Vec<i16>)>>,
    speaking: Vec<(u64, Vec<String>)>,
    roster: Vec<Participant>,
}

/// Process the bot's events for `dur`.
async fn drain(bot: &mut Session, dur: Duration, col: &mut Collected) {
    let end = tokio::time::Instant::now() + dur;
    loop {
        let left = end.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return;
        }
        match bot.next_event(left).await {
            Some(CallEvent::Audio(f)) => col
                .frames
                .entry(f.track)
                .or_default()
                .push((f.end_ms, f.pcm)),
            Some(CallEvent::Page(PageMsg::Chat { id, sender, text })) => {
                eprintln!("chat {id} from {sender}: {text}");
                col.chat.push_back((sender, text));
            }
            Some(CallEvent::Page(PageMsg::Speaking { names, at })) => {
                col.speaking.push((at, names))
            }
            Some(CallEvent::Page(PageMsg::Roster { participants })) => col.roster = participants,
            Some(CallEvent::Page(PageMsg::Track { id, label, stream })) => {
                eprintln!("page: audio track {id} ({label}) stream {stream}")
            }
            Some(CallEvent::Page(PageMsg::Log { msg })) => eprintln!("page: {msg}"),
            None => return,
        }
    }
}

fn verdict(ok: bool, check: &str, msg: String, results: &mut Vec<(String, bool)>) {
    println!("{} {check}: {msg}", if ok { "PASS" } else { "FAIL" });
    results.push((check.to_string(), ok));
}

fn find_chrome() -> Result<PathBuf> {
    let base = PathBuf::from(std::env::var("HOME")?).join(".local/share/opengrab/chromium/chrome");
    let mut dirs: Vec<_> = std::fs::read_dir(&base)
        .with_context(|| {
            format!(
                "no Chromium under {}; see crates/parley-join/README.md",
                base.display()
            )
        })?
        .flatten()
        .collect();
    dirs.sort_by_key(|d| d.file_name());
    Ok(dirs
        .last()
        .context("no Chromium installed")?
        .path()
        .join("chrome-linux64/chrome"))
}

/// Profiles live in the temp dir (tmpfs on this host): on a slow disk Chromium stalls page loads past CDP timeouts.
fn launch_opts(chrome: &Path, tag: &str, name: &str, role: Role) -> Opts {
    Opts {
        chrome: chrome.into(),
        profile: std::env::temp_dir()
            .join(format!("parley-p0-{}", std::process::id()))
            .join(tag),
        name: name.into(),
        role,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let a = Args::parse();
    let want = |c: &str| a.checks.iter().any(|x| x == c);
    if a.signin_email.is_some() && a.joins != 1 {
        // Each attempt uses a fresh profile, so each would sign in again.
        bail!("--signin-email allows one sign-in: use --joins 1");
    }
    let out = a
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("p0-out/{}", now_ms() / 1000)));
    std::fs::create_dir_all(&out)?;
    let chrome = a.chrome.clone().map_or_else(find_chrome, Ok)?;
    let mut results = Vec::new();
    let mut col = Collected::default();

    // Join: `joins` attempts, each with a fresh profile; the last one stays.
    let mut admitted = 0u32;
    let mut bot: Option<Session> = None;
    let mut joins_log = String::new();
    for i in 1..=a.joins {
        let mut s = Session::launch(launch_opts(
            &chrome,
            &format!("bot-{i}"),
            &a.name,
            Role::Bot,
        ))
        .await?;
        if i == 1 {
            println!("chromium: {}", s.version().await?);
        }
        if let Some(email) = &a.signin_email {
            let pw = std::env::var("GOOGLE_ACCOUNT_PASSWORD")
                .context("--signin-email needs GOOGLE_ACCOUNT_PASSWORD in the environment")?;
            match s.sign_in(email, &pw).await {
                Ok(()) => println!("signed in as {email}"),
                Err(e) => {
                    let _ = s.screenshot(&out.join("signin.png")).await;
                    println!("FAIL join: {e:#}");
                    s.leave().await?;
                    bail!("sign-in failed");
                }
            }
        }
        println!(
            "join {i}/{}: asking to join as \"{}\"; the host must admit it",
            a.joins, a.name
        );
        let r = s.join(&a.link, Duration::from_secs(a.join_timeout)).await?;
        let line = serde_json::json!({"n": i, "at": now_ms(), "report": r});
        joins_log += &format!("{line}\n");
        eprintln!(
            "join {i}: {:?} after {} ms; page: {}",
            r.outcome, r.waited_ms, r.last_state.text
        );
        if r.outcome == Outcome::Admitted {
            admitted += 1;
        }
        let _ = s.screenshot(&out.join(format!("join-{i}.png"))).await;
        if i == a.joins && r.outcome == Outcome::Admitted {
            bot = Some(s);
        } else {
            s.leave().await?;
        }
    }
    std::fs::write(out.join("joins.jsonl"), joins_log)?;
    if want("join") {
        verdict(
            admitted * 10 >= a.joins * 9 && admitted > 0,
            "join",
            format!(
                "{admitted} of {} guest joins reached the call (go limit: 9 of 10)",
                a.joins
            ),
            &mut results,
        );
    }
    let Some(mut bot) = bot else {
        println!("SKIP every other check: the last join did not reach the call");
        write_summary(&out, &results)?;
        bail!("not in the call");
    };

    drain(&mut bot, Duration::from_secs(5), &mut col).await;
    bot.screenshot(&out.join("in-call.png")).await?;

    if want("tile") {
        let mut pngs = Vec::new();
        for state in ["listening", "paused"] {
            bot.set_tile(state).await?;
            drain(&mut bot, Duration::from_secs(3), &mut col).await;
            let png = bot.tile_png().await?;
            std::fs::write(out.join(format!("tile-{state}.png")), &png)?;
            bot.screenshot(&out.join(format!("page-{state}.png")))
                .await?;
            pngs.push(png);
        }
        verdict(pngs[0] != pngs[1], "tile", "both states set and drawn (tile-*.png, page-*.png); a person must still look at the call".into(), &mut results);
        bot.set_tile("listening").await?;
    }

    if want("outsider") {
        let mut snaps = Vec::new();
        for _ in 0..3 {
            snaps.push(serde_json::json!({"at": now_ms(), "participants": bot.roster().await?}));
            drain(&mut bot, Duration::from_secs(3), &mut col).await;
        }
        let others = snaps.last().map_or(0, |s| {
            s["participants"]
                .as_array()
                .map_or(0, |p| p.iter().filter(|x| x["is_self"] == false).count())
        });
        let file = out.join("participants.json");
        std::fs::write(
            &file,
            serde_json::to_string_pretty(
                &serde_json::json!({"bot_kind": "guest", "snapshots": snaps}),
            )?,
        )?;
        verdict(
            others > 0,
            "outsider-input",
            format!(
                "{others} other participant(s) written to {} (verdict needs 3 calls with the outside account)",
                file.display()
            ),
            &mut results,
        );
    }

    if want("speakers") {
        match &a.speakers {
            Some(dir) => speakers(&a, &chrome, &out, dir, &mut bot, &mut col, &mut results).await?,
            None => verdict(
                false,
                "speakers",
                "no --speakers directory given".into(),
                &mut results,
            ),
        }
    }

    if want("chat") {
        println!(
            "chat: waiting for the host to post {} messages",
            a.chat_count
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(a.chat_timeout);
        let (mut read, mut landed, mut log) = (0u32, 0u32, String::new());
        while read < a.chat_count && tokio::time::Instant::now() < deadline {
            let Some((sender, text)) = col.chat.pop_front() else {
                drain(&mut bot, Duration::from_secs(1), &mut col).await;
                continue;
            };
            read += 1;
            let reply = format!(
                "ack {read}/{}: {}",
                a.chat_count,
                text.chars().take(50).collect::<String>()
            );
            let ok = bot.post_chat(&reply).await.unwrap_or(false);
            landed += u32::from(ok);
            log += &format!(
                "{}\n",
                serde_json::json!({"n": read, "from": sender, "text": text, "reply": reply, "landed": ok})
            );
        }
        std::fs::write(out.join("chat.jsonl"), log)?;
        verdict(
            read == a.chat_count && landed == read,
            "chat",
            format!(
                "read {read} of {} host messages, {landed} replies landed in the message list",
                a.chat_count
            ),
            &mut results,
        );
    }

    write_summary(&out, &results)?;
    bot.screenshot(&out.join("end.png")).await?;
    bot.leave().await?;
    Ok(())
}

fn write_summary(out: &Path, results: &[(String, bool)]) -> Result<()> {
    let map: BTreeMap<_, _> = results.iter().cloned().collect();
    std::fs::write(
        out.join("p0-exit.json"),
        serde_json::to_string_pretty(&map)?,
    )?;
    Ok(())
}

/// Three more Chromium profiles join; each plays a known recording as its microphone.
/// The conductor unmutes them in turns while the bot records what it hears and which tile Meet lights.
async fn speakers(
    a: &Args,
    chrome: &Path,
    out: &Path,
    dir: &Path,
    bot: &mut Session,
    col: &mut Collected,
    results: &mut Vec<(String, bool)>,
) -> Result<()> {
    let names = ["Speaker A", "Speaker B", "Speaker C"];
    let mut s = Vec::new();
    for (i, n) in names.iter().enumerate() {
        let wav = dir
            .join(format!("{}.wav", ["a", "b", "c"][i]))
            .canonicalize()
            .context("speaker wav missing")?;
        s.push(
            Session::launch(launch_opts(
                chrome,
                &format!("speaker-{i}"),
                n,
                Role::Speaker { audio: wav },
            ))
            .await?,
        );
    }
    let timeout = Duration::from_secs(a.join_timeout);
    println!("speakers: three guests ask to join; the host must admit Speaker A, B and C");
    let (r0, r1, r2) = {
        let [s0, s1, s2] = &mut s[..] else {
            unreachable!()
        };
        tokio::join!(
            s0.join(&a.link, timeout),
            s1.join(&a.link, timeout),
            s2.join(&a.link, timeout)
        )
    };
    let joined = [r0?, r1?, r2?]
        .iter()
        .filter(|r| r.outcome == Outcome::Admitted)
        .count();
    if joined < 3 {
        verdict(
            false,
            "speakers",
            format!("only {joined} of 3 speaker profiles reached the call"),
            results,
        );
        for x in s {
            x.leave().await?;
        }
        return Ok(());
    }
    for x in &s {
        x.set_media(false, false).await?;
    }
    drain(bot, Duration::from_secs(3), col).await;

    // (who talks, seconds): three solo turns, then two at once.
    let plan: [(&[usize], u64); 4] = [(&[0], 12), (&[1], 12), (&[2], 12), (&[0, 1], 12)];
    let mut windows = Vec::new();
    for (who, secs) in plan {
        for &i in who {
            s[i].set_mic(true).await?;
        }
        let start = now_ms() + 2_000; // leave 2 s for the unmute to reach the call
        drain(bot, Duration::from_secs(secs), col).await;
        let end = now_ms();
        for &i in who {
            s[i].set_mic(false).await?;
        }
        let mut expected: Vec<String> = who.iter().map(|i| names[*i].to_string()).collect();
        expected.sort();
        windows.push((start, end, expected));
        drain(bot, Duration::from_secs(3), col).await;
    }

    let mut report = Vec::new();
    for (start, end, expected) in &windows {
        let lit = speaking_match(&col.speaking, *start, *end, expected);
        let level = col
            .frames
            .values()
            .map(|fr| {
                let pcm: Vec<i16> = fr
                    .iter()
                    .filter(|(t, _)| t >= start && t < end)
                    .flat_map(|(_, p)| p.iter().copied())
                    .collect();
                rms(&pcm)
            })
            .fold(0.0, f64::max);
        report.push(serde_json::json!({"start_ms": start, "end_ms": end, "expected": expected, "tile_lit_share": lit, "loudest_track_rms": level}));
    }
    std::fs::write(
        out.join("speakers.json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    std::fs::create_dir_all(out.join("audio"))?;
    for (track, fr) in &col.frames {
        let pcm: Vec<i16> = fr.iter().flat_map(|(_, p)| p.iter().copied()).collect();
        std::fs::write(
            out.join(format!("audio/track-{track}.wav")),
            wav_bytes(&pcm),
        )?;
    }
    // Pass: in each solo window the bot heard sound (rms above 100 of 32768) and Meet lit exactly that speaker's tile for half the time.
    let solo_ok = windows
        .iter()
        .zip(&report)
        .filter(|(w, _)| w.2.len() == 1)
        .all(|(_, r)| {
            r["loudest_track_rms"].as_f64() > Some(100.0)
                && r["tile_lit_share"].as_f64() >= Some(0.5)
        });
    verdict(
        solo_ok,
        "speakers",
        format!(
            "{} audio track(s) captured; per-window levels and tile attribution in speakers.json, audio in audio/",
            col.frames.len()
        ),
        results,
    );
    for x in s {
        x.leave().await?;
    }
    Ok(())
}
