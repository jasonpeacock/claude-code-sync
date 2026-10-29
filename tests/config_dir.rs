//! A config moved with CLAUDE_CONFIG_DIR, backed up and restored with the real binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn run(home: &Path, config: Option<&Path>, args: &[&str]) -> (bool, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_claude-code-sync"));
    command.args(args).env("CLAUDE_SYNC_HOME", home);
    match config {
        Some(config) => command.env("CLAUDE_CONFIG_DIR", config),
        None => command.env_remove("CLAUDE_CONFIG_DIR"),
    };
    let output = command.output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

fn hook(settings: &Path) -> String {
    let value: serde_json::Value = serde_json::from_slice(&fs::read(settings).unwrap()).unwrap();
    value["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn a_moved_config_directory_backs_up_and_restores_anywhere() {
    let root: PathBuf = std::env::temp_dir().join(format!("ccs-config-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let spell = |path: &Path| path.to_string_lossy().replace('\\', "/");

    // The source keeps its config in ~/cfg/claude, and its hook says so.
    let source = root.join("source");
    let moved = source.join("cfg/claude");
    write(&moved.join("CLAUDE.md"), "# rules\n");
    write(&moved.join("hooks/x.sh"), "echo hi\n");
    write(
        &moved.join("settings.json"),
        &format!(
            r#"{{ "hooks": {{ "Stop": [ {{ "hooks": [ {{ "type": "command", "command": "{}/hooks/x.sh" }} ] }} ] }} }}"#,
            spell(&moved)
        ),
    );
    let out = root.join("out");
    let (ok, text) = run(&source, Some(&moved), &["backup", "--out", &spell(&out)]);
    assert!(ok, "{text}");
    let archive = fs::read_dir(&out).unwrap().next().unwrap().unwrap().path();
    let archive = spell(&archive);

    // Onto a default layout: everything lands in ~/.claude, and the hook names it there.
    let plain = root.join("plain");
    fs::create_dir_all(&plain).unwrap();
    let (ok, text) = run(&plain, None, &["restore", &archive]);
    assert!(ok, "{text}");
    assert!(plain.join(".claude/CLAUDE.md").exists(), "{text}");
    assert_eq!(
        hook(&plain.join(".claude/settings.json")),
        format!("{}/.claude/hooks/x.sh", spell(&plain))
    );

    // Onto another moved config: it lands there instead.
    let target = root.join("target");
    let elsewhere = target.join("dot/claude");
    fs::create_dir_all(&elsewhere).unwrap();
    let (ok, text) = run(&target, Some(&elsewhere), &["restore", &archive]);
    assert!(ok, "{text}");
    assert!(elsewhere.join("CLAUDE.md").exists(), "{text}");
    assert!(!target.join(".claude/CLAUDE.md").exists());
    assert_eq!(
        hook(&elsewhere.join("settings.json")),
        format!("{}/hooks/x.sh", spell(&elsewhere))
    );

    // Under CLAUDE_SYNC_HOME, a CLAUDE_CONFIG_DIR outside that home is ignored, so a trial
    // restore can never reach a live config left in the environment.
    let live = root.join("live-config");
    fs::create_dir_all(&live).unwrap();
    let trial = root.join("trial");
    fs::create_dir_all(&trial).unwrap();
    let (ok, text) = run(&trial, Some(&live), &["restore", &archive]);
    assert!(ok, "{text}");
    assert!(trial.join(".claude/CLAUDE.md").exists(), "{text}");
    assert_eq!(fs::read_dir(&live).unwrap().count(), 0);

    fs::remove_dir_all(&root).unwrap();
}
