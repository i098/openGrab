//! Rust driver for a Chromium bot that joins a Google Meet call (Phase 0 spike, plan 5.1 row 1).
//!
//! The driver starts a pinned, non-headless Chromium (run it inside Xvfb, R1), injects
//! `page/inject.js` before the page loads, and talks to it over a local WebSocket (audio, chat,
//! roster) and CDP (clicks, text input, commands). It signs in to nothing: guest join only.

pub mod analysis;
pub mod cdp;
pub mod protocol;
pub mod selectors;

use anyhow::{Context, Result, bail};
use cdp::Cdp;
use futures_util::StreamExt;
use protocol::{AudioFrame, CallEvent, PageMsg};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::{
    Message,
    handshake::server::{ErrorResponse, Request, Response},
};

const INJECT_JS: &str = include_str!("../page/inject.js");

#[derive(Debug, Clone)]
pub enum Role {
    /// The notetaker: silent microphone, camera showing the tile.
    Bot,
    /// A test speaker whose microphone plays `audio` (a WAV file) through Chromium's fake capture device.
    Speaker { audio: PathBuf },
}

#[derive(Debug, Clone)]
pub struct Opts {
    pub chrome: PathBuf,
    pub profile: PathBuf,
    pub name: String,
    pub role: Role,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Admitted,
    Denied,
    /// Meet refused the knock itself ("You can't join this video call").
    Refused,
    Expired,
    NotFound,
    /// The page left meet.google.com, for example to a Google sign-in page.
    Offsite,
    Timeout,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PageState {
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub can_join: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct JoinReport {
    pub outcome: Outcome,
    /// Time from the click on "Ask to join" to admission (or to the end state).
    pub waited_ms: u128,
    pub last_state: PageState,
}

pub struct Session {
    cdp: Cdp,
    child: tokio::process::Child,
    /// CDP session id of the one tab.
    tab: String,
    opts: Opts,
    events: mpsc::UnboundedReceiver<CallEvent>,
    tasks: Vec<JoinHandle<()>>,
}

impl Session {
    pub async fn launch(opts: Opts) -> Result<Self> {
        if std::env::var_os("DISPLAY").is_none() {
            bail!(
                "DISPLAY is not set: the bot never runs headless; start it inside Xvfb (scripts/p0-exit.sh does)"
            );
        }
        let token = random_token()?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let ws_url = format!("ws://{}/{token}", listener.local_addr()?);
        let (tx, events) = mpsc::unbounded_channel();
        let tasks = vec![tokio::spawn(serve(listener, token, tx))];

        let mut args = vec![
            "--use-fake-ui-for-media-stream".to_string(),
            "--use-fake-device-for-media-stream".to_string(),
            "--autoplay-policy=no-user-gesture-required".to_string(),
            "--lang=en-US".to_string(),
            "--accept-lang=en-US".to_string(),
        ];
        if let Role::Speaker { audio } = &opts.role {
            args.push(format!(
                "--use-file-for-fake-audio-capture={}",
                audio.display()
            ));
        }
        // Mode 700 and deleted in `leave`: the profile holds the browser's cookies.
        std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
            .create(&opts.profile)?;
        let port = free_port()?;
        // A fixed port, not 0: Chromium treats port 0 as automation mode and sets `navigator.webdriver` to true.
        // No `--enable-automation`, no stealth flags: a plain Chromium with a window and fake media devices.
        let mut child = tokio::process::Command::new(&opts.chrome)
            .arg(format!("--user-data-dir={}", opts.profile.display()))
            .arg(format!("--remote-debugging-port={port}"))
            .arg("--window-size=1280,800")
            .args(["--no-first-run", "--no-default-browser-check"])
            .args(&args)
            .arg("about:blank")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("launching Chromium")?;
        let ws_path = cdp::browser_socket(port, Duration::from_secs(120)).await?;
        let cdp = Cdp::connect(&ws_path).await?;
        let targets = cdp.call(None, "Target.getTargets", json!({})).await?;
        let target = targets["targetInfos"]
            .as_array()
            .and_then(|t| t.iter().find(|t| t["type"] == "page"))
            .context("the first tab is missing")?["targetId"]
            .clone();
        let tab = cdp
            .call(
                None,
                "Target.attachToTarget",
                json!({"targetId": target, "flatten": true}),
            )
            .await?["sessionId"]
            .as_str()
            .context("no session id for the tab")?
            .to_string();
        let headed: Result<()> = async {
            let ua = cdp
                .call(
                    Some(&tab),
                    "Runtime.evaluate",
                    json!({"expression": "navigator.userAgent", "returnByValue": true}),
                )
                .await?["result"]["value"]
                .as_str()
                .filter(|s| !s.is_empty())
                .context("no navigator.userAgent")?
                .to_string();
            let v = cdp.call(None, "Browser.getVersion", json!({})).await?;
            let product = v["product"].as_str().filter(|s| !s.is_empty());
            let version_ua = v["userAgent"].as_str().filter(|s| !s.is_empty());
            let (Some(product), Some(version_ua)) = (product, version_ua) else {
                bail!("Browser.getVersion gave no product or userAgent");
            };
            for s in [ua.as_str(), product, version_ua] {
                if s.to_lowercase().contains("headless") {
                    bail!("the browser says headless ({s}): the bot runs headed only");
                }
            }
            Ok(())
        }
        .await;
        if let Err(e) = headed {
            let _ = child.kill().await;
            let _ = std::fs::remove_dir_all(&opts.profile);
            return Err(e);
        }
        let cfg_js = serde_json::json!({
            "ws": ws_url,
            "role": if matches!(opts.role, Role::Bot) { "bot" } else { "speaker" },
            "name": opts.name,
            "selectors": selectors::load()?,
        });
        // Without `Page.enable` the new-document script did not run in this setup.
        cdp.call(Some(&tab), "Page.enable", json!({})).await?;
        let source = format!("window.__PARLEY_CFG = {cfg_js};\n{INJECT_JS}");
        cdp.call(
            Some(&tab),
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": source}),
        )
        .await?;
        Ok(Self {
            cdp,
            child,
            tab,
            opts,
            events,
            tasks,
        })
    }

    async fn cmd(&self, method: &str, params: Value) -> Result<Value> {
        self.cdp.call(Some(&self.tab), method, params).await
    }

    async fn insert_text(&self, text: &str) -> Result<()> {
        self.cmd("Input.insertText", json!({"text": text})).await?;
        Ok(())
    }

    pub async fn version(&self) -> Result<String> {
        let v = self.cdp.call(None, "Browser.getVersion", json!({})).await?;
        Ok(v["product"].as_str().unwrap_or_default().to_string())
    }

    /// Load a page without waiting for the load event, which Meet can delay.
    /// A loaded host can delay the reply past the CDP timeout while the navigation still happens.
    async fn navigate(&self, url: &str) {
        let nav = self.cmd("Page.navigate", json!({"url": url}));
        match tokio::time::timeout(Duration::from_secs(20), nav).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => eprintln!("navigate reply: {e}; polling the page anyway"),
            Err(_) => eprintln!("navigate: no reply in 20s; polling the page anyway"),
        }
    }

    /// Open the Meet link, pass the pre-join screen (a guest types a name), ask to join and wait for the host.
    pub async fn join(&mut self, url: &str, timeout: Duration) -> Result<JoinReport> {
        let start = Instant::now();
        self.navigate(url).await;
        let mut named = false;
        let mut clicked: Option<Instant> = None;
        let mut state = PageState::default();
        let outcome = loop {
            if start.elapsed() > timeout {
                break Outcome::Timeout;
            }
            state = self.state().await;
            match state.phase.as_str() {
                "in_call" => break Outcome::Admitted,
                "denied" => break Outcome::Denied,
                "refused" => break Outcome::Refused,
                "expired" => break Outcome::Expired,
                "not_found" => break Outcome::NotFound,
                "offsite" => break Outcome::Offsite,
                "prejoin" => {
                    if !named {
                        named = self.enter_name().await.is_ok();
                        self.set_media(false, matches!(self.opts.role, Role::Bot))
                            .await?;
                    } else if state.can_join
                        && clicked.is_none_or(|t| t.elapsed() > Duration::from_secs(20))
                    {
                        self.click("join").await?;
                        clicked = Some(Instant::now());
                    }
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(700)).await;
        };
        let waited_ms = clicked.map_or(0, |t| t.elapsed().as_millis());
        Ok(JoinReport {
            outcome,
            waited_ms,
            last_state: state,
        })
    }

    pub async fn state(&self) -> PageState {
        self.eval("window.__parley ? __parley.state() : {phase: 'loading'}")
            .await
            .unwrap_or_default()
    }

    pub async fn eval<T: DeserializeOwned>(&self, js: &str) -> Result<T> {
        let r = self
            .cmd(
                "Runtime.evaluate",
                json!({"expression": js, "returnByValue": true, "awaitPromise": true}),
            )
            .await?;
        if let Some(e) = r.get("exceptionDetails") {
            bail!(
                "page script threw: {}",
                e["exception"]["description"]
                    .as_str()
                    .or(e["text"].as_str())
                    .unwrap_or("?")
            );
        }
        Ok(serde_json::from_value(r["result"]["value"].clone())?)
    }

    /// A real (trusted) mouse click on a control the page script found.
    pub async fn click(&self, kind: &str) -> Result<()> {
        let p: Option<Pt> = self.eval(&format!("__parley.point('{kind}')")).await?;
        let p = p.with_context(|| format!("no {kind} control on the page"))?;
        for (ty, buttons) in [("mouseMoved", 0), ("mousePressed", 1), ("mouseReleased", 0)] {
            self.cmd(
                "Input.dispatchMouseEvent",
                json!({"type": ty, "x": p.x, "y": p.y, "button": "left", "buttons": buttons, "clickCount": 1}),
            )
            .await?;
        }
        Ok(())
    }

    async fn enter_name(&self) -> Result<()> {
        self.click("name").await?;
        self.eval::<serde_json::Value>("document.activeElement && document.activeElement.select && document.activeElement.select()").await?;
        self.insert_text(&self.opts.name).await?;
        Ok(())
    }

    /// Bring the microphone and camera toggles to the wanted state (on = true).
    pub async fn set_media(&self, mic: bool, cam: bool) -> Result<()> {
        for _ in 0..3 {
            let m: Media = self.eval("__parley.media()").await?;
            let mut changed = false;
            for (kind, have, want) in [("mic", m.mic, mic), ("cam", m.cam, cam)] {
                if have.is_some_and(|h| h != want) {
                    self.click(kind).await?;
                    changed = true;
                }
            }
            if !changed {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Ok(())
    }

    pub async fn set_mic(&self, on: bool) -> Result<()> {
        let cam = self
            .eval::<Media>("__parley.media()")
            .await?
            .cam
            .unwrap_or(false);
        self.set_media(on, cam).await
    }

    /// Post one chat line. True once the line shows in Meet's message list.
    pub async fn post_chat(&self, text: &str) -> Result<bool> {
        let js_text = serde_json::to_string(text)?;
        if !self.eval::<bool>("__parley.chatOpen()").await? {
            self.click("chatToggle").await?;
            tokio::time::sleep(Duration::from_millis(800)).await;
        }
        let before: u32 = self.eval(&format!("__parley.chatCount({js_text})")).await?;
        self.eval::<serde_json::Value>(&format!("__parley.rememberSent({js_text})"))
            .await?;
        self.click("chatComposer").await?;
        self.insert_text(text).await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        if self.click("chatSend").await.is_err() {
            for ty in ["keyDown", "keyUp"] {
                self.cmd(
                    "Input.dispatchKeyEvent",
                    json!({"type": ty, "key": "Enter", "code": "Enter", "windowsVirtualKeyCode": 13}),
                )
                .await?;
            }
        }
        for _ in 0..40 {
            if self
                .eval::<u32>(&format!("__parley.chatCount({js_text})"))
                .await?
                > before
            {
                return Ok(true);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Ok(false)
    }

    /// Show `state` ("listening", "paused", ...) on the bot camera tile.
    pub async fn set_tile(&self, state: &str) -> Result<()> {
        self.eval::<serde_json::Value>(&format!(
            "__parley.setTile({})",
            serde_json::to_string(state)?
        ))
        .await?;
        Ok(())
    }

    /// The tile canvas as a PNG data URL, for evidence.
    pub async fn tile_png(&self) -> Result<Vec<u8>> {
        let url: String = self.eval("__parley.tileImage()").await?;
        decode_data_url(&url)
    }

    pub async fn roster(&self) -> Result<Vec<protocol::Participant>> {
        self.eval("__parley.roster()").await
    }

    pub async fn screenshot(&self, path: &Path) -> Result<()> {
        let r = self
            .cmd("Page.captureScreenshot", json!({"format": "png"}))
            .await?;
        std::fs::write(path, decode_base64(r["data"].as_str().unwrap_or_default())?)?;
        Ok(())
    }

    /// Next page or audio event, or None after `within`.
    pub async fn next_event(&mut self, within: Duration) -> Option<CallEvent> {
        tokio::time::timeout(within, self.events.recv())
            .await
            .ok()
            .flatten()
    }

    pub async fn leave(mut self) -> Result<()> {
        let _ = self.click("leave").await;
        let _ = self.cdp.call(None, "Browser.close", json!({})).await;
        let _ = self.child.wait().await;
        for t in &self.tasks {
            t.abort();
        }
        let _ = std::fs::remove_dir_all(&self.opts.profile);
        Ok(())
    }
}

#[derive(Deserialize)]
struct Pt {
    x: f64,
    y: f64,
}

#[derive(Deserialize)]
struct Media {
    mic: Option<bool>,
    cam: Option<bool>,
}

/// A port the kernel just handed out; Chromium binds it a moment later.
fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

/// 16 random bytes as hex from the kernel; the page must present it to reach the WebSocket.
fn random_token() -> Result<String> {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn decode_data_url(url: &str) -> Result<Vec<u8>> {
    decode_base64(url.split_once(',').context("not a data URL")?.1)
}

fn decode_base64(b64: &str) -> Result<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let (mut out, mut acc, mut bits) = (Vec::new(), 0u32, 0);
    for c in b64.bytes().filter(|c| *c != b'=') {
        acc = (acc << 6)
            | ALPHABET
                .iter()
                .position(|a| *a == c)
                .context("bad base64")? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

async fn serve(listener: TcpListener, token: String, tx: mpsc::UnboundedSender<CallEvent>) {
    while let Ok((stream, _)) = listener.accept().await {
        let (tx, want) = (tx.clone(), format!("/{token}"));
        tokio::spawn(async move {
            #[allow(clippy::result_large_err)] // the callback type is fixed by tungstenite
            let check = |req: &Request, resp: Response| {
                if req.uri().path() == want {
                    Ok(resp)
                } else {
                    Err(ErrorResponse::new(Some("bad token".into())))
                }
            };
            let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, check).await else {
                return;
            };
            while let Some(Ok(msg)) = ws.next().await {
                let ev = match msg {
                    Message::Binary(b) => AudioFrame::decode(&b).ok().map(CallEvent::Audio),
                    Message::Text(t) => serde_json::from_str::<PageMsg>(&t)
                        .ok()
                        .map(CallEvent::Page),
                    _ => None,
                };
                if let Some(ev) = ev
                    && tx.send(ev).is_err()
                {
                    return;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn data_url_decodes_to_the_png_bytes() {
        // "\x89PNG" is iVBORw==
        assert_eq!(
            super::decode_data_url("data:image/png;base64,iVBORw==").unwrap(),
            [0x89, b'P', b'N', b'G']
        );
    }
}
