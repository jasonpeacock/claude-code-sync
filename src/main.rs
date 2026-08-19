use std::io::{self, BufRead, Write};

use claude_code_sync::app::{
    backup, home_dir, read_include_list, read_manifest, restore, settings_conflicts, RestoreOptions,
};
use claude_code_sync::archive::{read_zip, Result};
use claude_code_sync::classify::BackupOptions;
use claude_code_sync::merge::{Choice, Conflict, MergeStrategy};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
claude-code-sync: portable backup and restore for a Claude Code setup

  claude-code-sync backup  [--with-memory] [--include-credentials] [--out DIR]
  claude-code-sync restore <archive.zip> [--merge=STRATEGY] [--dry-run]
  claude-code-sync --version

Merge strategies, applied to settings.json only:
  incoming   deep merge, the archive wins a conflict          (default)
  existing   deep merge, this machine wins a conflict
  replace    discard this machine's settings entirely
  ask        prompt for each conflicting key

Always carried:  CLAUDE.md, settings.json, settings.local.json, keybindings.json,
                 agents/, commands/, hooks/, skills/, output-styles/, tools/,
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
                let value = raw.get(index + 1).cloned().unwrap_or_default();
                parsed.values.push((argument.clone(), value));
                index += 2;
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

/// `%Y%m%d-%H%M%S`, written out rather than handed to `strftime`.
///
/// chrono's strftime parser is a runtime interpreter, so asking it for one format string that
/// never changes links about 9 KB of parsing machinery into the binary. Local time is unchanged.
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

/// Size in KB to one decimal, in integer arithmetic.
///
/// `{:.1}` on an f64 is the only float formatting in the whole tool and it costs about 9 KB,
/// because it links both of core's float-to-decimal paths for one cosmetic line.
fn kilobytes(bytes: usize) -> String {
    let tenths = (bytes as u64 * 10 + 512) / 1024;
    format!("{}.{}", tenths / 10, tenths % 10)
}

fn ask_for(conflicts: &[Conflict]) -> Vec<(String, Choice)> {
    let stdin = io::stdin();
    let mut answers = Vec::new();

    for conflict in conflicts {
        println!("\n  {}", conflict.path);
        println!("    this machine: {}", conflict.existing);
        println!("    archive:      {}", conflict.incoming);
        print!("    keep [l]ocal or take [a]rchive? ");
        let _ = io::stdout().flush();

        let mut reply = String::new();
        let choice = match stdin.lock().read_line(&mut reply) {
            Ok(_) if reply.trim().to_lowercase().starts_with('l') => Choice::Existing,
            _ => Choice::Incoming,
        };
        answers.push((conflict.path.clone(), choice));
    }

    answers
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
            let out = directory.join(format!("claude-sync-{host}-{}.zip", stamp(&now)));
            std::fs::write(&out, &archive).map_err(|e| format!("{}: {e}", out.display()))?;

            println!(
                "{} files, {} links",
                manifest.files.len(),
                manifest.links.len()
            );
            println!("{} KB -> {}", kilobytes(archive.len()), out.display());
            Ok(0)
        }

        Some("restore") => {
            let Some(path) = args.positional.first() else {
                eprintln!("restore needs an archive path\n\n{USAGE}");
                return Ok(1);
            };

            let requested = args.value("--merge").unwrap_or("incoming");
            let Some(strategy) = MergeStrategy::parse(requested) else {
                eprintln!("unknown merge strategy: {requested}");
                return Ok(1);
            };

            let dry_run = args.has("--dry-run");
            let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
            let entries = read_zip(bytes).map_err(|e| format!("{path}: {e}"))?;
            let manifest = read_manifest(&entries)?;

            let mut answers: Vec<(String, Choice)> = Vec::new();
            if strategy == MergeStrategy::Ask {
                answers = ask_for(&settings_conflicts(&entries)?);
            }

            if !dry_run {
                let directory = home_dir()?.join(".claude").join("backups");
                // The snapshot must cover everything a restore could overwrite, extras included.
                let (snapshot, _) = backup(
                    &BackupOptions {
                        with_memory: true,
                        include_credentials: false,
                        extras: read_include_list(),
                    },
                    now.to_rfc3339(),
                    host.clone(),
                )?;
                std::fs::create_dir_all(&directory)?;
                let out = directory.join(format!("pre-restore-{}.zip", stamp(&now)));
                std::fs::write(&out, snapshot)?;
                println!("snapshot of current config -> {}\n", out.display());
            }

            let mut resolve = |conflict: &Conflict| {
                answers
                    .iter()
                    .find(|(path, _)| path == &conflict.path)
                    .map(|(_, choice)| *choice)
                    .unwrap_or(Choice::Incoming)
            };
            let mut options = RestoreOptions {
                strategy,
                dry_run,
                resolve: &mut resolve,
            };
            let actions = restore(&entries, &manifest, &mut options)?;

            for action in &actions {
                println!("  {action}");
            }
            println!(
                "\n{} {}",
                actions.len(),
                if dry_run {
                    "planned (dry run, nothing written)"
                } else {
                    "applied"
                }
            );
            Ok(0)
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
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Both helpers exist to keep a formatting path out of the binary, so what they must prove is
    // that they still print what the thing they replaced printed.

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
