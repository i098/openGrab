//! Embeds every file under `web/fonts` as `FONTS`, a list of (relative path, bytes).
use std::{env, fs, path::Path};

fn walk(dir: &Path, root: &Path, out: &mut String) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, root, out);
        } else {
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .replace('\\', "/");
            out.push_str(&format!(
                "({rel:?}, include_bytes!({:?}) as &[u8]),\n",
                p.to_str().unwrap()
            ));
        }
    }
}

fn main() {
    let root = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../web/fonts");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut body = String::new();
    walk(&root, &root, &mut body);
    let src = format!("pub static FONTS: &[(&str, &[u8])] = &[\n{body}];\n");
    fs::write(
        Path::new(&env::var("OUT_DIR").unwrap()).join("fonts.rs"),
        src,
    )
    .unwrap();
}
