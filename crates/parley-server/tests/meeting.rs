use std::sync::Arc;

use futures_util::StreamExt;
use parley_server::{Line, Meeting, SessionState, app, new_secret, now_ms};
use tokio_tungstenite::tungstenite::Message;

fn line(text: &str) -> Line {
    Line {
        speaker: "Ana".into(),
        text: text.into(),
        at_ms: now_ms(),
    }
}

#[test]
fn secret_is_long_url_safe_and_checked() {
    let s = new_secret();
    assert!(s.len() >= 22, "128 bits need 22 base64 characters");
    assert!(
        s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    );
    assert_ne!(s, new_secret());
    let m = Meeting::new(s.clone());
    assert!(m.check_secret(&s));
    assert!(!m.check_secret("wrong"));
    assert!(!m.check_secret(&s[..s.len() - 1]));
    assert!(!m.check_secret(""));
}

#[test]
fn pause_resume_remove() {
    let m = Meeting::new(new_secret());
    assert!(m.push(line("one")));
    assert_eq!(m.pause(), SessionState::Paused);
    assert!(!m.push(line("dropped")));
    assert_eq!(m.resume(), SessionState::Running);
    assert!(m.push(line("two")));
    assert_eq!(m.lines().len(), 2);
    assert_eq!(m.remove(), SessionState::Removed);
    assert!(m.lines().is_empty());
    // Remove is final.
    assert_eq!(m.resume(), SessionState::Removed);
    assert_eq!(m.pause(), SessionState::Removed);
    assert!(!m.push(line("late")));
}

async fn serve(m: Arc<Meeting>) -> (String, tokio::sync::mpsc::Sender<Line>) {
    let (router, tx) = app(m);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    (addr.to_string(), tx)
}

async fn http(addr: &str, method: &str, path: &str) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let code = out[9..12].parse().unwrap();
    (code, out)
}

#[tokio::test]
async fn http_routes_check_secret_and_control_state() {
    let m = Meeting::new(new_secret());
    let sec = m.secret().to_string();
    let (addr, _tx) = serve(m.clone()).await;
    assert_eq!(http(&addr, "GET", "/m/wrong").await.0, 404);
    assert_eq!(http(&addr, "POST", "/m/wrong/pause").await.0, 404);
    let (code, body) = http(&addr, "GET", &format!("/m/{sec}")).await;
    assert_eq!(code, 200);
    assert!(body.contains("Pause"));
    let (code, body) = http(&addr, "POST", &format!("/m/{sec}/pause")).await;
    assert_eq!(code, 200);
    assert!(body.contains("\"paused\""));
    assert_eq!(m.state(), SessionState::Paused);
    http(&addr, "POST", &format!("/m/{sec}/remove")).await;
    assert_eq!(m.state(), SessionState::Removed);
}

#[tokio::test]
async fn websocket_receives_a_line_and_state() {
    let m = Meeting::new(new_secret());
    let sec = m.secret().to_string();
    let (addr, tx) = serve(m).await;
    assert!(
        tokio_tungstenite::connect_async(format!("ws://{addr}/m/wrong/ws"))
            .await
            .is_err()
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/m/{sec}/ws"))
        .await
        .unwrap();
    let Message::Text(t) = ws.next().await.unwrap().unwrap() else {
        panic!("text")
    };
    assert!(t.contains("\"history\""));
    tx.send(line("hello there")).await.unwrap();
    let Message::Text(t) = ws.next().await.unwrap().unwrap() else {
        panic!("text")
    };
    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
    assert_eq!(v["type"], "line");
    assert_eq!(v["text"], "hello there");
    assert_eq!(v["speaker"], "Ana");
    http(&addr, "POST", &format!("/m/{sec}/pause")).await;
    let Message::Text(t) = ws.next().await.unwrap().unwrap() else {
        panic!("text")
    };
    assert!(t.contains("\"paused\""));
}

#[tokio::test]
async fn live_doc_converges_for_two_clients_and_a_server_edit() {
    use parley_artifacts::{Artifact, Span};
    let m = Meeting::new(new_secret());
    let sec = m.secret().to_string();
    let (addr, _tx) = serve(m.clone()).await;
    let url = format!("ws://{addr}/m/{sec}/doc/ws");
    assert!(
        tokio_tungstenite::connect_async(format!("ws://{addr}/m/wrong/doc/ws"))
            .await
            .is_err()
    );
    let (mut a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let local_a = Artifact::new();
    let local_b = Artifact::new();
    for (ws, local) in [(&mut a, &local_a), (&mut b, &local_b)] {
        let Message::Binary(s) = ws.next().await.unwrap().unwrap() else {
            panic!("binary")
        };
        local.apply_update(&s).unwrap();
    }

    // Client A types; client B receives it.
    use futures_util::SinkExt;
    let typed = Artifact::new();
    typed.append_paragraph(&[Span::plain("from a")]);
    a.send(Message::binary(typed.encode_update()))
        .await
        .unwrap();
    let Message::Binary(u) = b.next().await.unwrap().unwrap() else {
        panic!("binary")
    };
    local_b.apply_update(&u).unwrap();

    // Server edit reaches both.
    m.append_paragraph("from server");
    for (ws, local) in [(&mut a, &local_a), (&mut b, &local_b)] {
        let Message::Binary(u) = ws.next().await.unwrap().unwrap() else {
            panic!("binary")
        };
        local.apply_update(&u).unwrap();
    }
    local_a.apply_update(&typed.encode_update()).unwrap();

    let want = m.doc_json();
    assert_eq!(local_a.to_json().unwrap(), want);
    assert_eq!(local_b.to_json().unwrap(), want);
    let text = want.to_string();
    assert!(text.contains("from a") && text.contains("from server"));
}

#[tokio::test]
async fn removed_meeting_closes_doc_sockets_and_refuses_new_ones() {
    let m = Meeting::new(new_secret());
    let sec = m.secret().to_string();
    let (addr, _tx) = serve(m.clone()).await;
    let url = format!("ws://{addr}/m/{sec}/doc/ws");
    let (mut a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    a.next().await.unwrap().unwrap();
    m.append_paragraph("secret words");
    a.next().await.unwrap().unwrap();
    assert!(m.doc_json().to_string().contains("secret words"));

    m.remove();

    assert!(matches!(
        a.next().await,
        Some(Ok(Message::Close(_))) | Some(Err(_)) | None
    ));
    assert!(tokio_tungstenite::connect_async(&url).await.is_err());
    assert!(!m.doc_json().to_string().contains("secret words"));
}

fn board(labels: &[&str]) -> parley_artifacts::board::Board {
    use parley_artifacts::board::{Board, Edge, Node};
    Board {
        nodes: labels
            .iter()
            .map(|l| Node {
                id: l.to_lowercase(),
                label: (*l).into(),
            })
            .collect(),
        edges: labels
            .windows(2)
            .map(|w| Edge {
                from: w[0].to_lowercase(),
                to: w[1].to_lowercase(),
                label: None,
            })
            .collect(),
        groups: vec![],
    }
}

/// Element ids in order. Whole values differ in how Yjs stores a whole number (int or float).
fn ids(elements: &[serde_json::Value]) -> Vec<String> {
    elements.iter().map(|e| e["id"].to_string()).collect()
}

#[tokio::test]
async fn board_converges_for_two_clients_and_a_server_edit() {
    use futures_util::SinkExt;
    use parley_artifacts::board::BoardDoc;
    let m = Meeting::new(new_secret());
    let sec = m.secret().to_string();
    let (addr, _tx) = serve(m.clone()).await;
    let url = format!("ws://{addr}/m/{sec}/board/ws");
    assert!(
        tokio_tungstenite::connect_async(format!("ws://{addr}/m/wrong/board/ws"))
            .await
            .is_err()
    );
    m.set_board(&board(&["Api", "Store"])).unwrap();
    let (mut a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let mut locals = Vec::new();
    for ws in [&mut a, &mut b] {
        let Message::Binary(s) = ws.next().await.unwrap().unwrap() else {
            panic!("binary")
        };
        locals.push(BoardDoc::from_update(&s).unwrap());
    }
    let local_b = locals.pop().unwrap();
    let local_a = locals.pop().unwrap();
    assert_eq!(ids(&local_a.elements().unwrap()), ids(&m.board_elements()));
    let before = m.board_elements();
    assert!(!before.is_empty());

    // Client A adds a shape; client B receives it.
    let mut drawn = local_a.elements().unwrap();
    drawn.push(serde_json::json!({"type": "rectangle", "id": "drawn", "x": 1, "y": 2}));
    local_a.set_elements(&drawn);
    a.send(Message::binary(local_a.encode_update()))
        .await
        .unwrap();
    let Message::Binary(u) = b.next().await.unwrap().unwrap() else {
        panic!("binary")
    };
    local_b.apply_update(&u).unwrap();
    assert_eq!(ids(&local_b.elements().unwrap()), ids(&drawn));
    assert_eq!(ids(&m.board_elements()), ids(&drawn));

    // Server edit reaches both.
    m.set_board(&board(&["Api", "Queue", "Store"])).unwrap();
    for (ws, local) in [(&mut a, &local_a), (&mut b, &local_b)] {
        let Message::Binary(u) = ws.next().await.unwrap().unwrap() else {
            panic!("binary")
        };
        local.apply_update(&u).unwrap();
    }
    let want = m.board_elements();
    assert_ne!(ids(&want), ids(&drawn));
    assert_eq!(ids(&local_a.elements().unwrap()), ids(&want));
    assert_eq!(ids(&local_b.elements().unwrap()), ids(&want));
    assert!(want.iter().any(|e| e["id"] == "text:queue"));
}

#[tokio::test]
async fn removed_meeting_closes_board_sockets_and_clears_the_board() {
    let m = Meeting::new(new_secret());
    let sec = m.secret().to_string();
    let (addr, _tx) = serve(m.clone()).await;
    let url = format!("ws://{addr}/m/{sec}/board/ws");
    let (mut a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    a.next().await.unwrap().unwrap();
    m.set_board(&board(&["Api"])).unwrap();
    a.next().await.unwrap().unwrap();
    assert!(!m.board_elements().is_empty());

    m.remove();

    assert!(matches!(
        a.next().await,
        Some(Ok(Message::Close(_))) | Some(Err(_)) | None
    ));
    assert!(tokio_tungstenite::connect_async(&url).await.is_err());
    assert!(m.board_elements().is_empty());
}

/// Replays one scripted answer per call; an `Err` script fails the call.
struct Stub(std::sync::Mutex<Vec<Result<Vec<parley_llm::Delta>, String>>>);

impl parley_llm::LlmProvider for Stub {
    async fn stream(
        &self,
        _req: parley_llm::ChatRequest,
    ) -> Result<parley_llm::DeltaStream, parley_llm::LlmError> {
        let script = self.0.lock().unwrap().remove(0);
        let deltas = script.map_err(parley_llm::LlmError::Decode)?;
        Ok(Box::pin(futures_util::stream::iter(
            deltas.into_iter().map(Ok),
        )))
    }
}

fn call(name: &str, arguments: serde_json::Value) -> parley_llm::Delta {
    parley_llm::Delta::ToolCall {
        id: "c".into(),
        name: name.into(),
        arguments,
    }
}

fn texts(m: &Meeting) -> String {
    m.doc_json().to_string()
}

#[tokio::test]
async fn request_creates_then_extends_the_doc_and_a_failure_changes_nothing() {
    use parley_llm::{Delta, FinishReason};
    use serde_json::json;
    let m = Meeting::new(new_secret());
    m.push(line("we should plan the launch"));
    let (addr, _tx) = serve(m.clone()).await;
    // A page that is open before the request must see the edit with no reload.
    let url = format!("ws://{addr}/m/{}/doc/ws", m.secret());
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let page = parley_artifacts::Artifact::new();
    let stub = Stub(std::sync::Mutex::new(vec![
        Ok(vec![
            call("doc_create", json!({"title": "Launch plan"})),
            Delta::Text("Started the doc.".into()),
            Delta::End(FinishReason::ToolCalls),
        ]),
        Ok(vec![
            call(
                "doc_append",
                json!({"type": "paragraph", "text": "Ship on Friday"}),
            ),
            Delta::End(FinishReason::ToolCalls),
        ]),
        Err("model down".into()),
        Ok(vec![
            Delta::Text("no tool call".into()),
            Delta::End(FinishReason::Stop),
        ]),
    ]));

    assert_eq!(m.request(&stub, "Parley", "Ana: hello").await, Ok(None));
    let chat = m.request(&stub, "Parley", "Parley, start a doc").await;
    assert_eq!(chat, Ok(Some("Started the doc.".into())));
    assert!(texts(&m).contains("Launch plan"));
    let chat = m.request(&stub, "Parley", "Parley, add the date").await;
    assert_eq!(chat, Ok(Some("Done.".into())));
    let both = texts(&m);
    assert!(both.contains("Launch plan") && both.contains("Ship on Friday"));

    assert!(m.request(&stub, "Parley", "Parley, again").await.is_err());
    assert!(m.request(&stub, "Parley", "Parley, again").await.is_err());
    assert_eq!(texts(&m), both, "a failed call leaves the doc unchanged");

    // The open page received the full state of both edits over the doc socket.
    while let Ok(Some(Ok(Message::Binary(b)))) =
        tokio::time::timeout(std::time::Duration::from_millis(300), ws.next()).await
    {
        page.apply_update(&b).unwrap();
    }
    assert_eq!(page.to_json().unwrap().to_string(), both);
}
