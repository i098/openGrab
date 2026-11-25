use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn attach_writes_evidence_only_to_a_new_owner_only_directory() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("p0-exit-test-{}-{nonce}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let run = |extra: &[&str]| {
        Command::new("sh")
            .args([
                "-c",
                "umask 022; exec \"$@\"",
                "sh",
                env!("CARGO_BIN_EXE_p0_exit"),
                "https://meet.google.com/abc-defg-hij",
                "--attach",
                "9523",
                "--joins",
                "0",
                "--checks",
                "join",
            ])
            .args(extra)
            .env("TMPDIR", &root)
            .output()
            .unwrap()
    };
    let check_evidence = |path: &std::path::Path| {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(path.join("joins.jsonl").is_file());
    };

    let default = run(&[]);
    assert!(String::from_utf8_lossy(&default.stderr).contains("not in the call"));
    let out = fs::read_dir(&root).unwrap().next().unwrap().unwrap().path();
    check_evidence(&out);

    let custom = root.join("custom");
    let custom_arg = ["--out", custom.to_str().unwrap()];
    run(&custom_arg);
    check_evidence(&custom);
    let report = custom.join("joins.jsonl");
    fs::write(&report, "previous run").unwrap();
    let rejected = run(&custom_arg);
    assert!(!rejected.status.success());
    assert_eq!(fs::read_to_string(report).unwrap(), "previous run");

    let existing = root.join("shared");
    fs::create_dir(&existing).unwrap();
    fs::set_permissions(&existing, fs::Permissions::from_mode(0o755)).unwrap();
    let alias = root.join("alias");
    symlink(&existing, &alias).unwrap();
    let rejected = run(&["--out", alias.to_str().unwrap()]);
    assert!(!rejected.status.success());
    assert!(fs::read_dir(existing).unwrap().next().is_none());
    fs::remove_dir_all(root).unwrap();
}
