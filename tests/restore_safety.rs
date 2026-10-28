//! Restores of crafted archives, and of real ones onto homes holding symlinks.
//!
//! One test on purpose, for the same reason as `round_trip.rs`: `CLAUDE_SYNC_HOME` is process-wide.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use claude_code_sync::app::{
    backup, read_manifest, restore, snapshot, RestoreOptions, HOME_OVERRIDE,
};
use claude_code_sync::archive::{read_zip, Entry, FileRecord, LinkRecord, Manifest};
use claude_code_sync::classify::BackupOptions;
use claude_code_sync::merge::{Choice, Conflict, MergeStrategy};
use sha2::{Digest, Sha256};

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// An archive a hostile sender could hand over: every entry listed and correctly hashed.
fn forge(files: &[(&str, &str)], links: &[(&str, &str)]) -> (Vec<Entry>, Manifest) {
    let mut manifest = Manifest::new("now".into(), "evil".into(), &BackupOptions::default());
    let mut entries = Vec::new();
    for (path, data) in files {
        manifest.files.push(FileRecord {
            path: path.to_string(),
            sha256: format!("{:x}", Sha256::digest(data.as_bytes())),
            size: data.len(),
            executable: false,
        });
        entries.push(Entry {
            path: path.to_string(),
            data: data.as_bytes().to_vec(),
        });
    }
    for (path, target) in links {
        manifest.links.push(LinkRecord {
            path: path.to_string(),
            target: target.to_string(),
        });
    }
    (entries, manifest)
}

fn run(entries: &[Entry], manifest: &Manifest, home: &Path) -> Result<Vec<String>, String> {
    std::env::set_var(HOME_OVERRIDE, home);
    let mut resolve = |_: &Conflict| Choice::Incoming;
    let mut options = RestoreOptions {
        strategy: MergeStrategy::Incoming,
        dry_run: false,
        resolve: &mut resolve,
    };
    restore(entries, manifest, &mut options).map_err(|e| e.to_string())
}

fn refused(entries: &[Entry], manifest: &Manifest, home: &Path, why: &str) {
    let error = run(entries, manifest, home).expect_err("a crafted archive was restored");
    assert!(error.contains(why), "{error}");
}

fn take(home: &Path) -> (Vec<Entry>, Manifest) {
    std::env::set_var(HOME_OVERRIDE, home);
    let (bytes, _) = backup(&BackupOptions::default(), "now".into(), "host".into()).unwrap();
    let entries = read_zip(bytes).unwrap();
    let manifest = read_manifest(&entries).unwrap();
    (entries, manifest)
}

#[test]
fn a_restore_never_writes_outside_the_two_roots() {
    let root: PathBuf = std::env::temp_dir().join(format!("ccs-safety-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let home = root.join("home");
    let outside = root.join("outside");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::create_dir_all(home.join(".agents/skills/k")).unwrap();
    write(&outside.join("secret"), "keep me");
    write(&home.join(".claude/.credentials.json"), "{}");
    fs::create_dir_all(home.join(".claude/projects/p")).unwrap();

    let skipped = |actions: &[String], path: &str, why: &str| {
        let wanted = format!("skipped  {path}: {why}");
        assert!(
            actions.iter().any(|a| a.starts_with(&wanted)),
            "{actions:?}"
        );
    };

    // --- only skill links are ever made ------------------------------------------------------
    // Each of these got past some wider rule tried before: out of the roots, `..` through
    // another link, a bare root, the transcript store, the credentials file.
    for (path, target) in [
        ("claude/skills/k", "${HOME}/../outside"),
        ("claude/skills/k", "/etc"),
        ("claude/skills/k", "../../../outside"),
        ("claude/skills/k", "${HOME}/.claude/skills/a/../.."),
        ("claude/skills/k", "${HOME}/.agents/x/y/a/../../.ssh"),
        ("claude/skills/k", "${HOME}/.claude"),
        ("claude/skills/k", "${HOME}/.claude/projects"),
        ("claude/skills/k", "${HOME}/.agents"),
        ("claude/skills/k", "${HOME}/.agents/skills/k/sub"),
        ("claude/skills/k", "${HOME}/.claude/.credentials.json"),
        ("claude/hooks/t", "${HOME}/.agents/skills/k"),
        ("claude/x", "${HOME}/.agents/skills"),
        ("agents/skills/a", "${HOME}/.agents/skills/k"),
    ] {
        let (entries, mut manifest) = forge(&[], &[(path, target)]);
        manifest.with_memory = true;
        let actions = run(&entries, &manifest, &home).unwrap();
        skipped(
            &actions,
            path,
            "only skill links into ~/.agents/skills are recreated",
        );
        assert!(
            fs::symlink_metadata(home.join(".claude/skills/k")).is_err(),
            "{target}"
        );
        assert!(
            fs::symlink_metadata(home.join(".claude/hooks/t")).is_err(),
            "{target}"
        );
    }

    // --- a skill link is made, spelled relative and clean, however the archive spelled it ----
    for target in [
        "${HOME}/.agents/skills/k",
        "../../.agents/skills/k",
        "${HOME}/.claude/skills/a/../../../.agents/skills/k",
    ] {
        let (entries, manifest) = forge(&[], &[("claude/skills/k", target)]);
        run(&entries, &manifest, &home).unwrap();
        assert_eq!(
            fs::read_link(home.join(".claude/skills/k")).unwrap(),
            Path::new("../../.agents/skills/k"),
            "{target}"
        );
        fs::remove_file(home.join(".claude/skills/k")).unwrap();
    }
    // A skill that is not there is not linked to, so no dangling link is left.
    let (entries, manifest) = forge(&[], &[("claude/skills/zz", "${HOME}/.agents/skills/zz")]);
    skipped(
        &run(&entries, &manifest, &home).unwrap(),
        "claude/skills/zz",
        "its target",
    );
    // Nor is a skill that is itself a link to somewhere else.
    symlink(&outside, home.join(".agents/skills/away")).unwrap();
    let (entries, manifest) = forge(
        &[],
        &[("claude/skills/away", "${HOME}/.agents/skills/away")],
    );
    skipped(
        &run(&entries, &manifest, &home).unwrap(),
        "claude/skills/away",
        "its target",
    );
    fs::remove_file(home.join(".agents/skills/away")).unwrap();
    fs::remove_dir_all(home.join(".claude/skills")).unwrap();
    // The whole skills directory can be one link.
    let (entries, manifest) = forge(&[], &[("claude/skills", "${HOME}/.agents/skills")]);
    run(&entries, &manifest, &home).unwrap();
    assert_eq!(
        fs::read_link(home.join(".claude/skills")).unwrap(),
        Path::new("../.agents/skills")
    );
    fs::remove_file(home.join(".claude/skills")).unwrap();

    // --- one path named twice, or a path under the archive's own link, is refused -------------
    let (entries, manifest) = forge(
        &[],
        &[
            ("claude/skills/k", "${HOME}/.agents/skills/k"),
            ("claude/skills/k", "${HOME}/.agents/skills/k"),
        ],
    );
    refused(&entries, &manifest, &home, "more than once");
    let (entries, manifest) = forge(
        &[("claude/skills/k/SKILL.md", "x")],
        &[("claude/skills/k", "${HOME}/.agents/skills/k")],
    );
    refused(&entries, &manifest, &home, "under its own link");
    // Case only makes two names one file where the filesystem ignores it.
    let (entries, manifest) = forge(&[("claude/hooks/X", "x"), ("claude/hooks/x", "y")], &[]);
    if cfg!(target_os = "macos") {
        refused(&entries, &manifest, &home, "more than once");
    } else {
        run(&entries, &manifest, &home).unwrap();
        fs::remove_dir_all(home.join(".claude/hooks")).unwrap();
    }

    // --- an entry outside claude/ and agents/ stops the restore -----------------------------
    let (entries, manifest) = forge(&[("claude/CLAUDE.md", "x"), ("etc/passwd", "x")], &[]);
    refused(&entries, &manifest, &home, "outside claude/ and agents/");
    assert!(!home.join(".claude/CLAUDE.md").exists());

    // --- no backup carries a .zip, so one aimed at the snapshots is skipped ------------------
    let (entries, manifest) = forge(&[("claude/backups/pre-restore-1.zip", "x")], &[]);
    skipped(
        &run(&entries, &manifest, &home).unwrap(),
        "claude/backups/pre-restore-1.zip",
        "a backup never carries",
    );
    assert!(!home.join(".claude/backups/pre-restore-1.zip").exists());

    // --- a link already on disk does not carry a write out of the roots ---------------------
    symlink(outside.join("secret"), home.join(".claude/CLAUDE.md")).unwrap();
    symlink(&outside, home.join(".claude/hooks")).unwrap();
    let (entries, manifest) = forge(
        &[
            ("claude/CLAUDE.md", "pwned"),
            ("claude/hooks/secret", "pwned"),
        ],
        &[],
    );
    let actions = run(&entries, &manifest, &home).unwrap();
    assert!(
        actions.iter().all(|a| a.starts_with("skipped")),
        "{actions:?}"
    );
    assert_eq!(
        fs::read_to_string(outside.join("secret")).unwrap(),
        "keep me"
    );
    fs::remove_file(home.join(".claude/CLAUDE.md")).unwrap();
    fs::remove_file(home.join(".claude/hooks")).unwrap();

    // --- nor into a corner backups leave alone, such as a plugin checkout ---------------------
    write(
        &home.join(".claude/plugins/marketplaces/m/skills/r/SKILL.md"),
        "v2",
    );
    symlink(
        home.join(".claude/plugins/marketplaces/m/skills/r"),
        home.join(".agents/skills/r"),
    )
    .unwrap();
    let (entries, manifest) = forge(&[("agents/skills/r/SKILL.md", "v1")], &[]);
    skipped(
        &run(&entries, &manifest, &home).unwrap(),
        "agents/skills/r/SKILL.md",
        "a symlink here leads into",
    );
    assert_eq!(
        fs::read_to_string(home.join(".claude/plugins/marketplaces/m/skills/r/SKILL.md")).unwrap(),
        "v2"
    );
    fs::remove_dir_all(home.join(".claude/plugins")).unwrap();
    fs::remove_file(home.join(".agents/skills/r")).unwrap();

    // --- a hard link, which no path check can see, is replaced rather than written into -------
    fs::hard_link(outside.join("secret"), home.join(".claude/CLAUDE.md")).unwrap();
    let (entries, manifest) = forge(&[("claude/CLAUDE.md", "new rules")], &[]);
    run(&entries, &manifest, &home).unwrap();
    assert_eq!(
        fs::read_to_string(outside.join("secret")).unwrap(),
        "keep me"
    );
    assert_eq!(
        fs::read_to_string(home.join(".claude/CLAUDE.md")).unwrap(),
        "new rules"
    );
    fs::remove_file(home.join(".claude/CLAUDE.md")).unwrap();

    // --- a bad settings.json is refused before any other file is written ---------------------
    let (entries, manifest) = forge(
        &[
            ("claude/CLAUDE.md", "x"),
            ("claude/settings.json", "{ not json"),
        ],
        &[],
    );
    refused(&entries, &manifest, &home, "settings.json");
    assert!(!home.join(".claude/CLAUDE.md").exists());

    // --- a dangling link at a link path is left alone, not written through ------------------
    fs::create_dir_all(home.join(".claude/skills")).unwrap();
    symlink(outside.join("gone"), home.join(".claude/skills/k")).unwrap();
    let (entries, manifest) = forge(&[], &[("claude/skills/k", "${HOME}/.agents/skills/k")]);
    assert_eq!(
        run(&entries, &manifest, &home).unwrap(),
        Vec::<String>::new()
    );
    assert!(!outside.join("gone").exists());
    fs::remove_dir_all(home.join(".claude/skills")).unwrap();

    // --- the shared-skills layout: ~/.claude/skills is itself a link into ~/.agents/skills ----
    let shared = root.join("shared");
    fs::create_dir_all(shared.join(".agents/skills")).unwrap();
    fs::create_dir_all(shared.join(".claude")).unwrap();
    symlink("../.agents/skills", shared.join(".claude/skills")).unwrap();
    let (entries, manifest) = forge(
        &[("agents/skills/x/SKILL.md", "skill body")],
        &[("claude/skills/x", "../../.agents/skills/x")],
    );
    let actions = run(&entries, &manifest, &shared).unwrap();
    assert!(
        !actions.iter().any(|a| a.starts_with("failed")),
        "{actions:?}"
    );
    assert_eq!(
        fs::read_to_string(shared.join(".agents/skills/x/SKILL.md")).unwrap(),
        "skill body"
    );

    // --- ~/.claude itself a link (stow): the made link still leads to the real skill ---------
    let stowed = root.join("stowed");
    write(&stowed.join(".agents/skills/k/SKILL.md"), "# real");
    write(&root.join("dotfiles/.agents/skills/k/SKILL.md"), "# decoy");
    fs::create_dir_all(root.join("dotfiles/claude")).unwrap();
    symlink(root.join("dotfiles/claude"), stowed.join(".claude")).unwrap();
    let (entries, manifest) = forge(&[], &[("claude/skills/k", "${HOME}/.agents/skills/k")]);
    run(&entries, &manifest, &stowed).unwrap();
    assert_eq!(
        fs::read_to_string(stowed.join(".claude/skills/k/SKILL.md")).unwrap(),
        "# real"
    );

    // --- ~/.agents/skills itself a link out of the roots: no link is made into it -----------
    let away = root.join("away");
    write(&outside.join("skills/k/SKILL.md"), "# outside");
    fs::create_dir_all(away.join(".agents")).unwrap();
    fs::create_dir_all(away.join(".claude")).unwrap();
    symlink(outside.join("skills"), away.join(".agents/skills")).unwrap();
    let (entries, manifest) = forge(&[], &[("claude/skills/k", "${HOME}/.agents/skills/k")]);
    skipped(
        &run(&entries, &manifest, &away).unwrap(),
        "claude/skills/k",
        "its target",
    );

    // --- a never-carried directory is not a skill, so nothing is copied under a carried name -
    fs::create_dir_all(home.join(".agents/skills/.git")).unwrap();
    let (entries, manifest) = forge(&[], &[("claude/skills/k", "${HOME}/.agents/skills/.git")]);
    skipped(
        &run(&entries, &manifest, &home).unwrap(),
        "claude/skills/k",
        "only skill links",
    );

    // --- a file under another file is refused before anything is written --------------------
    let (entries, manifest) = forge(
        &[
            ("claude/CLAUDE.md", "x"),
            ("claude/hooks", "x"),
            ("claude/hooks/x.sh", "y"),
        ],
        &[],
    );
    refused(&entries, &manifest, &home, "which it also names");
    assert!(!home.join(".claude/CLAUDE.md").exists());

    fs::remove_dir_all(&root).unwrap();

    // --- backup: execute bits, skill links, and every other link left out ---------------------
    let source = root.join("source");
    let dotfiles = source.join("dotfiles");
    write(&source.join(".claude/hooks/run.sh"), "#!/bin/sh\n");
    fs::set_permissions(
        source.join(".claude/hooks/run.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    // Backup leaves out a .zip in any case, so restore never refuses its own archive over one.
    write(&source.join(".claude/hooks/assets.ZIP"), "PK");
    write(&source.join(".agents/skills/k/SKILL.md"), "# k\n");
    symlink("../../.agents/skills/k", source.join(".claude/skills/k")).unwrap_or_else(|_| {
        fs::create_dir_all(source.join(".claude/skills")).unwrap();
        symlink("../../.agents/skills/k", source.join(".claude/skills/k")).unwrap();
    });
    // None of these is a skill link, so none is carried, as a link or as what it points at.
    write(&dotfiles.join("CLAUDE.md"), "# rules\n");
    symlink(dotfiles.join("CLAUDE.md"), source.join(".claude/CLAUDE.md")).unwrap();
    write(&source.join(".agents/commands/c.md"), "# c\n");
    symlink(
        source.join(".agents/commands"),
        source.join(".claude/commands"),
    )
    .unwrap();
    write(&source.join(".ssh/id_rsa"), "PRIVATE");
    symlink(&source, source.join(".claude/agents")).unwrap();
    write(
        &source.join(".claude/projects/-work/sess.jsonl"),
        "{\"secret\":1}\n",
    );
    symlink(
        source.join(".claude/projects"),
        source.join(".claude/skills/helper"),
    )
    .unwrap();
    symlink("/usr/bin/env", source.join(".claude/hooks/env")).unwrap();
    write(&source.join(".agents/skills/py/bin/python"), "interpreter");
    symlink("python", source.join(".agents/skills/py/bin/python3")).unwrap();
    // Never carried: interpreter environments, stray credentials, FIFOs.
    write(
        &source.join(".agents/skills/py/.venv/lib/big"),
        "never carried",
    );
    write(&source.join(".agents/skills/py/.credentials.json"), "{}");
    let fifo = source.join(".agents/skills/py/pipe");
    assert!(std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());

    let (entries, manifest) = take(&source);
    let files: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
    for never in [
        "claude/CLAUDE.md",
        "claude/commands",
        "id_rsa",
        "sess.jsonl",
        "hooks/env",
        "python3",
        ".venv",
        ".credentials.json",
        "pipe",
        ".ZIP",
    ] {
        assert!(
            !files.iter().any(|f| f.contains(never)),
            "{never}: {files:?}"
        );
    }
    let links: Vec<(&str, &str)> = manifest
        .links
        .iter()
        .map(|l| (l.path.as_str(), l.target.as_str()))
        .collect();
    assert_eq!(links, vec![("claude/skills/k", "${HOME}/.agents/skills/k")]);
    assert!(manifest
        .files
        .iter()
        .any(|f| f.path == "claude/hooks/run.sh" && f.executable));

    let target = root.join("target");
    fs::create_dir_all(&target).unwrap();
    run(&entries, &manifest, &target).unwrap();
    let mode = fs::metadata(target.join(".claude/hooks/run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_ne!(mode & 0o100, 0, "the hook lost its execute bit: {mode:o}");
    assert_eq!(
        fs::read_to_string(target.join(".claude/skills/k/SKILL.md")).unwrap(),
        "# k\n"
    );

    // The whole skills directory linked into ~/.agents/skills travels as one link.
    let whole = root.join("whole");
    write(&whole.join(".agents/skills/k/SKILL.md"), "# k\n");
    fs::create_dir_all(whole.join(".claude")).unwrap();
    symlink("../.agents/skills", whole.join(".claude/skills")).unwrap();
    let (_, manifest) = take(&whole);
    let links: Vec<(&str, &str)> = manifest
        .links
        .iter()
        .map(|l| (l.path.as_str(), l.target.as_str()))
        .collect();
    assert_eq!(links, vec![("claude/skills", "${HOME}/.agents/skills")]);

    // --- the snapshot covers what the archive will overwrite, credentials included -----------
    write(&target.join(".claude/.credentials.json"), "{\"token\":1}");
    write(&target.join(".claude/projects/p/s.jsonl"), "{}\n");
    let (_, incoming) = forge(
        &[
            ("claude/.credentials.json", "{}"),
            ("claude/projects/p/s.jsonl", "x"),
            ("claude/not-here-yet.md", "x"),
        ],
        &[],
    );
    std::env::set_var(HOME_OVERRIDE, &target);
    let taken = read_zip(snapshot(&incoming, "now".into(), "host".into()).unwrap()).unwrap();
    let taken_manifest = read_manifest(&taken).unwrap();
    let held: Vec<&str> = taken_manifest
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(
        held,
        vec!["claude/.credentials.json", "claude/projects/p/s.jsonl"]
    );
    assert!(taken_manifest.includes_credentials);
    assert_eq!(taken[1].data, b"{\"token\":1}");

    // --- a backup of an invalid settings.json fails rather than writing an unrestorable archive
    write(&source.join(".claude/settings.json"), r#"{"a":1,}"#);
    std::env::set_var(HOME_OVERRIDE, &source);
    let error = backup(&BackupOptions::default(), "now".into(), "host".into())
        .err()
        .expect("an invalid settings.json was backed up")
        .to_string();
    assert!(error.contains("settings.json"), "{error}");

    fs::remove_dir_all(&root).unwrap();
}
