//! The tool that makes a board: `board_create`. The model names nodes, edges and groups; code places them.

use crate::{BrainError, MAX_ANSWER_CHARS, Transcript, take_chars};
use futures_util::StreamExt;
use parley_artifacts::board::{Board, BoardDoc};
use parley_llm::{ChatRequest, Delta, LlmProvider, Message, ToolDef};
use serde_json::{Value, json};

pub const BOARD_CREATE: &str = "board_create";

/// The most nodes one call may name. More would not stay readable on one screen.
pub const MAX_NODES: usize = 30;

const SYSTEM: &str = "You draw a diagram on a shared whiteboard for a meeting. Read the transcript and the request. Call board_create once. Name every node with a short id and a short label, and every edge with the ids it joins. Add a group only when the request needs one. Never give coordinates. Then reply with one short chat line, and no link.";

const DONE_LINE: &str = "Done.";

fn bad(msg: impl std::fmt::Display) -> BrainError {
    BrainError::BadBoardCall(msg.to_string())
}

/// Check the whole call, including the layout, before anything is written.
fn parse(calls: Vec<(String, Value)>) -> Result<Vec<Value>, BrainError> {
    if calls.is_empty() {
        return Err(BrainError::NoToolCall);
    }
    let [(name, args)] =
        <[_; 1]>::try_from(calls).map_err(|_| bad("call board_create exactly once"))?;
    if name != BOARD_CREATE {
        return Err(bad(format!("unknown tool {name}")));
    }
    let board: Board = serde_json::from_value(args).map_err(bad)?;
    if board.nodes.is_empty() {
        return Err(bad("a board needs at least one node"));
    }
    if board.nodes.len() > MAX_NODES {
        return Err(bad(format!("more than {MAX_NODES} nodes")));
    }
    board.elements().map_err(bad)
}

impl Transcript {
    /// Run the model with `board_create` on a request such as "draw the signup flow"
    /// and write the laid-out board to `board`.
    /// Returns the chat line to post, at most [`MAX_ANSWER_CHARS`] characters.
    /// A malformed call, a second call, or no call is an error and `board` is unchanged.
    pub async fn write_board<P: LlmProvider>(
        &self,
        llm: &P,
        request: &str,
        board: &BoardDoc,
    ) -> Result<String, BrainError> {
        if !board.elements().map_err(bad)?.is_empty() {
            return Err(bad(
                "the board already has content; editing is not supported",
            ));
        }
        let req = ChatRequest {
            system: Some(SYSTEM.into()),
            messages: vec![Message::user(format!(
                "Transcript:\n{}\nRequest: {request}",
                self.render()
            ))],
            tools: vec![create_tool()],
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
        board.set_elements(&parse(calls)?);
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
        name: BOARD_CREATE.into(),
        description: "Draw a board. Name the boxes (nodes), the arrows between them (edges) and optional groups. Positions are computed for you.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "nodes": {"type": "array", "description": "The boxes.", "items": {
                    "type": "object",
                    "properties": {"id": {"type": "string"}, "label": {"type": "string"}},
                    "required": ["id", "label"]}},
                "edges": {"type": "array", "description": "Arrows from one node id to another.", "items": {
                    "type": "object",
                    "properties": {"from": {"type": "string"}, "to": {"type": "string"},
                        "label": {"type": "string", "description": "At most 16 characters."}},
                    "required": ["from", "to"]}},
                "groups": {"type": "array", "description": "Labelled sets of node ids.", "items": {
                    "type": "object",
                    "properties": {"id": {"type": "string"}, "label": {"type": "string"},
                        "members": {"type": "array", "items": {"type": "string"}}},
                    "required": ["id", "label", "members"]}}
            },
            "required": ["nodes"]
        }),
    }
}
