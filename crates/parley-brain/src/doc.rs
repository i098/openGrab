//! The tools that write the live doc: `doc_create` and `doc_append` (plan 5.3 steps 5 and 6).

use crate::{BrainError, MAX_ANSWER_CHARS, Transcript, take_chars};
use futures_util::StreamExt;
use parley_artifacts::{Artifact, Span};
use parley_llm::{ChatRequest, Delta, LlmProvider, Message, ToolDef};
use serde::Deserialize;
use serde_json::{Value, json};

pub const DOC_CREATE: &str = "doc_create";
pub const DOC_APPEND: &str = "doc_append";

const SYSTEM: &str = "You write a shared document for a meeting. Read the transcript and the request. Call doc_create once with the title, then call doc_append once for each block, in reading order: headings, paragraphs and bullet lists. Then reply with one short chat line, and no link.";

const DONE_LINE: &str = "Done.";

#[derive(Deserialize)]
struct Create {
    title: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Block {
    Paragraph { text: String },
    Heading { level: u8, text: String },
    Bullets { items: Vec<String> },
}

fn bad(msg: impl std::fmt::Display) -> BrainError {
    BrainError::BadDocCall(msg.to_string())
}

fn text(s: &str) -> Result<Vec<Span>, BrainError> {
    match s.trim() {
        "" => Err(bad("empty text")),
        t => Ok(vec![Span::plain(t)]),
    }
}

/// Check every call before any is applied: a rejected call leaves the document unchanged.
fn parse(calls: Vec<(String, Value)>) -> Result<Vec<Edit>, BrainError> {
    if calls.is_empty() {
        return Err(BrainError::NoToolCall);
    }
    let mut edits = Vec::with_capacity(calls.len());
    for (name, args) in calls {
        let edit = match name.as_str() {
            DOC_CREATE if edits.is_empty() => {
                let c: Create = serde_json::from_value(args).map_err(bad)?;
                Edit::Heading(1, text(&c.title)?)
            }
            DOC_CREATE => return Err(bad("doc_create must be the first call, and only once")),
            DOC_APPEND if edits.is_empty() => return Err(bad("doc_append before doc_create")),
            DOC_APPEND => match serde_json::from_value(args).map_err(bad)? {
                Block::Paragraph { text: t } => Edit::Paragraph(text(&t)?),
                Block::Heading { level, text: t } if (1..=6).contains(&level) => {
                    Edit::Heading(level, text(&t)?)
                }
                Block::Heading { level, .. } => {
                    return Err(bad(format!("heading level {level} is not 1 to 6")));
                }
                Block::Bullets { items } if !items.is_empty() => {
                    Edit::Bullets(items.iter().map(|i| text(i)).collect::<Result<_, _>>()?)
                }
                Block::Bullets { .. } => return Err(bad("bullets needs at least one item")),
            },
            other => return Err(bad(format!("unknown tool {other}"))),
        };
        edits.push(edit);
    }
    Ok(edits)
}

enum Edit {
    Heading(u8, Vec<Span>),
    Paragraph(Vec<Span>),
    Bullets(Vec<Vec<Span>>),
}

impl Transcript {
    /// Run the model with `doc_create` and `doc_append` on a chat request such as
    /// "start a doc for the launch plan", and apply its calls in order to `doc`.
    /// Returns the chat line to post, at most [`MAX_ANSWER_CHARS`] characters.
    /// A malformed call, or no call, is an error and `doc` is unchanged.
    pub async fn write_doc<P: LlmProvider>(
        &self,
        llm: &P,
        request: &str,
        doc: &Artifact,
    ) -> Result<String, BrainError> {
        let req = ChatRequest {
            system: Some(SYSTEM.into()),
            messages: vec![Message::user(format!(
                "Transcript:\n{}\nRequest: {request}",
                self.render()
            ))],
            tools: vec![create_tool(), append_tool()],
            ..Default::default()
        };
        let mut s = llm.stream(req).await?;
        let (mut calls, mut chat) = (Vec::new(), String::new());
        while let Some(d) = s.next().await {
            match d? {
                Delta::ToolCall {
                    name, arguments, ..
                } => calls.push((name, arguments)),
                Delta::Text(t) => chat.push_str(&t),
                Delta::End(_) => {}
            }
        }
        for edit in parse(calls)? {
            match edit {
                Edit::Heading(level, spans) => doc.insert_heading(u32::MAX, level, &spans),
                Edit::Paragraph(spans) => doc.append_paragraph(&spans),
                Edit::Bullets(items) => doc.append_bullet_list(&items),
            }
        }
        let chat = chat.trim();
        Ok(take_chars(
            if chat.is_empty() { DONE_LINE } else { chat },
            MAX_ANSWER_CHARS,
        )
        .into())
    }
}

fn create_tool() -> ToolDef {
    ToolDef {
        name: DOC_CREATE.into(),
        description: "Start the document with its title. Call it once, first.".into(),
        parameters: json!({
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"]
        }),
    }
}

fn append_tool() -> ToolDef {
    ToolDef {
        name: DOC_APPEND.into(),
        description: "Add one block at the end of the document.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "type": {"type": "string", "enum": ["paragraph", "heading", "bullets"]},
                "text": {"type": "string", "description": "For paragraph and heading."},
                "level": {"type": "integer", "minimum": 1, "maximum": 6, "description": "For heading."},
                "items": {"type": "array", "items": {"type": "string"}, "description": "For bullets."}
            },
            "required": ["type"]
        }),
    }
}
