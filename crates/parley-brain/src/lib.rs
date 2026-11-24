//! Meeting brain (plan 5.1 row 3, 5.2, F12): a rolling transcript and notes from any
//! [`parley_llm::LlmProvider`]. The model answers through one tool call.

use futures_util::StreamExt;
use parley_llm::{ChatRequest, Delta, LlmError, LlmProvider, Message, ToolDef};
use serde::Deserialize;
use serde_json::json;
use std::fmt::Write;

const TOOL_NAME: &str = "write_notes";
const SYSTEM: &str = "You take notes for a meeting. Read the transcript, then call the write_notes tool once. Give a short summary. Add one action item for each task that a person agreed to do or was asked to do, with that person as the owner.";

const ANSWER_SYSTEM: &str = "You answer a question that a person asked in a meeting. Reply with a short answer for a chat message. Use only the transcript. If the transcript does not contain the answer, say so.";

/// The most characters `answer()` returns.
pub const MAX_ANSWER_CHARS: usize = 500;

/// The bot name when none is configured.
pub const DEFAULT_NAME: &str = "Parley";

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

    /// A plain-text answer, at most [`MAX_ANSWER_CHARS`] characters, for a chat message.
    pub async fn answer<P: LlmProvider>(
        &self,
        llm: &P,
        question: &str,
    ) -> Result<String, BrainError> {
        let req = ChatRequest {
            system: Some(ANSWER_SYSTEM.into()),
            messages: vec![Message::user(format!(
                "Transcript:\n{}\nQuestion: {question}",
                self.render()
            ))],
            ..Default::default()
        };
        let mut s = llm.stream(req).await?;
        let mut text = String::new();
        while let Some(d) = s.next().await {
            if let Delta::Text(t) = d? {
                text.push_str(&t);
            }
        }
        let text = text.trim();
        let end = text
            .char_indices()
            .nth(MAX_ANSWER_CHARS)
            .map_or(text.len(), |(i, _)| i);
        Ok(text[..end].into())
    }
}

/// Drop `prefix` from the start of `s`, ignoring case. Returns the rest.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let mut it = s.char_indices();
    for p in prefix.chars() {
        let (_, c) = it.next()?;
        if !c.to_lowercase().eq(p.to_lowercase()) {
            return None;
        }
    }
    Some(it.next().map_or("", |(i, _)| &s[i..]))
}

/// The question in `text` when it starts with `name` (optionally after "hey"), else `None`.
/// Matches any case, with or without a comma. "Parleyed" or a bare "Parley" is not a request.
pub fn find_request<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = text.trim_start();
    if let Some(r) = strip_prefix_ci(rest, "hey")
        && r.starts_with(|c: char| c.is_whitespace() || c == ',')
    {
        rest = r.trim_start_matches(|c: char| c.is_whitespace() || c == ',');
    }
    let rest = strip_prefix_ci(rest, name)?;
    if rest.starts_with(|c: char| c.is_alphanumeric()) {
        return None;
    }
    let q = rest
        .trim_start_matches(|c: char| c.is_whitespace() || ",:;-\u{2013}\u{2014}.".contains(c))
        .trim_end();
    (!q.is_empty()).then_some(q)
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

#[cfg(test)]
mod tests {
    use super::find_request;

    #[test]
    fn finds_requests() {
        let f = |t| find_request(t, "Parley");
        assert_eq!(
            f("Parley, what did Tom agree to?"),
            Some("what did Tom agree to?")
        );
        assert_eq!(f("hey parley what is next"), Some("what is next"));
        assert_eq!(f("  PARLEY: summarize"), Some("summarize"));
        assert_eq!(f("Hey, Parley, who owns this?"), Some("who owns this?"));
    }

    #[test]
    fn ignores_non_requests() {
        let f = |t| find_request(t, "Parley");
        assert_eq!(f("I like Parley a lot"), None);
        assert_eq!(f("Parley"), None);
        assert_eq!(f("Parley, "), None);
        assert_eq!(f("Parleyed along, what now"), None);
        assert_eq!(f("Hey team"), None);
    }

    #[test]
    fn name_is_a_parameter() {
        assert_eq!(find_request("Max, status?", "Max"), Some("status?"));
        assert_eq!(find_request("Max, status?", "Parley"), None);
    }
}
