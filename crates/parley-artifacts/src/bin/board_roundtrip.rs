//! Board -> layout -> Excalidraw elements -> Yjs -> Excalidraw -> Yjs -> board round trip.
//! Run `cargo run -p parley-artifacts --bin board_roundtrip` (needs `bun install` in
//! web/excalidraw-roundtrip). Prints each check, then PASS or FAIL.

use std::{
    env, fs,
    os::unix::fs::DirBuilderExt,
    path::Path,
    process::{Command, ExitCode},
    time::{SystemTime, UNIX_EPOCH},
};

use parley_artifacts::board::{Board, BoardDoc, Edge, Group, Node};
use serde_json::Value;

const MOVE_X: f64 = 40.0;
const MOVE_Y: f64 = 20.0;
const TOLERANCE: f64 = 1e-6;

fn main() -> ExitCode {
    match run() {
        Ok(true) => {
            println!("PASS");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            println!("FAIL");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("FAIL: {e}");
            ExitCode::FAILURE
        }
    }
}

fn sample() -> Board {
    Board {
        nodes: vec![
            Node {
                id: "intake".into(),
                label: "Intake".into(),
            },
            Node {
                id: "parse".into(),
                label: "Parse".into(),
            },
            Node {
                id: "store".into(),
                label: "Store".into(),
            },
            Node {
                id: "report".into(),
                label: "Report".into(),
            },
            Node {
                id: "notify".into(),
                label: "Notify".into(),
            },
            Node {
                id: "archive".into(),
                label: "Archive".into(),
            },
        ],
        edges: vec![
            Edge {
                from: "intake".into(),
                to: "parse".into(),
                label: None,
            },
            Edge {
                from: "parse".into(),
                to: "store".into(),
                label: None,
            },
            Edge {
                from: "store".into(),
                to: "report".into(),
                label: Some("ready".into()),
            },
            Edge {
                from: "report".into(),
                to: "notify".into(),
                label: None,
            },
            Edge {
                from: "report".into(),
                to: "archive".into(),
                label: None,
            },
            Edge {
                from: "intake".into(),
                to: "store".into(),
                label: None,
            },
        ],
        groups: vec![
            Group {
                id: "input".into(),
                label: "Input".into(),
                members: vec!["intake".into(), "parse".into()],
            },
            Group {
                id: "output".into(),
                label: "Output".into(),
                members: vec!["notify".into(), "archive".into()],
            },
        ],
    }
}

/// Both ends of an arrow in scene coordinates.
fn ends(arrow: &Value) -> Option<((f64, f64), (f64, f64))> {
    let (x, y) = (arrow["x"].as_f64()?, arrow["y"].as_f64()?);
    let points = arrow["points"].as_array()?;
    let point = |p: &Value| Some((x + p[0].as_f64()?, y + p[1].as_f64()?));
    Some((point(points.first()?)?, point(points.last()?)?))
}

fn run() -> Result<bool, Box<dyn std::error::Error>> {
    let board = sample();
    let elements = board.elements()?;
    println!(
        "board: {} nodes, {} edges, {} groups",
        board.nodes.len(),
        board.edges.len(),
        board.groups.len(),
    );

    let doc = BoardDoc::new();
    doc.set_elements(&elements);
    let before = doc.elements()?;

    // DirBuilder::create fails on an existing path, so the directory is new and owned by us.
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let dir = env::temp_dir().join(format!("parley-board-{}-{nanos}", std::process::id()));
    fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let dir = dir.canonicalize()?;
    let (input, output, png) = (
        dir.join("board.bin"),
        dir.join("board-back.bin"),
        dir.join("board.png"),
    );
    fs::write(&input, doc.encode_update())?;

    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/excalidraw-roundtrip");
    let run = Command::new("bun")
        .args(["run", "roundtrip.ts"])
        .arg(&input)
        .arg(&output)
        .arg(&png)
        .current_dir(web)
        .output()?;
    let report: Value = serde_json::from_slice(&run.stdout).map_err(|e| {
        format!(
            "bun report is not JSON ({e}): {}",
            String::from_utf8_lossy(&run.stderr)
        )
    })?;
    if !run.status.success() {
        eprintln!("bun: {}", String::from_utf8_lossy(&run.stderr));
    }

    let back = BoardDoc::from_update(&fs::read(&output)?)?.elements()?;

    let mut ok = true;
    let mut check = |name: &str, passed: bool| {
        println!("{name}: {}", if passed { "ok" } else { "FAILED" });
        ok &= passed;
    };

    println!("excalidraw: {}", report["counts"]);
    for (name, value) in report["checks"].as_object().into_iter().flatten() {
        check(&format!("web {name}"), value == &Value::Bool(true));
    }
    check("web ok", report["ok"] == Value::Bool(true));
    check("bun exit", run.status.success());
    for reason in report["reasons"].as_array().into_iter().flatten() {
        println!("bun: {}", reason.as_str().unwrap_or_default());
    }
    check(
        "web node count",
        report["counts"]["nodes"].as_u64() == Some(board.nodes.len() as u64),
    );
    check(
        "web edge count",
        report["counts"]["edges"].as_u64() == Some(board.edges.len() as u64),
    );
    check(
        "web group count",
        report["counts"]["groups"].as_u64() == Some(board.groups.len() as u64),
    );

    check("element ids kept", ids(&back) == ids(&before));
    check(
        "no element deleted",
        back.iter().all(|e| e["isDeleted"] != Value::Bool(true)),
    );

    let moved_id = report["moved"]["id"].as_str().unwrap_or_default();
    let text_id = report["moved"]["text_id"].as_str().unwrap_or_default();
    let moved_before = element(&before, moved_id)?;
    let expected = (
        moved_before["x"].as_f64().unwrap_or_default() + MOVE_X,
        moved_before["y"].as_f64().unwrap_or_default() + MOVE_Y,
    );
    let moved = element(&back, moved_id)?;
    check(
        "moved node read back",
        close(moved["x"].as_f64(), Some(expected.0))
            && close(moved["y"].as_f64(), Some(expected.1)),
    );
    check("bound text exists", element(&before, text_id).is_ok());

    let mut arrows_kept = true;
    let mut ends_followed = true;
    let mut groups_kept = true;
    let mut others_kept = true;
    let mut texts_kept = true;
    for (old, new) in before.iter().zip(back.iter()) {
        let id = old["id"].as_str().unwrap_or_default();
        let shift = |bound: &Value| {
            if bound["elementId"].as_str() == Some(moved_id) {
                (MOVE_X, MOVE_Y)
            } else {
                (0.0, 0.0)
            }
        };
        if old["type"] == "arrow" {
            arrows_kept &= old["startBinding"] == new["startBinding"]
                && old["endBinding"] == new["endBinding"];
            let (start_shift, end_shift) = (shift(&old["startBinding"]), shift(&old["endBinding"]));
            ends_followed &= match (ends(old), ends(new)) {
                (Some((os, oe)), Some((ns, ne))) => {
                    close(Some(ns.0), Some(os.0 + start_shift.0))
                        && close(Some(ns.1), Some(os.1 + start_shift.1))
                        && close(Some(ne.0), Some(oe.0 + end_shift.0))
                        && close(Some(ne.1), Some(oe.1 + end_shift.1))
                }
                _ => false,
            };
        } else {
            let (dx, dy) = if id == moved_id || id == text_id {
                (MOVE_X, MOVE_Y)
            } else {
                (0.0, 0.0)
            };
            others_kept &= close(new["x"].as_f64(), old["x"].as_f64().map(|x| x + dx))
                && close(new["y"].as_f64(), old["y"].as_f64().map(|y| y + dy))
                && close(new["width"].as_f64(), old["width"].as_f64())
                && close(new["height"].as_f64(), old["height"].as_f64());
        }
        if old["type"] == "text" {
            texts_kept &= old["text"] == new["text"]
                && old["originalText"] == new["originalText"]
                && old["fontSize"] == new["fontSize"];
        }
        groups_kept &= old["groupIds"] == new["groupIds"];
    }
    check("arrow bindings kept", arrows_kept);
    check("arrow ends followed the node", ends_followed);
    check("text content kept", texts_kept);
    check("group ids kept", groups_kept);
    check("nodes and texts at expected positions", others_kept);
    check(
        "png written",
        fs::metadata(&png).map(|m| m.len() > 0).unwrap_or(false),
    );

    println!("moved {moved_id} to ({}, {})", expected.0, expected.1);
    println!("png: {}", png.display());
    fs::remove_file(input)?;
    fs::remove_file(output)?;
    Ok(ok)
}

fn ids(elements: &[Value]) -> Vec<String> {
    elements
        .iter()
        .filter_map(|e| e["id"].as_str().map(str::to_string))
        .collect()
}

fn element<'a>(elements: &'a [Value], id: &str) -> Result<&'a Value, String> {
    elements
        .iter()
        .find(|e| e["id"].as_str() == Some(id))
        .ok_or_else(|| format!("element {id:?} missing"))
}

fn close(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() < TOLERANCE,
        _ => false,
    }
}
