//! `write_board()` with a scripted provider: a valid call lays out a board, a bad call writes nothing.

use futures_util::stream;
use parley_artifacts::board::BoardDoc;
use parley_brain::{BrainError, Transcript};
use parley_llm::{ChatRequest, Delta, DeltaStream, FinishReason, LlmError, LlmProvider};
use serde_json::{Value, json};

struct Script(Vec<Delta>);

impl LlmProvider for Script {
    async fn stream(&self, _: ChatRequest) -> Result<DeltaStream, LlmError> {
        let mut d = self.0.clone();
        d.push(Delta::End(FinishReason::Stop));
        Ok(Box::pin(stream::iter(d.into_iter().map(Ok))))
    }
}

fn call(name: &str, arguments: Value) -> Delta {
    Delta::ToolCall {
        id: "c".into(),
        name: name.into(),
        arguments,
    }
}

fn flow() -> Value {
    json!({
        "nodes": [
            {"id": "a", "label": "Sign up"},
            {"id": "b", "label": "Verify email"},
            {"id": "c", "label": "Welcome"}
        ],
        "edges": [{"from": "a", "to": "b"}, {"from": "b", "to": "c", "label": "link"}],
        "groups": [{"id": "g", "label": "Onboarding", "members": ["b", "c"]}]
    })
}

async fn run(script: Vec<Delta>) -> (Result<String, BrainError>, BoardDoc) {
    let board = BoardDoc::new();
    let r = Transcript::new()
        .write_board(&Script(script), "Parley, draw the signup flow", &board)
        .await;
    (r, board)
}

#[tokio::test]
async fn a_valid_call_lays_out_the_board() {
    let (r, board) = run(vec![
        Delta::Text("Drew it.".into()),
        call("board_create", flow()),
    ])
    .await;
    assert_eq!(r.unwrap(), "Drew it.");
    let els = board.elements().unwrap();
    let count = |t: &str| els.iter().filter(|e| e["type"] == t).count();
    assert_eq!(count("rectangle"), 3);
    assert_eq!(count("arrow"), 2);
}

#[tokio::test]
async fn each_rejected_call_changes_nothing() {
    let many: Vec<_> = (0..31)
        .map(|i| json!({"id": format!("n{i}"), "label": "x"}))
        .collect();
    let mut dup = flow();
    dup["nodes"][1]["id"] = json!("a");
    let mut blank = flow();
    blank["nodes"][0]["label"] = json!(" ");
    let mut unknown = flow();
    unknown["edges"][0]["to"] = json!("zzz");
    let mut long = flow();
    long["edges"][0]["label"] = json!("x".repeat(17));
    let mut coords = flow();
    coords["nodes"][0] = json!({"id": "a"});
    for script in [
        vec![call("board_create", dup)],
        vec![call("board_create", blank)],
        vec![call("board_create", unknown)],
        vec![call("board_create", long)],
        vec![call("board_create", coords)],
        vec![call("board_create", json!({"nodes": many}))],
        vec![call("board_create", json!({"nodes": []}))],
        vec![call("nope", flow())],
        vec![call("board_create", flow()), call("board_create", flow())],
    ] {
        let (r, board) = run(script).await;
        assert!(matches!(r, Err(BrainError::BadBoardCall(_))));
        assert!(board.elements().unwrap().is_empty());
    }
}

#[tokio::test]
async fn no_call_is_an_error() {
    let (r, board) = run(vec![Delta::Text("hi".into())]).await;
    assert!(matches!(r, Err(BrainError::NoToolCall)));
    assert!(board.elements().unwrap().is_empty());
}

#[tokio::test]
async fn a_board_with_content_is_not_edited() {
    let board = BoardDoc::new();
    let t = Transcript::new();
    t.write_board(&Script(vec![call("board_create", flow())]), "draw", &board)
        .await
        .unwrap();
    let r = t
        .write_board(&Script(vec![call("board_create", flow())]), "again", &board)
        .await;
    assert!(matches!(r, Err(BrainError::BadBoardCall(_))));
}
