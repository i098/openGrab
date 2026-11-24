//! Each adapter talks to a local HTTP server that replays a recorded SSE response.

use futures_util::StreamExt;
use parley_llm::{
    ChatRequest, Delta, FinishReason, LlmError, LlmProvider, Message, anthropic::Anthropic,
    openai::OpenAiCompatible,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serves `status` and `body` to every connection; returns the base URL.
async fn serve(status: &'static str, content_type: &'static str, body: String) -> String {
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
                    "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
                sock.shutdown().await.ok();
            });
        }
    });
    url
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn collect<P: LlmProvider>(p: &P) -> Vec<Result<Delta, LlmError>> {
    let req = ChatRequest {
        messages: vec![Message::user("hi")],
        ..Default::default()
    };
    p.stream(req).await.unwrap().collect().await
}

async fn openai(file: &str) -> OpenAiCompatible {
    let url = serve("200 OK", "text/event-stream", fixture(file)).await;
    OpenAiCompatible::new(url, None, "m")
}

async fn anthropic(file: &str) -> Anthropic {
    let url = serve("200 OK", "text/event-stream", fixture(file)).await;
    Anthropic::new(url, "k", "m")
}

fn ok(v: Vec<Result<Delta, LlmError>>) -> Vec<Delta> {
    v.into_iter().map(Result::unwrap).collect()
}

fn text_expected(end: FinishReason) -> Vec<Delta> {
    vec![
        Delta::Text("Hel".into()),
        Delta::Text("lo".into()),
        Delta::End(end),
    ]
}

fn tool_expected(end: FinishReason) -> Vec<Delta> {
    vec![
        Delta::ToolCall {
            id: "X".into(),
            name: "doc_append".into(),
            arguments: json!({"text": "hi"}),
        },
        Delta::End(end),
    ]
}

fn with_id(mut v: Vec<Delta>, id: &str) -> Vec<Delta> {
    if let Delta::ToolCall { id: i, .. } = &mut v[0] {
        *i = id.into();
    }
    v
}

#[tokio::test]
async fn openai_text() {
    assert_eq!(
        ok(collect(&openai("openai_text.sse").await).await),
        text_expected(FinishReason::Stop)
    );
}

#[tokio::test]
async fn openai_tool_call() {
    let got = ok(collect(&openai("openai_tool.sse").await).await);
    assert_eq!(
        got,
        with_id(tool_expected(FinishReason::ToolCalls), "call_1")
    );
}

#[tokio::test]
async fn openai_http_error() {
    let url = serve(
        "429 Too Many Requests",
        "application/json",
        r#"{"error":"slow down"}"#.into(),
    )
    .await;
    let r = OpenAiCompatible::new(url, None, "m")
        .stream(ChatRequest::default())
        .await;
    assert!(
        matches!(r, Err(LlmError::Api { status: 429, ref body }) if body.contains("slow down"))
    );
}

#[tokio::test]
async fn openai_cut_stream() {
    let got = collect(&openai("openai_cut.sse").await).await;
    assert_eq!(got.len(), 3);
    assert!(matches!(got[0], Ok(Delta::Text(_))));
    assert!(matches!(got[2], Err(LlmError::StreamCut)));
}

#[tokio::test]
async fn anthropic_text() {
    assert_eq!(
        ok(collect(&anthropic("anthropic_text.sse").await).await),
        text_expected(FinishReason::Stop)
    );
}

#[tokio::test]
async fn anthropic_tool_call() {
    let got = ok(collect(&anthropic("anthropic_tool.sse").await).await);
    assert_eq!(
        got,
        with_id(tool_expected(FinishReason::ToolCalls), "toolu_1")
    );
}

#[tokio::test]
async fn anthropic_http_error() {
    let url = serve(
        "401 Unauthorized",
        "application/json",
        r#"{"type":"error"}"#.into(),
    )
    .await;
    let r = Anthropic::new(url, "k", "m")
        .stream(ChatRequest::default())
        .await;
    assert!(matches!(r, Err(LlmError::Api { status: 401, .. })));
}

#[tokio::test]
async fn anthropic_cut_stream() {
    let got = collect(&anthropic("anthropic_cut.sse").await).await;
    assert_eq!(got.len(), 2);
    assert!(matches!(got[1], Err(LlmError::StreamCut)));
}
