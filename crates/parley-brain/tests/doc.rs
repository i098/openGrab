//! `write_doc()` with a scripted provider: calls apply in order, a bad call applies nothing.

use futures_util::stream;
use parley_artifacts::Artifact;
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

async fn run(script: Vec<Delta>) -> (Result<String, BrainError>, Value) {
    let doc = Artifact::new();
    let r = Transcript::new()
        .write_doc(
            &Script(script),
            "Parley, start a doc for the launch plan",
            &doc,
        )
        .await;
    (r, doc.to_json().unwrap())
}

#[tokio::test]
async fn create_and_three_appends_build_the_document() {
    let (chat, doc) = run(vec![
        Delta::Text("Started the launch plan.".into()),
        call("doc_create", json!({"title": "Launch plan"})),
        call(
            "doc_append",
            json!({"type": "heading", "level": 2, "text": "Goals"}),
        ),
        call(
            "doc_append",
            json!({"type": "paragraph", "text": "Ship on Friday."}),
        ),
        call(
            "doc_append",
            json!({"type": "bullets", "items": ["Notes", "Demo room"]}),
        ),
    ])
    .await;
    assert_eq!(chat.unwrap(), "Started the launch plan.");
    let kinds: Vec<_> = doc["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["heading", "heading", "paragraph", "bulletList"]);
    let s = doc.to_string();
    for t in ["Launch plan", "Goals", "Ship on Friday.", "Demo room"] {
        assert!(s.contains(t), "{t}");
    }
}

#[tokio::test]
async fn a_malformed_call_changes_nothing() {
    for bad in [
        call(
            "doc_append",
            json!({"type": "heading", "level": 9, "text": "x"}),
        ),
        call("doc_append", json!({"type": "poem", "text": "x"})),
        call("doc_append", json!({"type": "bullets", "items": []})),
        call("doc_create", json!({"title": "again"})),
        call("nope", json!({})),
    ] {
        let (r, doc) = run(vec![
            call("doc_create", json!({"title": "T"})),
            call("doc_append", json!({"type": "paragraph", "text": "ok"})),
            bad,
        ])
        .await;
        assert!(matches!(r, Err(BrainError::BadDocCall(_))));
        assert_eq!(doc["content"].as_array().unwrap().len(), 0);
    }
}

#[tokio::test]
async fn no_call_is_an_error() {
    let (r, doc) = run(vec![Delta::Text("hi".into())]).await;
    assert!(matches!(r, Err(BrainError::NoToolCall)));
    assert_eq!(doc["content"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn append_before_create_is_rejected() {
    let (r, doc) = run(vec![call(
        "doc_append",
        json!({"type": "paragraph", "text": "x"}),
    )])
    .await;
    assert!(matches!(r, Err(BrainError::BadDocCall(_))));
    assert_eq!(doc["content"].as_array().unwrap().len(), 0);
}
