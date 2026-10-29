use std::io::{self, BufRead, Write};

use claude_code_sync::app::{
    backup, claude_dir, home_dir, printable, read_include_list, read_manifest, remove_created,
    restore, settings_conflicts, snapshot, write_new, RestoreOptions, FAILED, SKIPPED,
};
use claude_code_sync::archive::{read_zip, Result};
use claude_code_sync::classify::BackupOptions;
use claude_code_sync::merge::{Choice, Conflict, MergeStrategy};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
claude-code-sync: portable backup and restore for a Claude Code setup

  claude-code-sync backup  [--with-memory] [--include-credentials] [--out DIR]
  claude-code-sync restore <archive.zip> [--merge=STRATEGY] [--dry-run]
  claude-code-sync rollback [pre-restore-STAMP.zip] [--dry-run]
  claude-code-sync --version

Merge strategies, applied to settings.json only:
  incoming   deep merge, the archive wins a conflict          (default)
  existing   deep merge, this machine wins a conflict
  replace    discard this machine's settings entirely
  ask        prompt for each conflicting key

Always carried:  CLAUDE.md, settings.json, settings.local.json, keybindings.json,
                 agents/, commands/, hooks/, skills/, output-styles/, tools/,
                 rules/, statuslines/, themes/,
                 plugin version pins, root-level feature-marker dotfiles,
                 and the whole ~/.agents skill source tree.
Never carried:   transcripts, plugin checkouts, caches, telemetry, session state.

Anything else you want carried goes in ~/.claude/.claude-sync-include, one archive-relative
path per line, a trailing / for a whole subtree. Example:  claude/history.jsonl";

struct Args {
    command: Option<String>,
    positional: Vec<String>,
    flags: Vec<String>,
    values: Vec<(String, String)>,
}

impl Args {
    /// Accepts both `--out=DIR` and `--out DIR`.
    fn parse(raw: Vec<String>) -> Self {
        const VALUE_FLAGS: &[&str] = &["--out", "--merge"];
        let mut parsed = Args {
            command: None,
            positional: Vec::new(),
            flags: Vec::new(),
            values: Vec::new(),
        };
        let mut index = 0;

        while index < raw.len() {
            let argument = &raw[index];
            // Everything after `--` is positional, so an archive named `-x.zip` can be restored.
            if argument == "--" {
                let rest = raw[index + 1..].iter().cloned();
                if parsed.command.is_none() {
                    let mut rest = rest;
                    parsed.command = rest.next();
                    parsed.positional.extend(rest);
                } else {
                    parsed.positional.extend(rest);
                }
                break;
            }
            if let Some((name, value)) = argument.split_once('=') {
                if name.starts_with("--") {
                    parsed.values.push((name.to_string(), value.to_string()));
                    index += 1;
                    continue;
                }
            }
            if VALUE_FLAGS.contains(&argument.as_str()) {
                // A dangling `--merge` records an empty value so the command rejects it, rather
                // than falling through to a flag and silently applying the default strategy.
                // So does one followed by another option, which is left to be parsed as itself.
                match raw.get(index + 1) {
                    Some(value) if !value.starts_with('-') => {
                        parsed.values.push((argument.clone(), value.clone()));
                        index += 2;
                    }
                    _ => {
                        parsed.values.push((argument.clone(), String::new()));
                        index += 1;
                    }
                }
                continue;
            }
            if argument.starts_with('-') {
                parsed.flags.push(argument.clone());
            } else if parsed.command.is_none() {
                parsed.command = Some(argument.clone());
            } else {
                parsed.positional.push(argument.clone());
            }
            index += 1;
        }

        parsed
    }

    /// CRITICAL: an option this command does not know is refused, never ignored. A mistyped
    /// `--dryrun`, or `--dry-run=true`, which parses as a value, would otherwise run a real
    /// restore while the user believes nothing is being written.
    fn check(&self, flags: &[&str], values: &[&str], positionals: usize) -> Result<()> {
        for (name, value) in &self.values {
            if flags.contains(&name.as_str()) {
                return Err(format!("{name} takes no value").into());
            }
            if !values.contains(&name.as_str()) {
                return Err(format!("unknown option: {name}").into());
            }
            if value.is_empty() {
                return Err(format!("{name} needs a value").into());
            }
        }
        if let Some(flag) = self.flags.iter().find(|f| !flags.contains(&f.as_str())) {
            return Err(format!("unknown option: {flag}").into());
        }
        if self.positional.len() > positionals {
            return Err(format!("unexpected argument: {}", self.positional[positionals]).into());
        }
        Ok(())
    }

    fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// CRITICAL: hand-formatted on purpose. chrono's strftime is a runtime interpreter, so handing
/// it one format string that never changes links about 9 KB of parsing machinery into the binary.
fn stamp(now: &chrono::DateTime<chrono::Local>) -> String {
    use chrono::{Datelike, Timelike};
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

/// CRITICAL: integer arithmetic on purpose. `{:.1}` would be the only float formatting in the
/// tool, and it links both of core's float-to-decimal paths, about 9 KB, for one cosmetic line.
fn kilobytes(bytes: usize) -> String {
    let tenths = (bytes as u64 * 10 + 512) / 1024;
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// Only an explicit answer decides a conflict. Enter re-asks, and end of input stops the restore
/// before anything is written rather than quietly taking the archive's side.
fn ask_for(conflicts: &[Conflict]) -> Result<Vec<(String, Choice)>> {
    let stdin = io::stdin();
    let mut answers = Vec::new();

    for conflict in conflicts {
        println!("\n  {}", printable(&conflict.path));
        println!(
            "    this machine: {}",
            printable(&conflict.existing.to_string())
        );
        println!(
            "    archive:      {}",
            printable(&conflict.incoming.to_string())
        );
        let choice = loop {
            print!("    keep [l]ocal or take [a]rchive? ");
            let _ = io::stdout().flush();

            let mut reply = String::new();
            if stdin.lock().read_line(&mut reply)? == 0 {
                return Err("no answer given; nothing was restored".into());
            }
            match reply.trim().to_lowercase().chars().next() {
                Some('l') => break Choice::Existing,
                Some('a') => break Choice::Incoming,
                _ => {}
            }
        };
        answers.push((conflict.path.clone(), choice));
    }

    Ok(answers)
}

/// Write `<stem>.zip`, or `<stem>-2.zip` and so on when a run in the same second got there first.
/// Never over an existing file.
fn write_unique(
    directory: &std::path::Path,
    stem: &str,
    data: &[u8],
    private: bool,
) -> Result<std::path::PathBuf> {
    for attempt in 1.. {
        let name = if attempt == 1 {
            format!("{stem}.zip")
        } else {
            format!("{stem}-{attempt}.zip")
        };
        let out = directory.join(name);
        match write_new(&out, data, private) {
            Ok(()) => return Ok(out),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempt < 100 => {}
            Err(error) => return Err(format!("{}: {error}", out.display()).into()),
        }
    }
    unreachable!("the loop returns")
}

/// `pre-restore-<stamp>.zip` and then `pre-restore-<stamp>-2.zip`, in the order they were written.
/// Plain name order puts the `-2` first, because `-` sorts before `.`.
fn snapshot_order(name: &str) -> (String, u32) {
    let stem = name
        .trim_start_matches("pre-restore-")
        .trim_end_matches(".zip");
    // The stamp is `YYYYMMDD-HHMMSS`; anything after a further `-` is the attempt.
    match stem.get(15..).and_then(|rest| rest.strip_prefix('-')) {
        Some(attempt) => (stem[..15].to_string(), attempt.parse().unwrap_or(0)),
        None => (stem.to_string(), 1),
    }
}

/// The snapshot to roll back to: the one named, which must be a pre-restore snapshot in
/// `~/.claude/backups`, or else the newest there. The stamp in the name sorts by time.
fn pick_snapshot(
    directory: &std::path::Path,
    named: Option<&String>,
) -> Result<std::path::PathBuf> {
    let is_snapshot = |name: &str| name.starts_with("pre-restore-") && name.ends_with(".zip");
    let Some(named) = named else {
        let newest = std::fs::read_dir(directory)
            .map_err(|e| format!("{}: {e}", directory.display()))?
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| is_snapshot(name))
            .max_by_key(|name| snapshot_order(name))
            .ok_or_else(|| format!("no pre-restore snapshot in {}", directory.display()))?;
        return Ok(directory.join(newest));
    };
    let given = std::path::Path::new(named);
    let name = given
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| is_snapshot(name))
        .ok_or_else(|| format!("{named} is not a pre-restore-*.zip snapshot"))?;
    let path = directory.join(name);
    let same_place = match given.parent().filter(|p| !p.as_os_str().is_empty()) {
        None => true,
        Some(parent) => std::fs::canonicalize(parent).ok() == std::fs::canonicalize(directory).ok(),
    };
    if !same_place {
        return Err(format!("{named} is not in {}", directory.display()).into());
    }
    Ok(path)
}

/// Vet, snapshot and apply an archive, then report. A rollback also removes what the restore it
/// undoes created, and its own snapshot covers those files so it can be undone in turn.
fn apply(
    entries: &[claude_code_sync::archive::Entry],
    manifest: &claude_code_sync::archive::Manifest,
    strategy: MergeStrategy,
    dry_run: bool,
    rollback: bool,
    now: &chrono::DateTime<chrono::Local>,
    host: &str,
) -> Result<i32> {
    let mut answers: Vec<(String, Choice)> = Vec::new();
    if strategy == MergeStrategy::Ask {
        answers = ask_for(&settings_conflicts(entries)?)?;
    }
    let mut resolve = |conflict: &Conflict| {
        answers
            .iter()
            .find(|(path, _)| path == &conflict.path)
            .map(|(_, choice)| *choice)
            .unwrap_or(Choice::Incoming)
    };

    // A dry run first, always: it vets the whole archive, so one that is refused leaves no
    // snapshot behind and writes nothing at all.
    let mut actions = restore(
        entries,
        manifest,
        &mut RestoreOptions {
            strategy,
            dry_run: true,
            resolve: &mut resolve,
        },
    )?;
    if rollback {
        actions.extend(remove_created(manifest, true)?);
    }

    if !dry_run {
        let directory = claude_dir(&home_dir()?).join("backups");
        // The snapshot must cover everything this restore could overwrite, which is decided by
        // the archive, not by this machine's include list.
        let removing: &[String] = if rollback { &manifest.created } else { &[] };
        let snapshot = snapshot(manifest, removing, now.to_rfc3339(), host.to_string())?;
        std::fs::create_dir_all(&directory)?;
        let out = write_unique(
            &directory,
            &format!("pre-restore-{}", stamp(now)),
            &snapshot,
            true,
        )?;
        println!("snapshot of current config -> {}\n", out.display());

        actions = restore(
            entries,
            manifest,
            &mut RestoreOptions {
                strategy,
                dry_run: false,
                resolve: &mut resolve,
            },
        )?;
        if rollback {
            actions.extend(remove_created(manifest, false)?);
        }
    }

    for action in &actions {
        println!("  {}", printable(action));
    }
    let failed = actions.iter().filter(|a| a.starts_with(FAILED)).count();
    let skipped = actions.iter().filter(|a| a.starts_with(SKIPPED)).count();
    println!(
        "\n{} {}{}",
        actions.len() - failed - skipped,
        if dry_run {
            "planned (dry run, nothing written)"
        } else {
            "applied"
        },
        if skipped > 0 {
            format!(", {skipped} skipped")
        } else {
            String::new()
        }
    );
    if failed > 0 {
        eprintln!("{failed} link(s) could not be restored");
        return Ok(1);
    }
    Ok(0)
}

fn run() -> Result<i32> {
    let args = Args::parse(std::env::args().skip(1).collect());

    if args.has("--version") || args.has("-V") || args.command.as_deref() == Some("version") {
        println!("claude-code-sync {VERSION}");
        return Ok(0);
    }
    if args.has("--help") || args.has("-h") || args.command.as_deref() == Some("help") {
        println!("{USAGE}");
        return Ok(0);
    }

    let now = chrono::Local::now();
    let host = gethostname::gethostname().to_string_lossy().to_string();

    match args.command.as_deref() {
        Some("backup") => {
            args.check(&["--with-memory", "--include-credentials"], &["--out"], 0)?;
            let claude = claude_dir(&home_dir()?);
            if !claude.is_dir() {
                return Err(
                    format!("{} does not exist; nothing to back up", claude.display()).into(),
                );
            }
            let options = BackupOptions {
                with_memory: args.has("--with-memory"),
                include_credentials: args.has("--include-credentials"),
                extras: read_include_list(),
            };
            if !options.extras.is_empty() {
                println!(
                    "{} extra path(s) from .claude-sync-include",
                    options.extras.len()
                );
            }
            if options.include_credentials {
                eprintln!(
                    "WARNING: this archive will contain .credentials.json. Do not share it.\n"
                );
            }

            let (archive, manifest) = backup(&options, now.to_rfc3339(), host.clone())?;

            let directory = args
                .value("--out")
                .map(std::path::PathBuf::from)
                .unwrap_or(std::env::current_dir()?);
            std::fs::create_dir_all(&directory)
                .map_err(|e| format!("{}: {e}", directory.display()))?;
            let out = write_unique(
                &directory,
                &format!("claude-sync-{host}-{}", stamp(&now)),
                &archive,
                manifest.includes_credentials,
            )?;

            println!(
                "{} files, {} links",
                manifest.files.len(),
                manifest.links.len()
            );
            println!("{} KB -> {}", kilobytes(archive.len()), out.display());
            Ok(0)
        }

        Some("restore") => {
            args.check(&["--dry-run"], &["--merge"], 1)?;
            let Some(path) = args.positional.first() else {
                eprintln!("restore needs an archive path\n\n{USAGE}");
                return Ok(1);
            };
            let requested = args.value("--merge").unwrap_or("incoming");
            let Some(strategy) = MergeStrategy::parse(requested) else {
                eprintln!("unknown merge strategy: {requested}");
                return Ok(1);
            };
            let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
            let entries = read_zip(bytes).map_err(|e| format!("{path}: {e}"))?;
            let manifest = read_manifest(&entries)?;
            apply(
                &entries,
                &manifest,
                strategy,
                args.has("--dry-run"),
                false,
                &now,
                &host,
            )
        }

        Some("rollback") => {
            args.check(&["--dry-run"], &[], 1)?;
            let directory = claude_dir(&home_dir()?).join("backups");
            let path = pick_snapshot(&directory, args.positional.first())?;
            let shown = path.display().to_string();
            let bytes = std::fs::read(&path).map_err(|e| format!("{shown}: {e}"))?;
            let entries = read_zip(bytes).map_err(|e| format!("{shown}: {e}"))?;
            let manifest = read_manifest(&entries)?;
            // A rollback deletes what the restore created, so it trusts only a snapshot this
            // machine wrote, never an archive from somewhere else renamed to look like one.
            if manifest.source_host != host {
                return Err(format!(
                    "{shown} was taken on {}, not this machine; refusing to roll back with it",
                    manifest.source_host
                )
                .into());
            }
            println!("rolling back to {shown}\n");
            apply(
                &entries,
                &manifest,
                MergeStrategy::Replace,
                args.has("--dry-run"),
                true,
                &now,
                &host,
            )
        }

        Some(other) => {
            eprintln!("unknown command: {other}\n\n{USAGE}");
            Ok(1)
        }
        None => {
            println!("{USAGE}");
            Ok(0)
        }
    }
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("error: {}", printable(&error.to_string()));
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Both helpers exist only to keep a formatting path out of the binary, so what they have to
    // prove is that they still print what the thing they replaced printed.

    #[test]
    fn kilobytes_matches_one_decimal_place() {
        assert_eq!(kilobytes(0), "0.0");
        assert_eq!(kilobytes(1024), "1.0");
        assert_eq!(kilobytes(1536), "1.5");
        assert_eq!(kilobytes(144588), "141.2");
        assert_eq!(kilobytes(100), "0.1");
    }

    #[test]
    fn a_stamp_is_the_sortable_form_the_filename_expects() {
        use chrono::TimeZone;
        let moment = chrono::Local
            .with_ymd_and_hms(2026, 8, 19, 4, 32, 3)
            .unwrap();
        assert_eq!(stamp(&moment), "20260819-043203");
    }

    #[test]
    fn a_value_flag_with_nothing_after_it_is_rejected_rather_than_defaulted() {
        let args = Args::parse(vec!["restore".into(), "a.zip".into(), "--merge".into()]);
        assert_eq!(args.value("--merge"), Some(""));
        assert_eq!(MergeStrategy::parse(args.value("--merge").unwrap()), None);
    }

    fn restore_args(raw: &[&str]) -> Result<()> {
        let mut all = vec!["restore".to_string(), "a.zip".to_string()];
        all.extend(raw.iter().map(|s| s.to_string()));
        Args::parse(all).check(&["--dry-run"], &["--merge"], 1)
    }

    // Each of these used to run a real restore while the user asked for a dry run.
    #[test]
    fn a_misspelled_or_valued_flag_is_refused_rather_than_ignored() {
        for raw in [
            &["--dry-run=true"][..],
            &["--dryrun"],
            &["-n"],
            &["--merge=incoming", "--bogus=1"],
            &["--merge", "--dry-run"],
            &["b.zip"],
        ] {
            assert!(restore_args(raw).is_err(), "{raw:?}");
        }
        assert!(restore_args(&["--dry-run", "--merge=existing"]).is_ok());
        assert!(restore_args(&["--merge", "ask"]).is_ok());
        assert!(
            Args::parse(vec!["backup".into(), "--out=-snapshots".into()])
                .check(&[], &["--out"], 0)
                .is_ok()
        );
        let dashed = Args::parse(vec!["restore".into(), "--".into(), "-x.zip".into()]);
        assert_eq!(dashed.positional, vec!["-x.zip".to_string()]);
    }

    #[test]
    fn control_characters_in_archive_text_are_escaped() {
        assert_eq!(
            printable("create  claude/a\x1b[2Kb"),
            "create  claude/a\\u{1b}[2Kb"
        );
        assert_eq!(
            printable("link    claude/x -> /home/dev"),
            "link    claude/x -> /home/dev"
        );
    }

    #[test]
    fn snapshots_are_ordered_as_they_were_written() {
        let mut names = vec![
            "pre-restore-20261029-101500-2.zip",
            "pre-restore-20261029-101500.zip",
            "pre-restore-20261028-235959.zip",
            "pre-restore-20261029-101500-10.zip",
        ];
        names.sort_by_key(|name| snapshot_order(name));
        assert_eq!(
            names,
            vec![
                "pre-restore-20261028-235959.zip",
                "pre-restore-20261029-101500.zip",
                "pre-restore-20261029-101500-2.zip",
                "pre-restore-20261029-101500-10.zip",
            ]
        );
    }

    #[test]
    fn both_flag_spellings_reach_the_same_value() {
        for raw in [
            vec!["backup".into(), "--out".into(), "dir".into()],
            vec!["backup".to_string(), "--out=dir".into()],
        ] {
            let args = Args::parse(raw);
            assert_eq!(args.command.as_deref(), Some("backup"));
            assert_eq!(args.value("--out"), Some("dir"));
        }
    }
}
