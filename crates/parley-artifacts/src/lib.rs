//! Meeting artifacts stored in `yrs`: Tiptap documents and [Excalidraw boards](board).
//!
//! A Tiptap [`Artifact`] uses the `y-prosemirror` layout with one root `XmlFragment` named [`FRAGMENT`].
//! Block nodes use `XmlElement` names that match Tiptap node types: `paragraph`, `heading`, `bulletList`, and `listItem`.
//! `XmlElement` attributes store node attributes, such as the numeric heading `level`.
//! Each `XmlText` formatting attribute stores a mark, such as `bold` or `italic`, as an object.

use std::{collections::HashMap, sync::Arc};

use serde_json::{Value, json};
use yrs::{
    Any, Doc, Number, Out, ReadTxn, StateVector, Text, Transact, Update, Xml, XmlElementPrelim,
    XmlElementRef, XmlFragment, XmlFragmentRef, XmlOut, XmlTextPrelim, types::Attrs,
    updates::decoder::Decode,
};

/// Name of the root fragment, the default of `@tiptap/extension-collaboration`.
pub const FRAGMENT: &str = "default";

pub mod board;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid update: {0}")]
    Decode(#[from] yrs::encoding::read::Error),
    #[error("cannot apply update: {0}")]
    Apply(#[from] yrs::error::UpdateError),
    #[error("unsupported node in the document: {0}")]
    Unsupported(String),
    #[error("invalid board: {0}")]
    Invalid(String),
}

/// A run of text with marks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
}

impl Span {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    pub fn bold(text: impl Into<String>) -> Self {
        Self {
            bold: true,
            ..Self::plain(text)
        }
    }

    pub fn italic(text: impl Into<String>) -> Self {
        Self {
            italic: true,
            ..Self::plain(text)
        }
    }
}

pub struct Artifact {
    doc: Doc,
    fragment: XmlFragmentRef,
}

impl Default for Artifact {
    fn default() -> Self {
        Self::new()
    }
}

impl Artifact {
    pub fn new() -> Self {
        let doc = Doc::new();
        let fragment = doc.get_or_insert_xml_fragment(FRAGMENT);
        Self { doc, fragment }
    }

    /// Build a document from a full update, as produced by [`Artifact::encode_update`] or by Yjs.
    pub fn from_update(update: &[u8]) -> Result<Self, Error> {
        let artifact = Self::new();
        artifact.apply_update(update)?;
        Ok(artifact)
    }

    pub fn apply_update(&self, update: &[u8]) -> Result<(), Error> {
        let update = Update::decode_v1(update)?;
        self.doc.transact_mut().apply_update(update)?;
        Ok(())
    }

    /// The full document state as a Yjs v1 update.
    pub fn encode_update(&self) -> Vec<u8> {
        self.doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default())
    }

    pub fn append_paragraph(&self, spans: &[Span]) {
        let len = self.fragment.len(&self.doc.transact());
        self.insert_block(len, "paragraph", None, spans);
    }

    /// Insert a heading before the block at `index` (`index` past the end appends).
    pub fn insert_heading(&self, index: u32, level: u8, spans: &[Span]) {
        let len = self.fragment.len(&self.doc.transact());
        self.insert_block(index.min(len), "heading", Some(level), spans);
    }

    pub fn append_bullet_list(&self, items: &[Vec<Span>]) {
        let mut txn = self.doc.transact_mut();
        let len = self.fragment.len(&txn);
        let list = self
            .fragment
            .insert(&mut txn, len, XmlElementPrelim::empty("bulletList"));
        for (i, spans) in items.iter().enumerate() {
            let item = list.insert(&mut txn, i as u32, XmlElementPrelim::empty("listItem"));
            let para = item.insert(&mut txn, 0, XmlElementPrelim::empty("paragraph"));
            write_spans(&para, &mut txn, spans);
        }
    }

    fn insert_block(&self, index: u32, tag: &str, level: Option<u8>, spans: &[Span]) {
        let mut txn = self.doc.transact_mut();
        let block = self
            .fragment
            .insert(&mut txn, index, XmlElementPrelim::empty(tag));
        if let Some(level) = level {
            block.insert_attribute(
                &mut txn,
                "level",
                Any::Number(Number::Int(i64::from(level))),
            );
        }
        write_spans(&block, &mut txn, spans);
    }

    /// Read the document back as Tiptap JSON (`editor.getJSON()` shape).
    pub fn to_json(&self) -> Result<Value, Error> {
        let txn = self.doc.transact();
        let content = nodes_json(self.fragment.children(&txn), &txn)?;
        Ok(json!({ "type": "doc", "content": content }))
    }
}

fn write_spans(block: &XmlElementRef, txn: &mut yrs::TransactionMut, spans: &[Span]) {
    let spans: Vec<&Span> = spans.iter().filter(|s| !s.text.is_empty()).collect();
    if spans.is_empty() {
        return;
    }
    let text = block.insert(txn, 0, XmlTextPrelim::new(""));
    let mut at = 0;
    for span in spans {
        let mut attrs = Attrs::new();
        if span.bold {
            attrs.insert("bold".into(), Any::Map(Arc::new(HashMap::new())));
        }
        if span.italic {
            attrs.insert("italic".into(), Any::Map(Arc::new(HashMap::new())));
        }
        text.insert_with_attributes(txn, at, &span.text, attrs);
        at += span.text.encode_utf16().count() as u32;
    }
}

fn nodes_json<T: ReadTxn>(
    nodes: impl Iterator<Item = XmlOut>,
    txn: &T,
) -> Result<Vec<Value>, Error> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            XmlOut::Element(el) => {
                let mut obj = json!({ "type": el.tag().as_ref() });
                let mut attrs = serde_json::Map::new();
                for (key, value) in el.attributes(txn) {
                    attrs.insert(key.to_string(), out_json(value)?);
                }
                if !attrs.is_empty() {
                    obj["attrs"] = Value::Object(attrs);
                }
                let content = nodes_json(el.children(txn), txn)?;
                if !content.is_empty() {
                    obj["content"] = Value::Array(content);
                }
                out.push(obj);
            }
            XmlOut::Text(text) => {
                for diff in text.diff::<T, (), _>(txn, |_| ()) {
                    let Out::Any(Any::String(s)) = &diff.insert else {
                        return Err(Error::Unsupported("embedded value in text".into()));
                    };
                    let mut node = json!({ "type": "text", "text": s.as_ref() });
                    if let Some(attrs) = &diff.attributes {
                        let mut names: Vec<_> = attrs.iter().collect();
                        names.sort_by_key(|(name, _)| name.to_string());
                        let marks: Vec<Value> = names
                            .into_iter()
                            .map(|(name, _)| json!({ "type": name.as_ref() }))
                            .collect();
                        if !marks.is_empty() {
                            node["marks"] = Value::Array(marks);
                        }
                    }
                    out.push(node);
                }
            }
            XmlOut::Fragment(_) => return Err(Error::Unsupported("nested fragment".into())),
        }
    }
    Ok(out)
}

fn out_json(value: Out) -> Result<Value, Error> {
    match value {
        Out::Any(Any::Number(Number::Int(n))) => Ok(json!(n)),
        Out::Any(Any::Number(Number::Float(n))) => Ok(json!(n)),
        Out::Any(Any::String(s)) => Ok(json!(s.as_ref())),
        Out::Any(Any::Bool(b)) => Ok(json!(b)),
        Out::Any(Any::Null) => Ok(Value::Null),
        other => Err(Error::Unsupported(format!("attribute value {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Artifact {
        let a = Artifact::new();
        a.append_paragraph(&[
            Span::plain("Hello "),
            Span::bold("bold"),
            Span::italic(" it"),
        ]);
        a.append_bullet_list(&[vec![Span::plain("one")], vec![Span::plain("two")]]);
        a
    }

    #[test]
    fn appends_paragraph_with_marks() {
        let json = sample().to_json().unwrap();
        assert_eq!(
            json["content"][0],
            json!({"type":"paragraph","content":[
                {"type":"text","text":"Hello "},
                {"type":"text","text":"bold","marks":[{"type":"bold"}]},
                {"type":"text","text":" it","marks":[{"type":"italic"}]},
            ]})
        );
    }

    #[test]
    fn inserts_heading_before_existing_block() {
        let a = sample();
        a.insert_heading(0, 2, &[Span::plain("Title")]);
        let json = a.to_json().unwrap();
        assert_eq!(
            json["content"][0],
            json!({"type":"heading","attrs":{"level":2},"content":[{"type":"text","text":"Title"}]})
        );
        assert_eq!(json["content"][1]["type"], "paragraph");
        assert_eq!(
            json["content"][2]["content"][1]["content"][0]["content"][0]["text"],
            "two"
        );
    }

    #[test]
    fn update_round_trips_between_docs() {
        let a = sample();
        let b = Artifact::from_update(&a.encode_update()).unwrap();
        assert_eq!(a.to_json().unwrap(), b.to_json().unwrap());
    }

    #[test]
    fn fragment_is_named_default() {
        let a = sample();
        let txn = a.doc.transact();
        assert!(yrs::GetString::get_string(&a.fragment, &txn).contains("Hello"));
        assert_eq!(FRAGMENT, "default");
    }
}
