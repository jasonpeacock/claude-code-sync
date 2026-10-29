//! A real backup and a real restore, against throwaway home directories.
//!
//! Everything lives in one test on purpose: `CLAUDE_SYNC_HOME` is process-wide, and cargo runs
//! the tests inside a file on parallel threads, so a second test setting it would race this one.

use std::fs;
use std::path::{Path, PathBuf};

use claude_code_sync::app::{backup, read_manifest, restore, RestoreOptions, HOME_OVERRIDE};
use claude_code_sync::archive::{read_zip, Entry, Manifest};
use claude_code_sync::classify::BackupOptions;
use claude_code_sync::merge::{Choice, Conflict, MergeStrategy};

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn use_home(home: &Path) {
    std::env::set_var(HOME_OVERRIDE, home);
}

fn take(home: &Path, options: &BackupOptions) -> (Vec<Entry>, Manifest) {
    use_home(home);
    let (bytes, _) = backup(options, "2026-08-20T00:00:00Z".into(), "test-host".into()).unwrap();
    let entries = read_zip(bytes).unwrap();
    let manifest = read_manifest(&entries).unwrap();
    (entries, manifest)
}

fn carried(manifest: &Manifest) -> Vec<&str> {
    manifest.files.iter().map(|f| f.path.as_str()).collect()
}

fn apply(entries: &[Entry], manifest: &Manifest, home: &Path, dry_run: bool) -> Vec<String> {
    use_home(home);
    let mut resolve = |_: &Conflict| Choice::Incoming;
    let mut options = RestoreOptions {
        strategy: MergeStrategy::Incoming,
        dry_run,
        resolve: &mut resolve,
    };
    restore(entries, manifest, &mut options).unwrap()
}

fn seed_source(home: &Path) {
    let claude = home.join(".claude");
    write(
        &claude.join("settings.json"),
        &format!(
            r#"{{
  "model": "opus",
  "hooks": {{ "SessionStart": [{{ "command": "{}/.claude/hooks/x.mjs" }}] }},
  "permissions": {{ "allow": ["Bash(npm view:*)", "Read(//c/src/\\(x\\)/**)"] }}
}}"#,
            home.to_string_lossy().replace('\\', "/")
        ),
    );
    write(&claude.join("CLAUDE.md"), "# rules\r\nsecond line\r\n");
    write(&claude.join("hooks/x.mjs"), "export default 1\n");
    write(&claude.join(".i-have-adhd-always"), "");
    write(&claude.join(".credentials.json"), r#"{"token":"secret"}"#);
    write(
        &claude.join("plugins/installed_plugins.json"),
        &format!(
            r#"{{"plugins":{{"p":[{{"installPath":"{}/.claude/plugins/cache/p/1.0"}}]}}}}"#,
            home.to_string_lossy().replace('\\', "/")
        ),
    );
    write(
        &claude.join("plugins/cache/some-plugin/1.0.0/SKILL.md"),
        "no",
    );
    write(
        &claude.join("projects/c--foo/memory/MEMORY.md"),
        "- a note\n",
    );
    write(&claude.join("projects/c--foo/session.jsonl"), "{}\n");
    write(&claude.join("shell-snapshots/snap.sh"), "echo hi\n");
    write(&home.join(".agents/skills/grilling/SKILL.md"), "# grill\n");
}

#[test]
fn a_config_survives_a_backup_and_a_restore_onto_a_different_machine() {
    let root: PathBuf = std::env::temp_dir().join(format!("ccs-round-trip-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (source, target, untouched) = (
        root.join("home-a"),
        root.join("home-b"),
        root.join("home-c"),
    );
    seed_source(&source);

    // --- backup ---------------------------------------------------------------------------
    let (entries, manifest) = take(&source, &BackupOptions::default());
    let files = carried(&manifest);

    for wanted in [
        "claude/settings.json",
        "claude/CLAUDE.md",
        "claude/hooks/x.mjs",
        "claude/.i-have-adhd-always",
        "claude/plugins/installed_plugins.json",
        "agents/skills/grilling/SKILL.md",
    ] {
        assert!(
            files.contains(&wanted),
            "{wanted} was not carried: {files:?}"
        );
    }
    for refused in [
        "claude/.credentials.json",
        "claude/plugins/cache/some-plugin/1.0.0/SKILL.md",
        "claude/projects/c--foo/memory/MEMORY.md",
        "claude/projects/c--foo/session.jsonl",
        "claude/shell-snapshots/snap.sh",
    ] {
        assert!(
            !files.contains(&refused),
            "{refused} leaked into the archive"
        );
    }
    assert!(!manifest.includes_credentials);

    // The archive stores LF whatever the machine writing it uses.
    let archived_md = entries
        .iter()
        .find(|e| e.path == "claude/CLAUDE.md")
        .expect("CLAUDE.md missing from the archive");
    assert_eq!(archived_md.data, b"# rules\nsecond line\n");

    // --with-memory reaches through projects/<p>/memory without pulling the transcript beside it.
    let (_, with_memory) = take(
        &source,
        &BackupOptions {
            with_memory: true,
            ..Default::default()
        },
    );
    let files = carried(&with_memory);
    assert!(
        files.contains(&"claude/projects/c--foo/memory/MEMORY.md"),
        "{files:?}"
    );
    assert!(!files.contains(&"claude/projects/c--foo/session.jsonl"));

    // --- restore onto a machine with its own settings -------------------------------------
    write(
        &target.join(".claude/settings.json"),
        r#"{ "model": "sonnet", "localOnly": true, "permissions": { "allow": ["Bash(cargo:*)"] } }"#,
    );
    let actions = apply(&entries, &manifest, &target, false);
    let (commands, writes): (Vec<&String>, Vec<&String>) =
        actions.iter().partition(|a| a.starts_with("command"));
    assert_eq!(writes.len(), manifest.files.len());
    // The hook is new to this machine, and a hook is a command it will run, so it is named.
    assert_eq!(commands.len(), 1, "{actions:?}");
    assert!(
        commands[0].contains("hooks.SessionStart[0].command") && commands[0].contains("x.mjs"),
        "{commands:?}"
    );

    // A file written with LF stays LF everywhere; only a CRLF one goes back to CRLF, and only
    // on Windows. A shell hook given CRLF stops running.
    assert_eq!(
        fs::read_to_string(target.join(".claude/hooks/x.mjs")).unwrap(),
        "export default 1\n"
    );
    assert_eq!(
        fs::read_to_string(target.join(".claude/CLAUDE.md")).unwrap(),
        if cfg!(windows) {
            "# rules\r\nsecond line\r\n"
        } else {
            "# rules\nsecond line\n"
        }
    );
    assert!(target.join(".agents/skills/grilling/SKILL.md").exists());
    assert!(!target.join(".claude/.credentials.json").exists());

    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(target.join(".claude/settings.json")).unwrap()).unwrap();

    // Plugin pins name the plugin's install path, which has to move with the home too.
    let plugins: serde_json::Value = serde_json::from_slice(
        &fs::read(target.join(".claude/plugins/installed_plugins.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        plugins["plugins"]["p"][0]["installPath"],
        serde_json::json!(format!(
            "{}/.claude/plugins/cache/p/1.0",
            target.to_string_lossy().replace('\\', "/")
        ))
    );

    // The point of the whole tool: the hook now names THIS machine's home.
    assert_eq!(
        settings["hooks"]["SessionStart"][0]["command"],
        serde_json::json!(format!(
            "{}/.claude/hooks/x.mjs",
            target.to_string_lossy().replace('\\', "/")
        ))
    );
    assert_eq!(settings["model"], serde_json::json!("opus"));
    assert_eq!(settings["localOnly"], serde_json::json!(true));
    assert_eq!(
        settings["permissions"]["allow"],
        serde_json::json!([
            "Bash(cargo:*)",
            "Bash(npm view:*)",
            r"Read(//c/src/\(x\)/**)"
        ])
    );

    // Restoring the same archive again must produce the identical file, not a near-duplicate.
    let before = fs::read(target.join(".claude/settings.json")).unwrap();
    apply(&entries, &manifest, &target, false);
    assert_eq!(
        fs::read(target.join(".claude/settings.json")).unwrap(),
        before
    );

    // --- a dry run writes nothing ---------------------------------------------------------
    let planned = apply(&entries, &manifest, &untouched, true);
    assert_eq!(
        planned.iter().filter(|a| !a.starts_with("command")).count(),
        manifest.files.len()
    );
    assert!(
        !untouched.exists(),
        "a dry run created {}",
        untouched.display()
    );

    // --- a tampered entry stops the restore before anything is written --------------------
    let mut tampered: Vec<Entry> = entries
        .iter()
        .map(|e| Entry {
            path: e.path.clone(),
            data: e.data.clone(),
        })
        .collect();
    let hook = tampered
        .iter_mut()
        .find(|e| e.path == "claude/hooks/x.mjs")
        .unwrap();
    hook.data = b"os.system('rm -rf /')\n".to_vec();

    use_home(&untouched);
    let mut resolve = |_: &Conflict| Choice::Incoming;
    let mut options = RestoreOptions {
        strategy: MergeStrategy::Incoming,
        dry_run: false,
        resolve: &mut resolve,
    };
    let refused = restore(&tampered, &manifest, &mut options)
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("does not match its manifest hash"),
        "{refused}"
    );
    assert!(!untouched.exists(), "a refused archive still wrote files");

    fs::remove_dir_all(&root).unwrap();
}
