//! Meeting brain (plan 5.1 row 3, 5.2, F12): a rolling transcript and notes from any
//! [`parley_llm::LlmProvider`]. The model answers through one tool call.

use futures_util::StreamExt;
use parley_llm::{ChatRequest, Delta, LlmError, LlmProvider, Message, ToolDef};
use serde::Deserialize;
use serde_json::json;
use std::fmt::Write;

const TOOL_NAME: &str = "write_notes";
const SYSTEM: &str = "You take notes for a meeting. Read the transcript, then call the write_notes tool once. Give a short summary. Add one action item for each task that a person agreed to do or was asked to do, with that person as the owner.";

/// One utterance. This crate's own type: it does not depend on the STT crate.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub speaker: String,
    pub start_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ActionItem {
    pub text: String,
    #[serde(default)]
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Notes {
    pub summary: String,
    pub action_items: Vec<ActionItem>,
}

#[derive(Debug, thiserror::Error)]
pub enum BrainError {
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error("the model did not call the {TOOL_NAME} tool")]
    NoToolCall,
    #[error("the {TOOL_NAME} arguments do not match the notes shape: {0}")]
    BadNotes(#[from] serde_json::Error),
}

/// Ordered utterances, kept in start-time order.
#[derive(Debug, Default)]
pub struct Transcript {
    lines: Vec<Line>,
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert after every line that starts at or before this one.
    pub fn push(&mut self, line: Line) {
        let at = self.lines.partition_point(|l| l.start_ms <= line.start_ms);
        self.lines.insert(at, line);
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    fn render(&self) -> String {
        let mut s = String::new();
        for l in &self.lines {
            let sec = l.start_ms / 1000;
            let _ = writeln!(
                s,
                "[{:02}:{:02}] {}: {}",
                sec / 60,
                sec % 60,
                l.speaker,
                l.text
            );
        }
        s
    }

    /// Summary and action items for the transcript so far.
    pub async fn notes<P: LlmProvider>(&self, llm: &P) -> Result<Notes, BrainError> {
        let req = ChatRequest {
            system: Some(SYSTEM.into()),
            messages: vec![Message::user(self.render())],
            tools: vec![notes_tool()],
            required_tool: Some(TOOL_NAME.into()),
            ..Default::default()
        };
        let mut s = llm.stream(req).await?;
        let mut found = None;
        while let Some(d) = s.next().await {
            if let Delta::ToolCall {
                name, arguments, ..
            } = d?
                && name == TOOL_NAME
                && found.is_none()
            {
                found = Some(arguments);
            }
        }
        Ok(serde_json::from_value(
            found.ok_or(BrainError::NoToolCall)?,
        )?)
    }
}

fn notes_tool() -> ToolDef {
    ToolDef {
        name: TOOL_NAME.into(),
        description: "Record the meeting notes.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string", "description": "Short summary of the meeting so far."},
                "action_items": {
                    "type": "array",
                    "description": "One entry for each task a person agreed to do. Empty only when nobody took a task.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "text": {"type": "string"},
                            "owner": {"type": "string", "description": "Who does it; omit when unknown."}
                        },
                        "required": ["text"]
                    }
                }
            },
            "required": ["summary", "action_items"]
        }),
    }
}
