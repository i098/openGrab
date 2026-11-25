//! Meeting brain (plan 5.1 row 3, 5.2, F12): a rolling transcript and notes from any
//! [`parley_llm::LlmProvider`]. The model answers through one tool call.

use futures_util::StreamExt;
use parley_llm::{ChatRequest, Delta, LlmError, LlmProvider, Message, ToolDef};
use serde::Deserialize;
use serde_json::json;
use std::fmt::Write;

mod doc;
pub use doc::{DOC_APPEND, DOC_CREATE};

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
    #[error("rejected doc tool call: {0}")]
    BadDocCall(String),
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

    /// The chat text for a recap request: summary and action items from [`Self::notes`],
    /// at most [`MAX_ANSWER_CHARS`] characters. See [`format_recap`].
    pub async fn recap<P: LlmProvider>(&self, llm: &P) -> Result<String, BrainError> {
        Ok(format_recap(&self.notes(llm).await?))
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

/// True when the request asks for a recap or a summary. A keyword match, no model call.
pub fn is_recap_request(request: &str) -> bool {
    let r = request.to_lowercase();
    r.contains("recap") || r.contains("summar")
}

fn take_chars(s: &str, n: usize) -> &str {
    &s[..s.char_indices().nth(n).map_or(s.len(), |(i, _)| i)]
}

/// Chat text for the notes, at most [`MAX_ANSWER_CHARS`] characters. The summary is cut
/// first; every action item line stays unless the items alone are too long.
pub fn format_recap(notes: &Notes) -> String {
    let mut items = String::new();
    for a in &notes.action_items {
        items.push_str("\n- ");
        items.push_str(a.text.trim());
        if let Some(o) = a.owner.as_deref().map(str::trim).filter(|o| !o.is_empty()) {
            let _ = write!(items, " ({o})");
        }
    }
    let items = if items.is_empty() {
        "\nNo action items.".to_string()
    } else {
        format!("\nAction items:{items}")
    };
    let summary = notes.summary.trim();
    let budget = MAX_ANSWER_CHARS.saturating_sub("Recap: ".len() + items.chars().count());
    let head = if summary.chars().count() <= budget {
        format!("Recap: {summary}")
    } else if budget > 1 {
        format!("Recap: {}\u{2026}", take_chars(summary, budget - 1))
    } else {
        "Recap:".to_string()
    };
    take_chars(&format!("{head}{items}"), MAX_ANSWER_CHARS).into()
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
    use super::{ActionItem, Notes, find_request, format_recap, is_recap_request};

    fn item(text: &str, owner: Option<&str>) -> ActionItem {
        ActionItem {
            text: text.into(),
            owner: owner.map(Into::into),
        }
    }

    #[test]
    fn classifies_recap_requests() {
        for q in [
            "recap",
            "give us a recap",
            "Summarize the meeting so far",
            "a SUMMARY please",
        ] {
            assert!(is_recap_request(q), "{q}");
        }
        for q in ["what did Tom agree to?", "who owns this?"] {
            assert!(!is_recap_request(q), "{q}");
        }
    }

    #[test]
    fn short_recap_has_summary_and_owners() {
        let n = Notes {
            summary: "We shipped.".into(),
            action_items: vec![item("Send deck", Some("Tom")), item("Book room", None)],
        };
        assert_eq!(
            format_recap(&n),
            "Recap: We shipped.\nAction items:\n- Send deck (Tom)\n- Book room"
        );
    }

    #[test]
    fn long_summary_is_cut_and_items_kept() {
        let n = Notes {
            summary: "é".repeat(2000),
            action_items: vec![
                item("Send deck", Some("Tom")),
                item("Book room", Some("Ann")),
            ],
        };
        let r = format_recap(&n);
        assert_eq!(r.chars().count(), 500);
        assert!(r.ends_with("Action items:\n- Send deck (Tom)\n- Book room (Ann)"));
        assert!(r.contains('\u{2026}'));
    }

    #[test]
    fn huge_items_still_fit_one_message() {
        let n = Notes {
            summary: "s".into(),
            action_items: vec![item(&"x".repeat(900), None)],
        };
        assert_eq!(format_recap(&n).chars().count(), 500);
    }

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
