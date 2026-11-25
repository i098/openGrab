//! Rust driver for a Chromium bot that joins a Google Meet call (Phase 0 spike, plan 5.1 row 1).
//!
//! The driver starts a pinned, non-headless Chromium (run it inside Xvfb, R1), injects
//! `page/inject.js` before the page loads, and talks to it over a local WebSocket (audio, chat,
//! roster) and CDP (clicks, text input, commands). A persistent profile retains a local sign-in.

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
const MEET_ORIGIN: &str = "location.origin === 'https://meet.google.com'";

#[derive(Debug, Clone)]
pub enum Role {
    /// The notetaker: silent microphone, camera showing the tile.
    Bot,
    /// A test speaker whose microphone plays `audio` (a WAV file) through Chromium's fake capture device.
    Speaker { audio: PathBuf },
}

#[derive(Debug, Clone)]
pub struct Opts {
    pub browser: Browser,
    pub name: String,
    pub role: Role,
}

#[derive(Debug, Clone)]
pub enum Browser {
    Launch {
        chrome: PathBuf,
        profile: PathBuf,
        persistent_profile: bool,
    },
    Attach(u16),
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

/// Accept a persistent profile path only if it is outside the repository and is new, empty or an existing Chromium profile.
/// It changes nothing on disk, so a rejected directory keeps its mode and content.
pub fn check_profile(profile: &Path) -> Result<()> {
    let abs = std::path::absolute(profile)?;
    let mut tail = Vec::new();
    let mut head = abs.as_path();
    while !head.exists() {
        tail.push(
            head.file_name()
                .context("profile path has no usable name")?,
        );
        head = head.parent().context("profile path has no parent")?;
    }
    let mut resolved = head.canonicalize()?;
    resolved.extend(tail.iter().rev());
    let mut roots = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")];
    if let Ok(cwd) = std::env::current_dir() {
        roots.extend(
            cwd.ancestors()
                .filter(|d| d.join(".git").exists())
                .map(Path::to_path_buf),
        );
    }
    for root in roots.iter().filter_map(|r| r.canonicalize().ok()) {
        if resolved.starts_with(&root) {
            bail!(
                "profile {} is inside the repository {}",
                profile.display(),
                root.display()
            );
        }
    }
    if resolved.exists() {
        if !resolved.is_dir() {
            bail!("profile {} is not a directory", profile.display());
        }
        if resolved.join("Local State").exists() || std::fs::read_dir(&resolved)?.next().is_none() {
            return Ok(());
        }
        bail!(
            "profile {} is not empty and is not a Chromium profile",
            profile.display()
        );
    }
    Ok(())
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
    /// Time from the click on "Ask to join" to the end state. With no click, from the start of the join.
    pub waited_ms: u128,
    pub last_state: PageState,
}

pub struct Session {
    cdp: Cdp,
    child: Option<tokio::process::Child>,
    /// CDP session id of the one tab.
    tab: String,
    opts: Opts,
    events: mpsc::UnboundedReceiver<CallEvent>,
    tasks: Vec<JoinHandle<()>>,
    /// The tab this driver opened in an attached browser. Closed on drop; the browser stays open.
    owned_target: Option<OwnedTab>,
}

impl Session {
    pub async fn launch(opts: Opts) -> Result<Self> {
        let temporary_profile = match &opts.browser {
            Browser::Attach(_) => None,
            Browser::Launch {
                profile,
                persistent_profile,
                ..
            } => {
                if std::env::var_os("DISPLAY").is_none() {
                    bail!(
                        "DISPLAY is not set: the bot never runs headless; start it inside Xvfb (scripts/p0-exit.sh does)"
                    );
                }
                (!persistent_profile).then(|| profile.clone())
            }
        };
        let token = random_token()?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let ws_url = format!("ws://{}/{token}", listener.local_addr()?);
        let (tx, events) = mpsc::unbounded_channel();
        let task = tokio::spawn(serve(listener, token, tx));
        let abort = task.abort_handle();
        match Self::start(opts, events, vec![task]).await {
            Ok(session) => {
                session.configure(ws_url).await?;
                Ok(session)
            }
            Err(error) => {
                abort.abort();
                if let Some(profile) = temporary_profile {
                    let _ = std::fs::remove_dir_all(profile);
                }
                Err(error)
            }
        }
    }

    async fn start(
        opts: Opts,
        events: mpsc::UnboundedReceiver<CallEvent>,
        tasks: Vec<JoinHandle<()>>,
    ) -> Result<Self> {
        let (port, child) = match &opts.browser {
            Browser::Attach(port) => (*port, None),
            Browser::Launch {
                chrome, profile, ..
            } => {
                let port = free_port()?;
                (
                    port,
                    Some(start_chromium(chrome, profile, &opts.role, port)?),
                )
            }
        };
        let ws_path = cdp::browser_socket(port, Duration::from_secs(120)).await?;
        let cdp = Cdp::connect(&ws_path).await?;
        // Attach: the user's browser has its own tabs, so the driver opens a new one. Never the first page target.
        let owned_target = if matches!(opts.browser, Browser::Attach(_)) {
            Some(OwnedTab::create(&cdp, port).await?)
        } else {
            None
        };
        let tab = if let Some(owned) = &owned_target {
            attach_tab(&cdp, owned.id.as_deref().unwrap()).await?
        } else {
            let targets = cdp.call(None, "Target.getTargets", json!({})).await?;
            let target = targets["targetInfos"]
                .as_array()
                .and_then(|t| t.iter().find(|t| t["type"] == "page"))
                .context("the first tab is missing")?["targetId"]
                .as_str()
                .context("the first tab has no target id")?;
            attach_tab(&cdp, target).await?
        };
        Ok(Self {
            cdp,
            child,
            tab,
            opts,
            events,
            tasks,
            owned_target,
        })
    }

    fn attached(&self) -> bool {
        matches!(self.opts.browser, Browser::Attach(_))
    }

    async fn configure(&self, ws_url: String) -> Result<()> {
        let ua: String = self.eval_raw("navigator.userAgent").await?;
        anyhow::ensure!(!ua.is_empty(), "no navigator.userAgent");
        let v = self.cdp.call(None, "Browser.getVersion", json!({})).await?;
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
        let cfg_js = serde_json::json!({
            "ws": ws_url,
            "role": if matches!(self.opts.role, Role::Bot) { "bot" } else { "speaker" },
            "name": self.opts.name,
            "selectors": selectors::load()?,
        });
        // Without `Page.enable` the new-document script did not run in this setup.
        self.cmd("Page.enable", json!({})).await?;
        let source = if self.attached() {
            format!(
                "if (window.top === window && {MEET_ORIGIN}) {{\nwindow.__PARLEY_CFG = {cfg_js};\n{INJECT_JS}\n}}"
            )
        } else {
            format!("window.__PARLEY_CFG = {cfg_js};\n{INJECT_JS}")
        };
        self.cmd(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": source}),
        )
        .await?;
        Ok(())
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
            Ok(Err(_)) if self.attached() => eprintln!("navigate failed; polling the page anyway"),
            Ok(Err(e)) => eprintln!("navigate reply: {e}; polling the page anyway"),
            Err(_) => eprintln!("navigate: no reply in 20s; polling the page anyway"),
        }
    }

    /// Open the Meet link, pass the pre-join screen (a guest types a name), ask to join and wait for the host.
    pub async fn join(&mut self, url: &str, timeout: Duration) -> Result<JoinReport> {
        let start = Instant::now();
        self.navigate(url).await;
        let mut ready = false;
        let mut clicked: Option<Instant> = None;
        let mut state = PageState::default();
        let outcome = loop {
            if start.elapsed() > timeout {
                break Outcome::Timeout;
            }
            state = self.state().await;
            if let Some(outcome) = terminal_outcome(&state.phase) {
                break outcome;
            }
            if state.phase == "prejoin" {
                if !ready {
                    ready = (!self.attached() && self.enter_name().await.is_ok()) || state.can_join;
                    self.set_media(false, matches!(self.opts.role, Role::Bot))
                        .await?;
                } else if state.can_join
                    && clicked.is_none_or(|t| t.elapsed() > Duration::from_secs(20))
                {
                    self.click("join").await?;
                    clicked = Some(Instant::now());
                }
            }
            tokio::time::sleep(Duration::from_millis(700)).await;
        };
        // With no click, the wait ran from the start of the join: report that, not zero.
        let waited_ms = clicked.unwrap_or(start).elapsed().as_millis();
        Ok(JoinReport {
            outcome,
            waited_ms,
            last_state: state,
        })
    }

    pub async fn state(&self) -> PageState {
        const STATE: &str = "window.__parley ? __parley.state() : {phase: 'loading'}";
        if self.attached() {
            let js = format!(
                "{MEET_ORIGIN} ? ({STATE}) : {{phase: location.protocol === 'about:' ? 'loading' : 'offsite'}}"
            );
            self.eval_raw(&js).await.unwrap_or_default()
        } else {
            self.eval_raw(STATE).await.unwrap_or_default()
        }
    }

    pub async fn eval<T: DeserializeOwned>(&self, js: &str) -> Result<T> {
        if self.attached() {
            return self
                .eval_raw(&format!("{MEET_ORIGIN} ? ({js}) : null"))
                .await;
        }
        self.eval_raw(js).await
    }

    async fn eval_raw<T: DeserializeOwned>(&self, js: &str) -> Result<T> {
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

    /// Open the chat panel with a trusted click and give the page script one scan to prime.
    pub async fn open_chat(&self) -> Result<()> {
        if !self.eval::<bool>("__parley.chatOpen()").await? {
            self.click("chatToggle").await?;
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
        Ok(())
    }

    /// Post one chat line. True once the line shows in Meet's message list.
    pub async fn post_chat(&self, text: &str) -> Result<bool> {
        let js_text = serde_json::to_string(text)?;
        self.open_chat().await?;
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

    pub async fn screenshot(&self, path: &Path) -> Result<bool> {
        const SCREENSHOT_DOCUMENT: &str = "window.__parley?.screenshotDocument() ?? null";
        let document = if self.attached() {
            let Some(id) = self.eval::<Option<String>>(SCREENSHOT_DOCUMENT).await? else {
                return Ok(false);
            };
            Some(id)
        } else {
            None
        };
        let capture = self
            .cmd("Page.captureScreenshot", json!({"format": "png"}))
            .await;
        if let Some(before) = document {
            let after: Option<String> = self.eval(SCREENSHOT_DOCUMENT).await?;
            if after.as_ref() != Some(&before) {
                return Ok(false);
            }
        }
        let r = capture?;
        std::fs::write(path, decode_base64(r["data"].as_str().unwrap_or_default())?)?;
        Ok(true)
    }

    /// Next page or audio event, or None after `within`.
    pub async fn next_event(&mut self, within: Duration) -> Option<CallEvent> {
        tokio::time::timeout(within, self.events.recv())
            .await
            .ok()
            .flatten()
    }

    pub async fn leave(mut self) -> Result<()> {
        if self.attached() {
            let mut target = self
                .owned_target
                .take()
                .context("the attached tab is missing")?;
            return tokio::task::spawn_blocking(move || target.close()).await?;
        }
        let _ = self.click("leave").await;
        let _ = self.cdp.call(None, "Browser.close", json!({})).await;
        if let Some(child) = self.child.as_mut() {
            let _ = child.wait().await;
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
            for _ in 0..40 {
                if !matches!(child.try_wait(), Ok(None)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        for t in &self.tasks {
            t.abort();
        }
        if let Browser::Launch {
            profile,
            persistent_profile: false,
            ..
        } = &self.opts.browser
        {
            let _ = std::fs::remove_dir_all(profile);
        }
    }
}

struct OwnedTab {
    port: u16,
    id: Option<String>,
}

impl OwnedTab {
    async fn create(cdp: &Cdp, port: u16) -> Result<Self> {
        let reply = cdp
            .call(None, "Target.createTarget", json!({"url": "about:blank"}))
            .await?;
        let id = reply["targetId"]
            .as_str()
            .filter(|id| {
                !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
            .context("no valid target id for the new tab")?;
        Ok(Self {
            port,
            id: Some(id.to_owned()),
        })
    }

    fn close(&mut self) -> Result<()> {
        use std::io::{BufRead, BufReader, Write};
        use std::net::{Ipv4Addr, SocketAddr, TcpStream};

        let Some(id) = self.id.as_deref() else {
            return Ok(());
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        let mut status = String::new();
        loop {
            let time_left = deadline.saturating_duration_since(Instant::now());
            anyhow::ensure!(
                !time_left.is_zero(),
                "Chromium did not confirm closing tab {id}"
            );
            let mut stream = TcpStream::connect_timeout(&addr, time_left)?;
            stream.set_read_timeout(Some(time_left))?;
            stream.set_write_timeout(Some(time_left))?;
            write!(
                stream,
                "GET /json/close/{id} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
            )?;
            status.clear();
            BufReader::new(stream).read_line(&mut status)?;
            match status.split_ascii_whitespace().nth(1) {
                Some("404") => {
                    self.id = None;
                    return Ok(());
                }
                Some("200") => std::thread::sleep(Duration::from_millis(100)),
                _ => bail!(
                    "Chromium did not accept closing tab {id}: {}",
                    status.trim()
                ),
            }
        }
    }
}

impl Drop for OwnedTab {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            eprintln!("Could not close the attached tab: {error:#}");
        }
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

/// Attach to a tab by target id and return its CDP session id.
async fn attach_tab(cdp: &Cdp, target: &str) -> Result<String> {
    Ok(cdp
        .call(
            None,
            "Target.attachToTarget",
            json!({"targetId": target, "flatten": true}),
        )
        .await?["sessionId"]
        .as_str()
        .context("no session id for the tab")?
        .to_string())
}

fn start_chromium(
    chrome: &Path,
    profile: &Path,
    role: &Role,
    port: u16,
) -> Result<tokio::process::Child> {
    // The profile holds the browser's cookies; a persistent one may already exist (see `check_profile`).
    std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
        .create(profile)?;
    std::fs::set_permissions(profile, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    // A fixed port, not 0: Chromium treats port 0 as automation mode and sets `navigator.webdriver` to true.
    // No `--enable-automation`, no stealth flags: a plain Chromium with a window and fake media devices.
    let mut cmd = tokio::process::Command::new(chrome);
    cmd.arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--remote-debugging-port={port}"))
        .args([
            "--window-size=1280,800",
            "--no-first-run",
            "--no-default-browser-check",
            "--use-fake-ui-for-media-stream",
            "--use-fake-device-for-media-stream",
            "--autoplay-policy=no-user-gesture-required",
            "--lang=en-US",
            "--accept-lang=en-US",
        ]);
    if let Role::Speaker { audio } = role {
        cmd.arg(format!(
            "--use-file-for-fake-audio-capture={}",
            audio.display()
        ));
    }
    cmd.arg("about:blank")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("launching Chromium")
}

/// A port the kernel just handed out; Chromium binds it a moment later.
fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

/// The outcome a page phase ends the join with, or None while the join keeps waiting.
fn terminal_outcome(phase: &str) -> Option<Outcome> {
    Some(match phase {
        "in_call" => Outcome::Admitted,
        "denied" => Outcome::Denied,
        "refused" => Outcome::Refused,
        "expired" => Outcome::Expired,
        "not_found" => Outcome::NotFound,
        "offsite" => Outcome::Offsite,
        _ => return None,
    })
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
    fn terminal_outcome_ends_only_on_end_phases() {
        assert_eq!(
            super::terminal_outcome("in_call"),
            Some(super::Outcome::Admitted)
        );
        assert_eq!(
            super::terminal_outcome("offsite"),
            Some(super::Outcome::Offsite)
        );
        // A signed-in join passes the prejoin and the loading phases on its way to the call.
        for phase in ["prejoin", "loading", "waiting", ""] {
            assert_eq!(super::terminal_outcome(phase), None, "phase {phase:?}");
        }
    }

    async fn mock_attached_session(
        mut respond: impl FnMut(&super::Value) -> super::Value + Send + 'static,
    ) -> (super::Session, super::JoinHandle<()>) {
        use super::*;
        use futures_util::SinkExt;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let request: Value = serde_json::from_str(&text).unwrap();
                let result = respond(&request);
                ws.send(Message::text(
                    json!({"id": request["id"], "result": result}).to_string(),
                ))
                .await
                .unwrap();
            }
        });
        let session = Session {
            cdp: Cdp::connect(&format!("ws://127.0.0.1:{port}"))
                .await
                .unwrap(),
            child: None,
            tab: "tab-session".into(),
            opts: Opts {
                browser: Browser::Attach(port),
                name: "Test".into(),
                role: Role::Bot,
            },
            events: mpsc::unbounded_channel().1,
            tasks: Vec::new(),
            owned_target: None,
        };
        (session, browser)
    }

    #[tokio::test]
    async fn signed_in_lobby_clicks_join_now_without_entering_a_name() {
        use super::*;

        let mut pressed = false;
        let mut joined = false;
        let (mut session, browser) = mock_attached_session(move |request| {
            let params = &request["params"];
            match request["method"].as_str().unwrap() {
                "Page.navigate" => json!({}),
                "Runtime.evaluate" => {
                    let expression = params["expression"].as_str().unwrap();
                    let value = if expression.contains("__parley.state()") {
                        if joined {
                            json!({"phase": "in_call"})
                        } else {
                            json!({"phase": "prejoin", "can_join": true})
                        }
                    } else if expression.contains("__parley.media()") {
                        json!({"mic": false, "cam": true})
                    } else if expression.contains("__parley.point('join')") {
                        json!({"x": 30, "y": 35})
                    } else {
                        panic!("unexpected evaluation: {expression}");
                    };
                    json!({"result": {"value": value}})
                }
                "Input.dispatchMouseEvent" => {
                    assert_eq!(params["x"], 30.0);
                    assert_eq!(params["y"], 35.0);
                    match params["type"].as_str().unwrap() {
                        "mouseMoved" => {}
                        "mousePressed" => {
                            assert!(!pressed && !joined);
                            assert_eq!(params["button"], "left");
                            assert_eq!(params["buttons"], 1);
                            pressed = true;
                        }
                        "mouseReleased" => {
                            assert!(pressed && !joined);
                            assert_eq!(params["button"], "left");
                            assert_eq!(params["buttons"], 0);
                            pressed = false;
                            joined = true;
                        }
                        kind => panic!("unexpected mouse event: {kind}"),
                    }
                    json!({})
                }
                method => panic!("unexpected browser command: {method}"),
            }
        })
        .await;
        let report = session
            .join(
                "https://meet.google.com/abc-defg-hij",
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(report.outcome, Outcome::Admitted);
        drop(session);
        browser.await.unwrap();
    }

    #[tokio::test]
    async fn attached_screenshot_is_saved_only_for_an_unchanged_meet_document() {
        use super::*;

        let root =
            std::env::temp_dir().join(format!("parley-screenshot-{}", random_token().unwrap()));
        std::fs::create_dir(&root).unwrap();
        for (index, (before, after, saved)) in [
            (None, None, false),
            (Some("meet-a"), None, false),
            (Some("meet-a"), Some("meet-b"), false),
            (Some("meet-a"), Some("meet-a"), true),
        ]
        .into_iter()
        .enumerate()
        {
            let mut active = before;
            let (session, browser) = mock_attached_session(move |request| {
                match request["method"].as_str().unwrap() {
                    "Runtime.evaluate" => json!({"result": {"value": active}}),
                    "Page.captureScreenshot" => {
                        assert!(active.is_some(), "off-site screenshot requested");
                        active = after;
                        json!({"data": "iVBORw0KGgo="})
                    }
                    method => panic!("unexpected browser command: {method}"),
                }
            })
            .await;
            let path = root.join(format!("{index}.png"));
            assert_eq!(session.screenshot(&path).await.unwrap(), saved);
            if saved {
                assert_eq!(std::fs::read(path).unwrap(), b"\x89PNG\r\n\x1a\n");
            } else {
                assert!(!path.exists());
            }
            drop(session);
            browser.await.unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn attached_tab_drop_waits_for_its_target_to_disappear() {
        use super::*;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser = tokio::spawn(async move {
            for status in ["200 OK", "404 Not Found"] {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                assert_eq!(line, "GET /json/close/owned-tab HTTP/1.1\r\n");
                while line != "\r\n" {
                    line.clear();
                    assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
                }
                let reply = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n");
                let stream = stream.get_mut();
                stream.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        tokio::task::spawn_blocking(move || {
            drop(OwnedTab {
                port,
                id: Some("owned-tab".into()),
            })
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), browser)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn failed_launch_keeps_persistent_profile_and_removes_temporary_profile() {
        if std::env::var_os("PARLEY_PROFILE_TEST_CHILD").is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::failed_launch_keeps_persistent_profile_and_removes_temporary_profile",
                ])
                .env("PARLEY_PROFILE_TEST_CHILD", "1")
                .env("DISPLAY", ":999")
                .status()
                .unwrap();
            assert!(result.success());
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "parley-profile-test-{}-{}",
            std::process::id(),
            super::random_token().unwrap()
        ));
        for persistent_profile in [true, false] {
            let profile = root.join(persistent_profile.to_string());
            std::fs::create_dir_all(&profile).unwrap();
            std::fs::write(profile.join("existing-session"), b"test").unwrap();
            let result = super::Session::launch(super::Opts {
                browser: super::Browser::Launch {
                    chrome: root.join("missing-browser"),
                    profile: profile.clone(),
                    persistent_profile,
                },
                name: "test".into(),
                role: super::Role::Bot,
            })
            .await;
            assert!(result.is_err());
            assert_eq!(profile.exists(), persistent_profile);
            if persistent_profile {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::read(profile.join("existing-session")).unwrap(),
                    b"test"
                );
                assert_eq!(
                    std::fs::metadata(&profile).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn check_profile_rejects_repository_and_arbitrary_directories_without_changing_them() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "parley-check-profile-{}-{}",
            std::process::id(),
            super::random_token().unwrap()
        ));
        let other = root.join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("notes.txt"), b"x").unwrap();
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(super::check_profile(&other).is_err());
        assert_eq!(
            std::fs::metadata(&other).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(std::fs::read_dir(&other).unwrap().count() == 1);

        let repo_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bot-profile");
        assert!(super::check_profile(&repo_path).is_err());
        assert!(!repo_path.exists());

        let chromium = root.join("chromium");
        std::fs::create_dir_all(&chromium).unwrap();
        std::fs::write(chromium.join("Local State"), b"{}").unwrap();
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        for ok in [chromium, empty, root.join("new/profile")] {
            super::check_profile(&ok).unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn data_url_decodes_to_the_png_bytes() {
        // "\x89PNG" is iVBORw==
        assert_eq!(
            super::decode_data_url("data:image/png;base64,iVBORw==").unwrap(),
            [0x89, b'P', b'N', b'G']
        );
    }
}
