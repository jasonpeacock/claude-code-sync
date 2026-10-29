//! A restore rolled back with the real binary, then the rollback rolled back in turn.
//!
//! Driven through the binary because rollback lives in the command: it picks the snapshot,
//! checks it was taken here, and removes what the restore created.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use claude_code_sync::archive::{read_zip, write_zip};

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

fn run(home: &Path, args: &[&str]) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_claude-code-sync"))
        .args(args)
        .env("CLAUDE_SYNC_HOME", home)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

fn settings(home: &Path) -> serde_json::Value {
    serde_json::from_str(&read(&home.join(".claude/settings.json"))).unwrap()
}

#[test]
fn a_restore_rolls_back_and_the_rollback_rolls_back_too() {
    let root: PathBuf = std::env::temp_dir().join(format!("ccs-rollback-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (source, home, out) = (root.join("source"), root.join("home"), root.join("out"));

    write(&source.join(".claude/CLAUDE.md"), "# new\n");
    write(&source.join(".claude/settings.json"), r#"{ "model": "b" }"#);
    write(&source.join(".claude/commands/c.md"), "# c\n");
    let (ok, text) = run(&source, &["backup", "--out", out.to_str().unwrap()]);
    assert!(ok, "{text}");
    let archive = fs::read_dir(&out).unwrap().next().unwrap().unwrap().path();

    write(&home.join(".claude/CLAUDE.md"), "# old\n");
    write(
        &home.join(".claude/settings.json"),
        r#"{ "model": "a", "localOnly": true }"#,
    );
    let before = settings(&home);

    let (ok, text) = run(&home, &["restore", archive.to_str().unwrap()]);
    assert!(ok, "{text}");
    assert_eq!(read(&home.join(".claude/CLAUDE.md")), "# new\n");
    assert_eq!(settings(&home)["model"], "b");
    assert!(home.join(".claude/commands/c.md").exists());

    // A dry run lists the removal and changes nothing.
    let (ok, text) = run(&home, &["rollback", "--dry-run"]);
    assert!(ok, "{text}");
    assert!(text.contains("remove  claude/commands/c.md"), "{text}");
    assert_eq!(read(&home.join(".claude/CLAUDE.md")), "# new\n");

    // Rolling back puts back what was there and removes what the restore created, its
    // directory included. settings.json comes back exactly, not merged with the archive's.
    let (ok, text) = run(&home, &["rollback"]);
    assert!(ok, "{text}");
    assert_eq!(read(&home.join(".claude/CLAUDE.md")), "# old\n");
    assert_eq!(settings(&home), before);
    assert!(!home.join(".claude/commands").exists(), "{text}");

    // The rollback took its own snapshot, so the newest one undoes it.
    let (ok, text) = run(&home, &["rollback"]);
    assert!(ok, "{text}");
    assert_eq!(read(&home.join(".claude/CLAUDE.md")), "# new\n");
    assert_eq!(read(&home.join(".claude/commands/c.md")), "# c\n");

    // Only a snapshot in ~/.claude/backups, taken on this machine, is accepted.
    let (ok, text) = run(&home, &["rollback", archive.to_str().unwrap()]);
    assert!(!ok && text.contains("not a pre-restore"), "{text}");
    let stray = out.join("pre-restore-20200101-000000.zip");
    fs::copy(&archive, &stray).unwrap();
    let (ok, text) = run(&home, &["rollback", stray.to_str().unwrap()]);
    assert!(!ok && text.contains("is not in"), "{text}");
    // One from another machine, planted where snapshots live, is refused: a rollback deletes.
    let mut entries = read_zip(fs::read(&archive).unwrap()).unwrap();
    let mut manifest: serde_json::Value = serde_json::from_slice(&entries[0].data).unwrap();
    manifest["sourceHost"] = "elsewhere".into();
    manifest["created"] = serde_json::json!(["claude/CLAUDE.md"]);
    entries[0].data = serde_json::to_vec(&manifest).unwrap();
    let planted = home.join(".claude/backups/pre-restore-29991231-235959.zip");
    fs::write(&planted, write_zip(&entries).unwrap()).unwrap();
    let (ok, text) = run(&home, &["rollback"]);
    assert!(!ok && text.contains("not this machine"), "{text}");
    assert!(home.join(".claude/CLAUDE.md").exists());

    fs::remove_dir_all(&root).unwrap();
}
