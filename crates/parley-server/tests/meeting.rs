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
