//! `notes()` against a local HTTP server that replays a recorded SSE response.

use parley_brain::{ActionItem, BrainError, Line, Transcript};
use parley_llm::openai::OpenAiCompatible;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serves `body` as an SSE response to every connection; returns the base URL.
async fn serve(body: String) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = l.accept().await.unwrap();
            let body = body.clone();
            tokio::spawn(async move {
                // Read request head and body (Content-Length) so the client finishes sending.
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let (head_end, len) = loop {
                    let n = sock.read(&mut tmp).await.unwrap();
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..p]).to_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .map_or(0, |v| v.trim().parse::<usize>().unwrap());
                        break (p + 4, len);
                    }
                    assert!(n > 0);
                };
                while buf.len() < head_end + len {
                    let n = sock.read(&mut tmp).await.unwrap();
                    buf.extend_from_slice(&tmp[..n]);
                }
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
                sock.shutdown().await.ok();
            });
        }
    });
    url
}

async fn replay(file: &str) -> Result<parley_brain::Notes, BrainError> {
    let body = std::fs::read_to_string(format!(
        "{}/tests/fixtures/{file}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let llm = OpenAiCompatible::new(serve(body).await, None, "m");
    let mut t = Transcript::new();
    // Pushed out of order: the transcript keeps start-time order.
    for (who, ms, text) in [("Bo", 4000, "Fine."), ("Ana", 1000, "Ship Friday?")] {
        t.push(Line {
            speaker: who.into(),
            start_ms: ms,
            text: text.into(),
        });
    }
    assert_eq!(t.lines()[0].speaker, "Ana");
    t.notes(&llm).await
}

#[tokio::test]
async fn notes_with_action_items() {
    let n = replay("notes_items.sse").await.unwrap();
    assert_eq!(n.summary, "Ship the beta on Friday.");
    assert_eq!(
        n.action_items,
        vec![
            ActionItem {
                text: "Write release notes".into(),
                owner: Some("Ana".into())
            },
            ActionItem {
                text: "Book the demo room".into(),
                owner: None
            },
        ]
    );
}

#[tokio::test]
async fn notes_without_action_items() {
    let n = replay("notes_none.sse").await.unwrap();
    assert_eq!(n.summary, "Small talk only.");
    assert!(n.action_items.is_empty());
}

#[tokio::test]
async fn no_tool_call_is_an_error() {
    assert!(matches!(
        replay("notes_no_tool.sse").await,
        Err(BrainError::NoToolCall)
    ));
}
