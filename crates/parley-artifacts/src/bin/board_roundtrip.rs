//! Board -> layout -> Excalidraw elements -> Yjs -> Excalidraw -> Yjs -> board round trip.
//! Run `cargo run -p parley-artifacts --bin board_roundtrip` (needs `bun install` in
//! web/excalidraw-roundtrip). Prints each check, then PASS or FAIL.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
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

fn run() -> Result<bool, Box<dyn std::error::Error>> {
    let board = sample();
    let elements = board.elements()?;
    let layout = board.layout()?;
    println!(
        "board: {} nodes, {} edges, {} groups -> layout {}x{}",
        board.nodes.len(),
        board.edges.len(),
        board.groups.len(),
        layout.width,
        layout.height
    );

    let doc = BoardDoc::new();
    doc.set_elements(&elements);
    let before = doc.elements()?;

    let dir = env::temp_dir().join(format!("parley-board-{}", std::process::id()));
    fs::create_dir_all(&dir)?;
    let (input, output, png) = (
        dir.join("board.bin"),
        dir.join("board-back.bin"),
        dir.join("board.png"),
    );
    fs::write(&input, doc.encode_update())?;

    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/excalidraw-roundtrip");
    let run = Command::new("bun")
        .args(["run", "roundtrip.ts"])
        .arg(canonical(&input))
        .arg(canonical(&output))
        .arg(canonical(&png))
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

    check("element count kept", back.len() == before.len());
    check("element ids kept", ids(&back) == ids(&before));

    let moved_id = report["moved"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let moved_before = element(&before, &moved_id)?;
    let expected = (
        moved_before["x"].as_f64().unwrap_or_default() + MOVE_X,
        moved_before["y"].as_f64().unwrap_or_default() + MOVE_Y,
    );
    let moved = element(&back, &moved_id)?;
    check(
        "moved node read back",
        close(moved["x"].as_f64(), Some(expected.0))
            && close(moved["y"].as_f64(), Some(expected.1)),
    );

    let text_id = report["moved"]["text_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let text_before = element(&before, &text_id)?;
    let text = element(&back, &text_id)?;
    check(
        "bound text followed the node",
        close(
            text["x"].as_f64(),
            text_before["x"].as_f64().map(|x| x + MOVE_X),
        ) && close(
            text["y"].as_f64(),
            text_before["y"].as_f64().map(|y| y + MOVE_Y),
        ),
    );

    let mut arrows_kept = true;
    let mut groups_kept = true;
    let mut others_kept = true;
    for (old, new) in before.iter().zip(back.iter()) {
        let id = old["id"].clone();
        if old["type"] == "arrow" {
            arrows_kept &= old["startBinding"] == new["startBinding"]
                && old["endBinding"] == new["endBinding"];
        }
        if old["groupIds"] != new["groupIds"] {
            groups_kept = false;
        }
        if id.as_str() != Some(moved_id.as_str()) && id.as_str() != Some(text_id.as_str()) {
            others_kept &= close(old["x"].as_f64(), new["x"].as_f64())
                && close(old["y"].as_f64(), new["y"].as_f64());
        }
    }
    check("arrow bindings kept", arrows_kept);
    check("group ids kept", groups_kept);
    check("other nodes unmoved", others_kept);
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

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}
