//! Meeting page server. One meeting, one secret URL `/m/<secret>`.
//! The server owns the state (running, paused, removed) and keeps only transcript text and the live doc in memory.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use axum::{
    Json, Router,
    extract::{
        Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use parley_artifacts::{Artifact, Span};
use serde::Serialize;
use subtle::ConstantTimeEq;
use tokio::sync::{broadcast, mpsc};

const PAGE: &str = include_str!("../../../web/index.html");
/// Built from `web/editor` with `bun run build`.
const EDITOR_JS: &str = include_str!("../../../web/editor.js");
/// Largest Yjs update one client may send.
const MAX_DOC_MESSAGE: usize = 1 << 20;

/// A Yjs update and the id of the client that sent it (0 for the server).
#[derive(Clone)]
enum DocEvent {
    /// A Yjs update with the id of the client that sent it (0 for the server).
    Update(u64, Arc<Vec<u8>>),
    /// The meeting was removed: close the socket.
    Closed,
}

/// One transcript line.
#[derive(Clone, Debug, Serialize)]
pub struct Line {
    pub speaker: String,
    pub text: String,
    /// Unix time in milliseconds.
    pub at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    Running,
    Paused,
    Removed,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Event {
    Line(Line),
    State { state: SessionState },
}

struct Inner {
    state: SessionState,
    lines: Vec<Line>,
}

/// The meeting: secret, state and transcript.
pub struct Meeting {
    secret: String,
    inner: Mutex<Inner>,
    events: broadcast::Sender<Event>,
    /// The live doc, None once removed. Held while an update is applied and sent, so a joiner misses none.
    doc: Mutex<Option<Artifact>>,
    doc_events: broadcast::Sender<DocEvent>,
    next_client: AtomicU64,
}

/// 128 random bits from the OS, URL-safe base64 (22 characters).
pub fn new_secret() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("OS random source");
    URL_SAFE_NO_PAD.encode(b)
}

impl Meeting {
    pub fn new(secret: String) -> Arc<Self> {
        Arc::new(Self {
            secret,
            inner: Mutex::new(Inner {
                state: SessionState::Running,
                lines: Vec::new(),
            }),
            events: broadcast::channel(1024).0,
            doc: Mutex::new(Some(Artifact::new())),
            doc_events: broadcast::channel(1024).0,
            next_client: AtomicU64::new(1),
        })
    }

    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// Constant-time secret check.
    pub fn check_secret(&self, candidate: &str) -> bool {
        self.secret.as_bytes().ct_eq(candidate.as_bytes()).into()
    }

    pub fn state(&self) -> SessionState {
        self.inner.lock().unwrap().state
    }

    /// Add a line. Dropped when paused or removed. Returns whether it was kept.
    pub fn push(&self, line: Line) -> bool {
        let mut g = self.inner.lock().unwrap();
        if g.state != SessionState::Running {
            return false;
        }
        g.lines.push(line.clone());
        // Sent under the lock so the order matches the history.
        let _ = self.events.send(Event::Line(line));
        true
    }

    /// Pause: only from running. Returns the state after the call.
    pub fn pause(&self) -> SessionState {
        self.transition(SessionState::Running, SessionState::Paused)
    }

    /// Resume: only from paused.
    pub fn resume(&self) -> SessionState {
        self.transition(SessionState::Paused, SessionState::Running)
    }

    /// Remove: final. Drops the transcript and the live doc from memory.
    pub fn remove(&self) -> SessionState {
        let mut g = self.inner.lock().unwrap();
        if g.state != SessionState::Removed {
            g.state = SessionState::Removed;
            g.lines = Vec::new();
            let mut doc = self.doc.lock().unwrap();
            *doc = None;
            let _ = self.doc_events.send(DocEvent::Closed);
            let _ = self.events.send(Event::State {
                state: SessionState::Removed,
            });
        }
        g.state
    }

    fn transition(&self, from: SessionState, to: SessionState) -> SessionState {
        let mut g = self.inner.lock().unwrap();
        if g.state == from {
            g.state = to;
            let _ = self.events.send(Event::State { state: to });
        }
        g.state
    }

    /// Snapshot and live feed, taken under one lock so no line is lost or doubled.
    fn subscribe(&self) -> (String, broadcast::Receiver<Event>) {
        let g = self.inner.lock().unwrap();
        let hello = serde_json::json!({"type": "history", "state": g.state, "lines": g.lines});
        (hello.to_string(), self.events.subscribe())
    }

    /// Server-side edit of the live doc; every open page shows it at once.
    pub fn append_paragraph(&self, text: &str) {
        let guard = self.doc.lock().unwrap();
        let Some(doc) = guard.as_ref() else { return };
        doc.append_paragraph(&[Span::plain(text)]);
        // The full state is a valid update for every client: Yjs ignores what it has.
        let _ = self
            .doc_events
            .send(DocEvent::Update(0, Arc::new(doc.encode_update())));
    }

    /// The live doc as Tiptap JSON.
    pub fn doc_json(&self) -> serde_json::Value {
        self.doc
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|d| d.to_json().ok())
            .unwrap_or_default()
    }

    /// Apply a client update and pass it on. Err means the update is invalid.
    fn doc_update(&self, from: u64, update: Vec<u8>) -> Result<(), parley_artifacts::Error> {
        let guard = self.doc.lock().unwrap();
        let Some(doc) = guard.as_ref() else {
            return Ok(());
        };
        doc.apply_update(&update)?;
        let _ = self
            .doc_events
            .send(DocEvent::Update(from, Arc::new(update)));
        Ok(())
    }

    /// Full state and live updates, taken under one lock. None once removed.
    fn doc_subscribe(&self) -> Option<(Vec<u8>, broadcast::Receiver<DocEvent>)> {
        let guard = self.doc.lock().unwrap();
        let doc = guard.as_ref()?;
        Some((doc.encode_update(), self.doc_events.subscribe()))
    }

    /// Copy of the kept lines.
    pub fn lines(&self) -> Vec<Line> {
        self.inner.lock().unwrap().lines.clone()
    }
}

/// Build the router and the channel any source feeds lines into.
pub fn app(meeting: Arc<Meeting>) -> (Router, mpsc::Sender<Line>) {
    let (tx, mut rx) = mpsc::channel::<Line>(256);
    let m = meeting.clone();
    tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            m.push(line);
        }
    });
    let router = Router::new()
        .route("/m/{secret}", get(page))
        .route("/m/{secret}/ws", get(ws))
        .route("/m/{secret}/editor.js", get(editor_js))
        .route("/m/{secret}/doc/ws", get(doc_ws))
        .route("/m/{secret}/{action}", post(action))
        .with_state(meeting);
    (router, tx)
}

fn authorize(m: &Meeting, secret: &str) -> Result<(), StatusCode> {
    if m.check_secret(secret) {
        Ok(())
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn page(State(m): State<Arc<Meeting>>, Path(secret): Path<String>) -> Response {
    match authorize(&m, &secret) {
        Ok(()) => Html(PAGE).into_response(),
        Err(c) => c.into_response(),
    }
}

async fn editor_js(State(m): State<Arc<Meeting>>, Path(secret): Path<String>) -> Response {
    match authorize(&m, &secret) {
        Ok(()) => ([("content-type", "text/javascript")], EDITOR_JS).into_response(),
        Err(c) => c.into_response(),
    }
}

async fn doc_ws(
    State(m): State<Arc<Meeting>>,
    Path(secret): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Response {
    match authorize(&m, &secret) {
        Ok(()) if m.state() == SessionState::Removed => StatusCode::NOT_FOUND.into_response(),
        Ok(()) => upgrade
            .max_message_size(MAX_DOC_MESSAGE)
            .on_upgrade(move |s| doc_feed(s, m)),
        Err(c) => c.into_response(),
    }
}

async fn doc_feed(mut socket: WebSocket, m: Arc<Meeting>) {
    let id = m.next_client.fetch_add(1, Ordering::Relaxed);
    let Some((state, mut rx)) = m.doc_subscribe() else {
        return;
    };
    if socket.send(Message::binary(state)).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(DocEvent::Update(from, update)) => {
                    if from != id && socket.send(Message::binary(update.to_vec())).await.is_err() {
                        return;
                    }
                }
                Ok(DocEvent::Closed) => {
                    let _ = socket.send(Message::Close(None)).await;
                    return;
                }
                // Missed updates: resend the full state.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let Some((state, r)) = m.doc_subscribe() else {
                        return;
                    };
                    rx = r;
                    if socket.send(Message::binary(state)).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(b))) => {
                    if m.doc_update(id, b.to_vec()).is_err() {
                        return;
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

async fn action(
    State(m): State<Arc<Meeting>>,
    Path((secret, action)): Path<(String, String)>,
) -> Response {
    if let Err(c) = authorize(&m, &secret) {
        return c.into_response();
    }
    let state = match action.as_str() {
        "pause" => m.pause(),
        "resume" => m.resume(),
        "remove" => m.remove(),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    Json(serde_json::json!({ "state": state })).into_response()
}

async fn ws(
    State(m): State<Arc<Meeting>>,
    Path(secret): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Response {
    match authorize(&m, &secret) {
        Ok(()) => upgrade.on_upgrade(move |s| feed(s, m)),
        Err(c) => c.into_response(),
    }
}

async fn feed(mut socket: WebSocket, m: Arc<Meeting>) {
    let (hello, mut rx) = m.subscribe();
    if socket.send(Message::text(hello)).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) => {
                    let text = serde_json::to_string(&ev).expect("event serializes");
                    if socket.send(Message::text(text)).await.is_err() {
                        return;
                    }
                }
                // Slow reader missed events: send a fresh snapshot.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let (hello, r) = m.subscribe();
                    rx = r;
                    if socket.send(Message::text(hello)).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
