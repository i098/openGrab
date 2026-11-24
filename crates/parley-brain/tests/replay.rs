//! `notes()` against a local HTTP server that replays a recorded SSE response.

use parley_brain::{ActionItem, BrainError, Line, Transcript};
use parley_llm::openai::OpenAiCompatible;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serves `body` as an SSE response to every connection; returns the base URL.
/// Each request body is sent on `seen`.
async fn serve(body: String, seen: tokio::sync::mpsc::UnboundedSender<String>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = l.accept().await.unwrap();
            let (body, seen) = (body.clone(), seen.clone());
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
                seen.send(String::from_utf8_lossy(&buf[head_end..head_end + len]).into_owned())
                    .ok();
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

async fn replay_request(file: &str) -> (Result<parley_brain::Notes, BrainError>, String) {
    let body = std::fs::read_to_string(format!(
        "{}/tests/fixtures/{file}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let llm = OpenAiCompatible::new(serve(body, tx).await, None, "m");
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
    let notes = t.notes(&llm).await;
    (notes, rx.recv().await.unwrap())
}

async fn replay(file: &str) -> Result<parley_brain::Notes, BrainError> {
    replay_request(file).await.0
}

/// OpenAI chat-completions contract: `tool_choice` naming a function forces that call.
#[tokio::test]
async fn notes_forces_the_write_notes_tool() {
    let (_, request) = replay_request("notes_items.sse").await;
    let request: serde_json::Value = serde_json::from_str(&request).unwrap();
    assert_eq!(
        request["tool_choice"],
        serde_json::json!({"type": "function", "function": {"name": "write_notes"}})
    );
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

async fn ask(sse: &str, question: &str) -> (String, String) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let llm = OpenAiCompatible::new(serve(sse.into(), tx).await, None, "m");
    let mut t = Transcript::new();
    t.push(Line {
        speaker: "Tom".into(),
        start_ms: 1000,
        text: "I will send the deck.".into(),
    });
    let a = t.answer(&llm, question).await.unwrap();
    (a, rx.recv().await.unwrap())
}

#[tokio::test]
async fn answer_is_plain_text_from_the_transcript() {
    let sse = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Tom \"}}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"sends the deck.\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    let (a, req) = ask(sse, "what did Tom agree to?").await;
    assert_eq!(a, "Tom sends the deck.");
    let req: serde_json::Value = serde_json::from_str(&req).unwrap();
    assert!(
        req.get("tools")
            .is_none_or(|t| t.as_array().unwrap().is_empty())
    );
    let body = req["messages"].to_string();
    assert!(body.contains("I will send the deck.") && body.contains("what did Tom agree to?"));
}

#[tokio::test]
async fn answer_is_cut_at_500_chars_on_a_char_boundary() {
    let long = "é".repeat(700);
    let sse = format!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{long}\"}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
    );
    let (a, _) = ask(&sse, "q").await;
    assert_eq!(a.chars().count(), 500);
}
