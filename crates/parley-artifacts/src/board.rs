//! Board model, deterministic layered layout, and the `yrs` map of Excalidraw elements.
//!
//! An LLM can supply the topology and labels. Rust computes all positions without a model or API call.
//! [`BoardDoc`] uses a root `Y.Map` named [`BOARD`]. The [`ELEMENTS`] key holds a `Y.Array` of
//! Excalidraw elements that `restoreElements` can read.
//! Each node has a rectangle and bound text. Each edge has an arrow bound to both nodes.
//! Group members and their group label share a `groupIds` entry.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use yrs::{
    Any, Array, ArrayRef, Doc, Map as _, MapRef, Number, Out, ReadTxn, StateVector, Transact,
    Update, updates::decoder::Decode,
};

use crate::Error;

pub const NODE_WIDTH: f64 = 180.0;
// shortcut: long labels can extend outside the fixed 160 × 25 text box; revisit if labels must fit inside nodes.
pub const NODE_HEIGHT: f64 = 80.0;
/// Horizontal space between layers.
pub const LAYER_GAP: f64 = 220.0;
/// Vertical space between rows.
pub const ROW_GAP: f64 = 72.0;
/// Space between the top member of a group and its label above it.
pub const GROUP_LABEL_OFFSET: f64 = 30.0;
/// Name of the root `Y.Map`, read and written by the web round trip.
pub const BOARD: &str = "board";
/// Key in that map of the `Y.Array` of Excalidraw elements.
pub const ELEMENTS: &str = "elements";

const STROKE: &str = "#1e1e1e";
const NODE_FILL: &str = "#a5d8ff";
const UPDATED: i64 = 1_700_000_000_000;
const FONT_FAMILY: i64 = 5;
const MAX_LABEL_WIDTH: f64 = 100.0;
const MAX_EDGE_LABEL_CHARS: usize = 16;

/// A board as the LLM names it: no coordinates.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Board {
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub groups: Vec<Group>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlacedGroup {
    pub id: String,
    pub label: String,
    /// Position of the group label.
    pub x: f64,
    pub y: f64,
}

/// Every position the layout computed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layout {
    pub nodes: Vec<Placed>,
    pub groups: Vec<PlacedGroup>,
}

impl Board {
    /// Lay the board out in layers.
    ///
    /// Keep group members in one layer, even when a cycle makes an arrow point left.
    pub fn layout(&self) -> Result<Layout, Error> {
        self.validate()?;
        let n = self.nodes.len();
        let index: HashMap<&str, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, node)| (node.id.as_str(), i))
            .collect();
        let lookup = |id: &str| {
            index
                .get(id)
                .copied()
                .ok_or_else(|| Error::Invalid(format!("unknown node {id:?}")))
        };
        let mut group_of = vec![None; n];
        let mut column: Vec<_> = (0..n).collect();
        for group in &self.groups {
            let first = lookup(&group.members[0])?;
            for member in &group.members {
                let i = lookup(member)?;
                group_of[i] = Some(group);
                column[i] = first;
            }
        }
        let mut edges = Vec::with_capacity(self.edges.len());
        for edge in &self.edges {
            let (from, to) = (column[lookup(&edge.from)?], column[lookup(&edge.to)?]);
            if from != to {
                edges.push((from, to));
            }
        }
        // Grouping can close a cycle. Drop each edge that closes one, found by a depth-first
        // walk in input order, so every other edge points right and the dropped arrow points left.
        let mut out = vec![Vec::new(); n];
        for &(from, to) in &edges {
            out[from].push(to);
        }
        let mut state = vec![0u8; n];
        let mut forward = Vec::with_capacity(edges.len());
        for root in 0..n {
            if state[root] != 0 {
                continue;
            }
            state[root] = 1;
            let mut stack = vec![(root, 0usize)];
            while let Some((node, next)) = stack.last_mut() {
                let node = *node;
                let Some(&to) = out[node].get(*next) else {
                    state[node] = 2;
                    stack.pop();
                    continue;
                };
                *next += 1;
                if state[to] != 1 {
                    forward.push((node, to));
                }
                if state[to] == 0 {
                    state[to] = 1;
                    stack.push((to, 0));
                }
            }
        }
        let mut layer = vec![0usize; n];
        for _ in 0..n {
            let mut changed = false;
            for &(from, to) in &forward {
                let next = layer[from] + 1;
                if next > layer[to] {
                    layer[to] = next;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        // Within a column, group members stay adjacent.
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|a, b| {
            let key = |i: usize| {
                (
                    group_of[i].map(|g| g.id.as_str()),
                    self.nodes[i].id.as_str(),
                )
            };
            key(*a).cmp(&key(*b))
        });

        let mut rows = vec![0; layer.iter().max().map_or(0, |rank| rank + 1)];
        let mut placed = vec![
            Placed {
                id: String::new(),
                x: 0.0,
                y: 0.0,
                width: NODE_WIDTH,
                height: NODE_HEIGHT,
            };
            n
        ];
        for i in order {
            let column = layer[column[i]];
            let row = rows[column];
            rows[column] += 1;
            placed[i] = Placed {
                id: self.nodes[i].id.clone(),
                x: column as f64 * (NODE_WIDTH + LAYER_GAP),
                y: row as f64 * (NODE_HEIGHT + ROW_GAP),
                width: NODE_WIDTH,
                height: NODE_HEIGHT,
            };
        }

        let mut groups = Vec::with_capacity(self.groups.len());
        for group in &self.groups {
            let members = group
                .members
                .iter()
                .map(|m| lookup(m).map(|i| &placed[i]))
                .collect::<Result<Vec<_>, _>>()?;
            groups.push(PlacedGroup {
                id: group.id.clone(),
                label: group.label.clone(),
                x: members.iter().map(|p| p.x).fold(f64::INFINITY, f64::min),
                y: members.iter().map(|p| p.y).fold(f64::INFINITY, f64::min) - GROUP_LABEL_OFFSET,
            });
        }

        // Shift so the topmost group label starts at or below the origin.
        let shift = groups.iter().map(|g| g.y).fold(0.0, f64::min);
        for p in &mut placed {
            p.y -= shift;
        }
        for g in &mut groups {
            g.y -= shift;
        }
        Ok(Layout {
            nodes: placed,
            groups,
        })
    }

    /// The board as Excalidraw elements, positions from [`Board::layout`].
    pub fn elements(&self) -> Result<Vec<Value>, Error> {
        let layout = self.layout()?;
        let group_of = self.group_ids();
        let mut bound: HashMap<&str, Vec<Value>> = HashMap::new();
        for (i, edge) in self.edges.iter().enumerate() {
            let arrow = format!("edge:{i}");
            for node in [edge.from.as_str(), edge.to.as_str()] {
                bound
                    .entry(node)
                    .or_default()
                    .push(json!({"type": "arrow", "id": arrow}));
            }
        }
        for node in &self.nodes {
            bound
                .entry(node.id.as_str())
                .or_default()
                .push(json!({"type": "text", "id": format!("text:{}", node.id)}));
        }

        let mut out = Vec::new();
        for node in &self.nodes {
            let p = layout.node(&node.id)?;
            let group_ids = match group_of.get(node.id.as_str()) {
                Some(g) => vec![format!("group:{g}")],
                None => Vec::new(),
            };
            let mut rect = base(
                &format!("node:{}", node.id),
                "rectangle",
                p.x,
                p.y,
                p.width,
                p.height,
                NODE_FILL,
            );
            rect.insert("groupIds".into(), json!(group_ids.clone()));
            rect.insert(
                "boundElements".into(),
                Value::Array(bound.remove(node.id.as_str()).unwrap_or_default()),
            );
            out.push(Value::Object(rect));

            out.push(bound_text(
                &format!("text:{}", node.id),
                &node.label,
                p.x + (p.width - 160.0) / 2.0,
                p.y + (p.height - 25.0) / 2.0,
                160.0,
                25.0,
                20,
                Some(format!("node:{}", node.id)),
                group_ids.clone(),
            ));
        }

        for (i, edge) in self.edges.iter().enumerate() {
            let (from, to) = (layout.node(&edge.from)?, layout.node(&edge.to)?);
            let route = route(from, to, &layout.nodes);
            let (sx, sy) = route[0];
            let xs = route.iter().map(|p| p.0);
            let ys = route.iter().map(|p| p.1);
            let width = xs.clone().fold(f64::MIN, f64::max) - xs.fold(f64::MAX, f64::min);
            let height = ys.clone().fold(f64::MIN, f64::max) - ys.fold(f64::MAX, f64::min);
            let mut arrow = base(
                &format!("edge:{i}"),
                "arrow",
                sx,
                sy,
                width,
                height,
                "transparent",
            );
            // A rounded multi-point arrow is a spline that can cut through the node it avoids.
            arrow.insert(
                "roundness".into(),
                if route.len() == 2 {
                    json!({"type": 2})
                } else {
                    Value::Null
                },
            );
            arrow.insert(
                "points".into(),
                Value::Array(route.iter().map(|p| json!([p.0 - sx, p.1 - sy])).collect()),
            );
            arrow.insert("lastCommittedPoint".into(), Value::Null);
            arrow.insert(
                "startBinding".into(),
                json!({"elementId": format!("node:{}", edge.from), "focus": 0, "gap": 4}),
            );
            arrow.insert(
                "endBinding".into(),
                json!({"elementId": format!("node:{}", edge.to), "focus": 0, "gap": 4}),
            );
            arrow.insert("startArrowhead".into(), Value::Null);
            arrow.insert("endArrowhead".into(), json!("arrow"));
            arrow.insert("elbowed".into(), json!(false));
            let mut arrow_bound = Vec::new();
            if let Some(label) = edge
                .label
                .as_deref()
                .filter(|label| !label.trim().is_empty())
            {
                let (mx, my) = label_anchor(&route);
                let (mask, height) = label_size(label);
                arrow_bound.push(json!({"type": "text", "id": format!("edge-text:{i}")}));
                let (tx, ty) = (mx - mask / 2.0, my - height / 2.0);
                let mut text = bound_text(
                    &format!("edge-text:{i}"),
                    label,
                    tx,
                    ty,
                    mask,
                    height,
                    16,
                    Some(format!("edge:{i}")),
                    Vec::new(),
                );
                text["autoResize"] = json!(false);
                out.push(text);
            }
            arrow.insert("boundElements".into(), Value::Array(arrow_bound));
            out.push(Value::Object(arrow));
        }

        for group in &layout.groups {
            out.push(bound_text(
                &format!("label:{}", group.id),
                &group.label,
                group.x,
                group.y,
                120.0,
                22.0,
                14,
                None,
                vec![format!("group:{}", group.id)],
            ));
        }
        Ok(out)
    }

    fn group_ids(&self) -> HashMap<&str, &str> {
        self.groups
            .iter()
            .flat_map(|g| g.members.iter().map(move |m| (m.as_str(), g.id.as_str())))
            .collect()
    }

    fn validate(&self) -> Result<(), Error> {
        let mut ids = HashSet::new();
        for node in &self.nodes {
            if !ids.insert(node.id.as_str()) {
                return Err(Error::Invalid(format!("duplicate node id {:?}", node.id)));
            }
            if node.label.trim().is_empty() {
                return Err(Error::Invalid(format!(
                    "node {:?} has a blank label",
                    node.id
                )));
            }
        }
        for edge in &self.edges {
            let label = edge.label.as_deref().filter(|l| !l.trim().is_empty());
            if label.map_or(0, |l| l.chars().count()) > MAX_EDGE_LABEL_CHARS {
                return Err(Error::Invalid(format!(
                    "edge label is longer than {MAX_EDGE_LABEL_CHARS} characters"
                )));
            }
            if edge.from == edge.to {
                return Err(Error::Invalid(format!(
                    "edge loops back to node {:?}",
                    edge.from
                )));
            }
            for end in [&edge.from, &edge.to] {
                if !ids.contains(end.as_str()) {
                    return Err(Error::Invalid(format!("edge names unknown node {end:?}")));
                }
            }
        }
        let mut groups = HashSet::new();
        let mut member_of: HashMap<&str, &str> = HashMap::new();
        for group in &self.groups {
            if !groups.insert(group.id.as_str()) {
                return Err(Error::Invalid(format!("duplicate group id {:?}", group.id)));
            }
            if group.label.trim().is_empty() {
                return Err(Error::Invalid(format!(
                    "group {:?} has a blank label",
                    group.id
                )));
            }
            if group.members.is_empty() {
                return Err(Error::Invalid(format!(
                    "group {:?} has no members",
                    group.id
                )));
            }
            for member in &group.members {
                if !ids.contains(member.as_str()) {
                    return Err(Error::Invalid(format!(
                        "group {:?} names unknown node {member:?}",
                        group.id
                    )));
                }
                if member_of
                    .insert(member.as_str(), group.id.as_str())
                    .is_some()
                {
                    return Err(Error::Invalid(format!("repeated group member {member:?}")));
                }
            }
        }
        Ok(())
    }
}

impl Layout {
    pub fn node(&self, id: &str) -> Result<&Placed, Error> {
        self.nodes
            .iter()
            .find(|n| n.id == id)
            .ok_or_else(|| Error::Invalid(format!("unknown node {id:?}")))
    }
}

/// Arrow points from `from` to `to`: straight, or around the nodes between them.
///
/// Try a lane above both nodes, then a lane right of both. The lanes run in the gaps between
/// rows and layers, which no node occupies. Fall back to the straight line when no lane is clear.
fn route(from: &Placed, to: &Placed, nodes: &[Placed]) -> Vec<(f64, f64)> {
    let (sx, sy, ex, ey) = arrow_ends(from, to);
    let others = || nodes.iter().filter(|n| n.id != from.id && n.id != to.id);
    let clear = |pts: &[(f64, f64)]| {
        pts.windows(2)
            .all(|w| others().all(|n| !crosses(w[0], w[1], n)))
    };
    let straight = vec![(sx, sy), (ex, ey)];
    if clear(&straight) {
        return straight;
    }
    let lane_y = from.y.min(to.y) - ROW_GAP / 2.0;
    // Leave the node at three quarters of its width, clear of a group label above its left end.
    let (fx, tx) = (from.x + from.width * 0.75, to.x + to.width * 0.75);
    let top = vec![(fx, from.y), (fx, lane_y), (tx, lane_y), (tx, to.y)];
    let lane_x = (from.x + from.width).max(to.x + to.width) + LAYER_GAP / 2.0;
    let side = vec![
        (from.x + from.width, from.y + from.height / 2.0),
        (lane_x, from.y + from.height / 2.0),
        (lane_x, to.y + to.height / 2.0),
        (to.x + to.width, to.y + to.height / 2.0),
    ];
    // A same-row arrow goes over the top; a same-column arrow goes along the side.
    let candidates = if (from.y - to.y).abs() < NODE_HEIGHT {
        [top, side]
    } else {
        [side, top]
    };
    candidates
        .into_iter()
        .find(|c| clear(c))
        .unwrap_or(straight)
}

/// Whether the segment `a`-`b` passes through the rectangle of `n`.
fn crosses(a: (f64, f64), b: (f64, f64), n: &Placed) -> bool {
    let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
    for (p, d, lo, hi) in [
        (a.0, b.0 - a.0, n.x, n.x + n.width),
        (a.1, b.1 - a.1, n.y, n.y + n.height),
    ] {
        if d == 0.0 {
            if p < lo || p > hi {
                return false;
            }
        } else {
            let (u, v) = ((lo - p) / d, (hi - p) / d);
            t0 = t0.max(u.min(v));
            t1 = t1.min(u.max(v));
        }
    }
    t0 <= t1
}

/// Size of the label mask at font size 16: about 10 px a character, plus padding, wrapped at 100 px.
/// `validate` caps a label at two lines, so the mask stays inside the 72 px row gap and the lanes.
fn label_size(label: &str) -> (f64, f64) {
    let natural = label.chars().count() as f64 * 10.0 + 16.0;
    (
        natural.min(MAX_LABEL_WIDTH),
        20.0 * (natural / MAX_LABEL_WIDTH).ceil(),
    )
}

/// Where Excalidraw centres an arrow label: the midpoint of the middle segment of an even-length route.
fn label_anchor(route: &[(f64, f64)]) -> (f64, f64) {
    let i = route.len() / 2 - 1;
    let (a, b) = (route[i], route[i + 1]);
    ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
}

fn arrow_ends(from: &Placed, to: &Placed) -> (f64, f64, f64, f64) {
    let (cx1, cy1) = (from.x + from.width / 2.0, from.y + from.height / 2.0);
    let (cx2, cy2) = (to.x + to.width / 2.0, to.y + to.height / 2.0);
    let (dx, dy) = (cx2 - cx1, cy2 - cy1);
    let len = (dx * dx + dy * dy).sqrt();
    let (ux, uy) = (dx / len, dy / len);
    let reach = |w: f64, h: f64| {
        let tx = if ux == 0.0 {
            f64::INFINITY
        } else {
            (w / 2.0) / ux.abs()
        };
        let ty = if uy == 0.0 {
            f64::INFINITY
        } else {
            (h / 2.0) / uy.abs()
        };
        tx.min(ty)
    };
    let t1 = reach(from.width, from.height);
    let t2 = reach(to.width, to.height);
    (cx1 + ux * t1, cy1 + uy * t1, cx2 - ux * t2, cy2 - uy * t2)
}

fn base(
    id: &str,
    kind: &str,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    fill: &str,
) -> Map<String, Value> {
    json!({
        "id": id,
        "type": kind,
        "x": x,
        "y": y,
        "width": width,
        "height": height,
        "angle": 0,
        "strokeColor": STROKE,
        "backgroundColor": fill,
        "fillStyle": "solid",
        "strokeWidth": 2,
        "strokeStyle": "solid",
        "roughness": 1,
        "opacity": 100,
        "groupIds": [],
        "frameId": null,
        "roundness": null,
        "seed": seed(id),
        "version": 1,
        "versionNonce": seed(&format!("nonce:{id}")),
        "isDeleted": false,
        "boundElements": [],
        "updated": UPDATED,
        "link": null,
        "locked": false,
    })
    .as_object()
    .expect("base element is an object")
    .clone()
}

#[allow(clippy::too_many_arguments)]
fn bound_text(
    id: &str,
    text: &str,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    font_size: i64,
    container: Option<String>,
    group_ids: Vec<String>,
) -> Value {
    let mut el = base(id, "text", x, y, width, height, "transparent");
    el.insert("groupIds".into(), json!(group_ids));
    el.insert("boundElements".into(), Value::Null);
    el.insert("text".into(), json!(text));
    el.insert("originalText".into(), json!(text));
    el.insert("fontSize".into(), json!(font_size));
    el.insert("fontFamily".into(), json!(FONT_FAMILY));
    el.insert("textAlign".into(), json!("center"));
    el.insert("verticalAlign".into(), json!("middle"));
    el.insert(
        "containerId".into(),
        container.map(Value::String).unwrap_or(Value::Null),
    );
    el.insert("autoResize".into(), json!(true));
    el.insert("lineHeight".into(), json!(1.25));
    Value::Object(el)
}

/// Deterministic 31-bit seed from an id.
fn seed(id: &str) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    (hash & 0x7fff_ffff) as i64
}

/// A `yrs` document holding the board as a map whose `elements` key is an array of Excalidraw elements.
pub struct BoardDoc {
    doc: Doc,
    board: MapRef,
}

impl Default for BoardDoc {
    fn default() -> Self {
        Self::new()
    }
}

impl BoardDoc {
    pub fn new() -> Self {
        let doc = Doc::new();
        let board = doc.get_or_insert_map(BOARD);
        Self { doc, board }
    }

    pub fn from_update(update: &[u8]) -> Result<Self, Error> {
        let board = Self::new();
        let update = Update::decode_v1(update)?;
        board.doc.transact_mut().apply_update(update)?;
        Ok(board)
    }

    pub fn encode_update(&self) -> Vec<u8> {
        self.doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default())
    }

    /// Replace the whole array for a single writer. Do not use this method to merge concurrent edits.
    pub fn set_elements(&self, elements: &[Value]) {
        let mut txn = self.doc.transact_mut();
        let array: ArrayRef = self.board.get_or_init(&mut txn, ELEMENTS);
        let len = array.len(&txn);
        if len > 0 {
            array.remove_range(&mut txn, 0, len);
        }
        let values: Vec<Any> = elements.iter().map(value_to_any).collect();
        array.insert_range(&mut txn, 0, values);
    }

    pub fn elements(&self) -> Result<Vec<Value>, Error> {
        let txn = self.doc.transact();
        let array = match self.board.get(&txn, ELEMENTS) {
            None => return Ok(Vec::new()),
            Some(Out::YArray(array)) => array,
            Some(other) => return Err(Error::Unsupported(format!("elements of type {other:?}"))),
        };
        let mut out = Vec::with_capacity(array.len(&txn) as usize);
        for value in array.iter(&txn) {
            match value {
                Out::Any(any) => out.push(any_to_value(&any)?),
                other => return Err(Error::Unsupported(format!("element of type {other:?}"))),
            }
        }
        Ok(out)
    }
}

fn value_to_any(value: &Value) -> Any {
    match value {
        Value::Null => Any::Null,
        Value::Bool(b) => Any::Bool(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Any::Number(Number::Int(i)),
            None => Any::Number(Number::Float(n.as_f64().unwrap_or(0.0))),
        },
        Value::String(s) => Any::String(Arc::from(s.as_str())),
        Value::Array(items) => Any::Array(items.iter().map(value_to_any).collect()),
        Value::Object(map) => Any::Map(Arc::new(
            map.iter()
                .map(|(k, v)| (k.clone(), value_to_any(v)))
                .collect::<HashMap<_, _>>(),
        )),
    }
}

fn any_to_value(value: &Any) -> Result<Value, Error> {
    Ok(match value {
        Any::Null | Any::Undefined => Value::Null,
        Any::Bool(b) => Value::Bool(*b),
        Any::Number(Number::Int(i)) => json!(*i),
        Any::Number(Number::Float(f)) => json!(*f),
        Any::String(s) => Value::String(s.to_string()),
        Any::Array(items) => Value::Array(
            items
                .iter()
                .map(any_to_value)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Any::Map(map) => {
            let mut out = Map::new();
            for (key, value) in map.iter() {
                out.insert(key.clone(), any_to_value(value)?);
            }
            Value::Object(out)
        }
        other => return Err(Error::Unsupported(format!("value {other:?}"))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same_numbers(value: Value) -> Value {
        match value {
            Value::Number(n) => json!(n.as_f64()),
            Value::Array(items) => Value::Array(items.into_iter().map(same_numbers).collect()),
            Value::Object(map) => {
                Value::Object(map.into_iter().map(|(k, v)| (k, same_numbers(v))).collect())
            }
            other => other,
        }
    }

    fn sample() -> Board {
        Board {
            nodes: vec![
                Node {
                    id: "a".into(),
                    label: "Intake".into(),
                },
                Node {
                    id: "b".into(),
                    label: "Parse".into(),
                },
                Node {
                    id: "c".into(),
                    label: "Store".into(),
                },
                Node {
                    id: "d".into(),
                    label: "Report".into(),
                },
            ],
            edges: vec![
                Edge {
                    from: "a".into(),
                    to: "b".into(),
                    label: None,
                },
                Edge {
                    from: "a".into(),
                    to: "c".into(),
                    label: None,
                },
                Edge {
                    from: "b".into(),
                    to: "d".into(),
                    label: Some("done".into()),
                },
                Edge {
                    from: "c".into(),
                    to: "d".into(),
                    label: None,
                },
            ],
            groups: vec![Group {
                id: "input".into(),
                label: "Input".into(),
                members: vec!["a".into(), "b".into()],
            }],
        }
    }

    #[test]
    fn layout_is_deterministic() {
        let board = sample();
        assert_eq!(board.layout().unwrap(), board.layout().unwrap());
    }

    #[test]
    fn layers_follow_edges_and_groups_share_a_column() {
        let board = Board {
            groups: vec![],
            ..sample()
        };
        let layout = board.layout().unwrap();
        let x = |id: &str| layout.node(id).unwrap().x;
        assert!(x("b") > x("a"), "a -> b goes right");
        assert!(x("d") > x("b"), "b -> d goes right");

        let grouped = sample().layout().unwrap();
        assert_eq!(grouped.node("a").unwrap().x, grouped.node("b").unwrap().x);
        let group = &grouped.groups[0];
        assert_eq!(group.x, grouped.node("a").unwrap().x);
        assert!(group.y < grouped.node("a").unwrap().y);
    }

    #[test]
    fn nodes_never_overlap() {
        for board in [
            sample(),
            Board {
                groups: vec![],
                ..sample()
            },
        ] {
            let layout = board.layout().unwrap();
            for (i, a) in layout.nodes.iter().enumerate() {
                for b in &layout.nodes[i + 1..] {
                    let apart = a.x + a.width <= b.x
                        || b.x + b.width <= a.x
                        || a.y + a.height <= b.y
                        || b.y + b.height <= a.y;
                    assert!(apart, "{} overlaps {}", a.id, b.id);
                }
            }
        }
    }

    #[test]
    fn cycles_do_not_hang_and_stay_deterministic() {
        let board = Board {
            nodes: vec![
                Node {
                    id: "a".into(),
                    label: "A".into(),
                },
                Node {
                    id: "b".into(),
                    label: "B".into(),
                },
            ],
            edges: vec![
                Edge {
                    from: "a".into(),
                    to: "b".into(),
                    label: None,
                },
                Edge {
                    from: "b".into(),
                    to: "a".into(),
                    label: None,
                },
            ],
            groups: vec![],
        };
        assert_eq!(board.layout().unwrap(), board.layout().unwrap());
    }

    #[test]
    fn collapsing_a_group_keeps_external_edges_pointing_right() {
        let node = |id: &str| Node {
            id: id.into(),
            label: id.into(),
        };
        let edge = |from: &str, to: &str| Edge {
            from: from.into(),
            to: to.into(),
            label: None,
        };
        let board = Board {
            nodes: vec![node("a"), node("b"), node("c"), node("d")],
            edges: vec![edge("a", "b"), edge("c", "d")],
            groups: vec![Group {
                id: "g".into(),
                label: "G".into(),
                members: vec!["b".into(), "c".into()],
            }],
        };
        let layout = board.layout().unwrap();
        let x = |id: &str| layout.node(id).unwrap().x;
        assert_eq!(x("b"), x("c"), "the group keeps one column");
        assert!(x("b") > x("a"));
        assert!(x("d") > x("c"), "the edge after the group points right");
    }

    #[test]
    fn groups_need_a_member_but_not_two() {
        let mut board = sample();
        board.groups[0].members.truncate(1);
        let elements = board.elements().unwrap();
        for id in ["node:a", "label:input"] {
            let element = elements.iter().find(|e| e["id"] == id).unwrap();
            assert_eq!(element["groupIds"], json!(["group:input"]));
        }
        let other = elements.iter().find(|e| e["id"] == "node:b").unwrap();
        assert_eq!(other["groupIds"], json!([]));
        board.groups[0].members.clear();
        assert!(board.layout().is_err());
    }

    #[test]
    fn cycle_through_a_group_keeps_the_external_edge_pointing_right() {
        let node = |id: &str| Node {
            id: id.into(),
            label: id.into(),
        };
        let edge = |from: &str, to: &str| Edge {
            from: from.into(),
            to: to.into(),
            label: None,
        };
        let board = Board {
            nodes: vec![node("a1"), node("a2"), node("b"), node("c")],
            edges: vec![edge("b", "c"), edge("a1", "b"), edge("b", "a2")],
            groups: vec![Group {
                id: "g".into(),
                label: "G".into(),
                members: vec!["a1".into(), "a2".into()],
            }],
        };
        let layout = board.layout().unwrap();
        let x = |id: &str| layout.node(id).unwrap().x;
        assert_eq!(x("a1"), x("a2"));
        assert!(x("b") > x("a1"));
        assert!(x("c") > x("b"), "b -> c must not be the reversed edge");
    }

    #[test]
    fn empty_group_id_does_not_pull_in_ungrouped_nodes() {
        let node = |id: &str| Node {
            id: id.into(),
            label: id.into(),
        };
        let board = Board {
            nodes: vec![node("a"), node("b"), node("c")],
            edges: vec![],
            groups: vec![Group {
                id: String::new(),
                label: "G".into(),
                members: vec!["a".into(), "c".into()],
            }],
        };
        let layout = board.layout().unwrap();
        let gap = (layout.node("a").unwrap().y - layout.node("c").unwrap().y).abs();
        assert_eq!(gap, NODE_HEIGHT + ROW_GAP, "a and c are neighbours");
    }

    #[test]
    fn unknown_names_are_rejected() {
        let mut board = sample();
        board.edges[0].to = "zz".into();
        assert!(board.layout().is_err());
    }

    #[test]
    fn blank_required_labels_are_rejected() {
        let mut board = sample();
        board.nodes[0].label = " \t\n".into();
        assert!(matches!(board.elements(), Err(Error::Invalid(_))));

        let mut board = sample();
        board.groups[0].label = "".into();
        assert!(matches!(board.elements(), Err(Error::Invalid(_))));
    }

    #[test]
    fn blank_optional_label_emits_neither_text_nor_binding() {
        let mut board = sample();
        board.edges[2].label = Some(" \t\n".into());
        let elements = board.elements().unwrap();
        assert!(!elements.iter().any(|e| e["id"] == "edge-text:2"));
        let arrow = elements.iter().find(|e| e["id"] == "edge:2").unwrap();
        assert_eq!(arrow["boundElements"], json!([]));
    }

    #[test]
    fn elements_have_bound_arrows_and_group_ids() {
        let board = sample();
        let elements = board.elements().unwrap();
        let find = |id: &str| elements.iter().find(|e| e["id"] == json!(id)).unwrap();
        let arrow = elements
            .iter()
            .find(|e| e["id"] == json!("edge:2"))
            .unwrap();
        assert_eq!(arrow["startBinding"]["elementId"], json!("node:b"));
        assert_eq!(arrow["endBinding"]["elementId"], json!("node:d"));
        assert!(
            find("node:b")["boundElements"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["id"] == json!("edge:2") && b["type"] == json!("arrow"))
        );
        assert_eq!(find("node:a")["groupIds"], json!(["group:input"]));
        assert_eq!(find("text:a")["containerId"], json!("node:a"));
        let label = find("edge-text:2");
        assert_eq!(label["text"], "done");
        assert_eq!(label["containerId"], "edge:2");
        assert_eq!(
            arrow["boundElements"],
            json!([{"id": "edge-text:2", "type": "text"}])
        );
    }

    #[test]
    fn map_round_trips_through_an_update() {
        let elements = sample().elements().unwrap();
        let doc = BoardDoc::new();
        doc.set_elements(&elements);
        let back = BoardDoc::from_update(&doc.encode_update())
            .unwrap()
            .elements()
            .unwrap();
        // Yjs stores JavaScript numbers as doubles, so compare numbers by value.
        assert_eq!(
            same_numbers(Value::Array(back)),
            same_numbers(Value::Array(elements))
        );

        // The Yjs contract the web side relies on: a root map whose "elements" key is an array.
        let raw = Doc::new();
        raw.transact_mut()
            .apply_update(Update::decode_v1(&doc.encode_update()).unwrap())
            .unwrap();
        let root = raw.get_or_insert_map("board");
        let txn = raw.transact();
        assert!(matches!(root.get(&txn, "elements"), Some(Out::YArray(_))));
    }

    fn chain(edges: &[(&str, &str, Option<&str>)], ids: &[&str]) -> Vec<Value> {
        Board {
            nodes: ids
                .iter()
                .map(|id| Node {
                    id: (*id).into(),
                    label: (*id).into(),
                })
                .collect(),
            edges: edges
                .iter()
                .map(|(from, to, label)| Edge {
                    from: (*from).into(),
                    to: (*to).into(),
                    label: label.map(Into::into),
                })
                .collect(),
            groups: Vec::new(),
        }
        .elements()
        .unwrap()
    }

    fn abs_points(arrow: &Value) -> Vec<(f64, f64)> {
        let (x, y) = (arrow["x"].as_f64().unwrap(), arrow["y"].as_f64().unwrap());
        arrow["points"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (x + p[0].as_f64().unwrap(), y + p[1].as_f64().unwrap()))
            .collect()
    }

    fn rect(elements: &[Value], id: &str) -> Placed {
        let e = elements.iter().find(|e| e["id"] == id).unwrap();
        Placed {
            id: id.into(),
            x: e["x"].as_f64().unwrap(),
            y: e["y"].as_f64().unwrap(),
            width: e["width"].as_f64().unwrap(),
            height: e["height"].as_f64().unwrap(),
        }
    }

    #[test]
    fn arrows_route_around_a_node_in_the_same_row_or_column() {
        // a, b, c share a row: a->c would cross b.
        let row = chain(
            &[("a", "b", None), ("b", "c", None), ("a", "c", None)],
            &["a", "b", "c"],
        );
        // A group puts x, y, z in one column: x->z would cross y.
        let mut board = Board {
            nodes: ["x", "y", "z"]
                .iter()
                .map(|id| Node {
                    id: (*id).into(),
                    label: (*id).into(),
                })
                .collect(),
            edges: vec![Edge {
                from: "x".into(),
                to: "z".into(),
                label: None,
            }],
            groups: Vec::new(),
        };
        board.groups.push(Group {
            id: "g".into(),
            label: "G".into(),
            members: vec!["x".into(), "y".into(), "z".into()],
        });
        let column = board.elements().unwrap();
        for (elements, blocker, edge) in [(&row, "node:b", "edge:2"), (&column, "node:y", "edge:0")]
        {
            let arrow = elements.iter().find(|e| e["id"] == edge).unwrap();
            let points = abs_points(arrow);
            assert!(points.len() > 2, "{edge} is routed");
            let n = rect(elements, blocker);
            for w in points.windows(2) {
                assert!(!crosses(w[0], w[1], &n), "{edge} crosses {blocker}");
            }
        }
    }

    #[test]
    fn a_label_leaves_the_arrow_shaft_visible() {
        let long = "sixteen chars lb";
        let row = chain(
            &[
                ("a", "b", Some("ready")),
                ("b", "c", Some(long)),
                ("a", "c", Some(long)),
            ],
            &["a", "b", "c"],
        );
        let column = Board {
            nodes: ["x", "y", "z"]
                .iter()
                .map(|id| Node {
                    id: (*id).into(),
                    label: (*id).into(),
                })
                .collect(),
            edges: vec![
                Edge {
                    from: "x".into(),
                    to: "y".into(),
                    label: Some("ok".into()),
                },
                Edge {
                    from: "y".into(),
                    to: "z".into(),
                    label: Some(long.into()),
                },
                Edge {
                    from: "x".into(),
                    to: "z".into(),
                    label: Some(long.into()),
                },
            ],
            groups: vec![Group {
                id: "g".into(),
                label: "G".into(),
                members: vec!["x".into(), "y".into(), "z".into()],
            }],
        }
        .elements()
        .unwrap();
        for (elements, edges) in [(&row, 3), (&column, 3)] {
            for i in 0..edges {
                let arrow = elements
                    .iter()
                    .find(|e| e["id"] == format!("edge:{i}"))
                    .unwrap();
                let text = rect(elements, &format!("edge-text:{i}"));
                let (x0, x1) = (text.x, text.x + text.width);
                let (y0, y1) = (text.y, text.y + text.height);
                // Excalidraw ignores the stored position of an arrow label and centres it on the
                // midpoint of the arrow's middle segment. The mask then hides its own extent along that segment.
                let points = abs_points(arrow);
                let (a, b) = (points[points.len() / 2 - 1], points[points.len() / 2]);
                let extent = if (a.0 - b.0).abs() < 1.0 {
                    text.height
                } else {
                    text.width
                };
                let shaft = (a.0 - b.0).hypot(a.1 - b.1);
                assert!(
                    shaft - extent >= 24.0,
                    "edge {i}: mask {extent} hides shaft {shaft}"
                );
                let centre = (text.x + text.width / 2.0, text.y + text.height / 2.0);
                assert_eq!(centre, ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0));
                for n in elements
                    .iter()
                    .filter(|e| e["id"].as_str().is_some_and(|id| id.starts_with("node:")))
                {
                    let (nx, ny) = (n["x"].as_f64().unwrap(), n["y"].as_f64().unwrap());
                    let (nw, nh) = (n["width"].as_f64().unwrap(), n["height"].as_f64().unwrap());
                    assert!(
                        x0 >= nx + nw || x1 <= nx || y0 >= ny + nh || y1 <= ny,
                        "edge {i} label covers {}",
                        n["id"]
                    );
                }
            }
        }
    }

    #[test]
    fn an_edge_label_longer_than_two_lines_is_rejected() {
        let too_long = "x".repeat(MAX_EDGE_LABEL_CHARS + 1);
        let board = Board {
            nodes: ["a", "b"]
                .iter()
                .map(|id| Node {
                    id: (*id).into(),
                    label: (*id).into(),
                })
                .collect(),
            edges: vec![Edge {
                from: "a".into(),
                to: "b".into(),
                label: Some(too_long),
            }],
            groups: Vec::new(),
        };
        assert!(matches!(board.elements(), Err(Error::Invalid(_))));
    }
}
