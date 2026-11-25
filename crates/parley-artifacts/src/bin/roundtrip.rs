//! Rust -> Tiptap -> Rust round trip. Prints both JSON sides, then PASS or FAIL.
//! Run from anywhere: `cargo run -p parley-artifacts --bin roundtrip` (needs `bun install` in web/tiptap-roundtrip).

use std::{
    env, fs,
    path::Path,
    process::{Command, ExitCode},
};

use parley_artifacts::{Artifact, Span};
use serde_json::Value;

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

fn run() -> Result<bool, Box<dyn std::error::Error>> {
    let artifact = Artifact::new();
    artifact.append_paragraph(&[
        Span::plain("Hello "),
        Span::bold("bold"),
        Span::italic(" italic"),
    ]);
    artifact.insert_heading(0, 2, &[Span::plain("Meeting notes")]);
    artifact.append_bullet_list(&[vec![Span::plain("first")], vec![Span::bold("second")]]);
    let rust_json = artifact.to_json()?;

    let dir = env::temp_dir().join(format!("parley-roundtrip-{}", std::process::id()));
    fs::create_dir_all(&dir)?;
    let (input, output) = (dir.join("rust.bin"), dir.join("tiptap.bin"));
    fs::write(&input, artifact.encode_update())?;

    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/tiptap-roundtrip");
    let run = Command::new("bun")
        .args(["run", "roundtrip.ts"])
        .arg(&input)
        .arg(&output)
        .current_dir(web)
        .output()?;
    if !run.status.success() {
        return Err(format!("bun failed: {}", String::from_utf8_lossy(&run.stderr)).into());
    }
    let tiptap: Value = serde_json::from_slice(&run.stdout)?;
    let back = Artifact::from_update(&fs::read(&output)?)?.to_json()?;
    fs::remove_dir_all(&dir)?;

    println!("rust wrote   : {rust_json}");
    println!("tiptap read  : {}", tiptap["before"]);
    println!("tiptap edited: {}", tiptap["after"]);
    println!("rust read back: {back}");

    let ok_read = tiptap["before"] == rust_json;
    let ok_back = tiptap["after"] == back && back != rust_json;
    println!("tiptap read matches rust write: {ok_read}");
    println!("rust read matches tiptap edit : {ok_back}");
    Ok(ok_read && ok_back)
}
