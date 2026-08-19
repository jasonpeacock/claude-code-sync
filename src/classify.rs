//! Deciding what goes into an archive.
//!
//! An allowlist, not a denylist: anything unrecognised is excluded, so a cache directory added
//! in a future Claude Code release costs nothing. The cost of that choice is that genuinely
//! authored files can be missed, which is what `extras` and the dotfile rule below exist for.

use crate::paths::forward_slashes;

#[derive(Default)]
pub struct BackupOptions {
    /// Carry `projects/<project>/memory/`, which is distilled by hand and never regenerates.
    pub with_memory: bool,
    /// Carry `.credentials.json`. Off unless asked for: the archive is meant to travel.
    pub include_credentials: bool,
    /// Extra archive-relative paths from `~/.claude/.claude-sync-include`, one per line.
    ///
    /// The allowlist cannot know about every plugin's marker file or a directory that does not
    /// exist yet, so this is the escape hatch. A trailing `/` matches a whole subtree.
    pub extras: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    Include,
    Exclude,
    /// Refused, and worth telling the user about by name.
    Secret,
}

/// Directories under `~/.claude` whose contents are authored, not generated.
const PORTABLE_DIRS: &[&str] = &[
    "agents",
    "commands",
    "hooks",
    "skills",
    "tools",
    "output-styles",
];

/// Individual files at the root of `~/.claude` worth carrying.
const PORTABLE_FILES: &[&str] = &[
    "CLAUDE.md",
    "settings.json",
    "settings.local.json",
    "keybindings.json",
];

/// Plugin state worth carrying. `installed_plugins.json` pins exact versions and commit SHAs,
/// which `settings.json` does not record, so without it a restore drifts to whatever is latest.
const PLUGIN_FILES: &[&str] = &[
    "installed_plugins.json",
    "known_marketplaces.json",
    "blocklist.json",
];

/// Root-level dotfiles that are tool state rather than something the user chose.
const STATE_DOTFILES: &[&str] = &[
    ".last-cleanup",
    ".last-update-result.json",
    ".DS_Store",
    ".gitignore",
];

const CREDENTIALS: &str = "claude/.credentials.json";

/// The file that supplies [`BackupOptions::extras`], carried so the list travels with the archive.
pub const INCLUDE_LIST: &str = ".claude-sync-include";

/// Is this `projects/<something>/memory/...`?
fn is_memory_path(rest: &str) -> bool {
    let mut parts = rest.split('/');
    parts.next() == Some("projects")
        && parts.next().is_some_and(|p| !p.is_empty())
        && parts.next() == Some("memory")
        && parts.next().is_some()
}

/// A user-listed extra matches a path outright, or a whole subtree when it ends in `/`.
fn matches_extra(path: &str, extras: &[String]) -> bool {
    extras.iter().any(|extra| {
        let extra = extra.trim();
        if extra.is_empty() {
            false
        } else if let Some(prefix) = extra.strip_suffix('/') {
            path.starts_with(&format!("{prefix}/"))
        } else {
            path == extra
        }
    })
}

/// Does a user-listed extra name something at, above, or below this directory?
fn extra_reaches_into(dir: &str, extra: &str) -> bool {
    let extra = extra.trim().trim_end_matches('/');
    !extra.is_empty()
        && (extra == dir
            || extra.starts_with(&format!("{dir}/"))
            || dir.starts_with(&format!("{extra}/")))
}

/// Decide what happens to one archive-relative path.
pub fn classify_entry(rel_path: &str, options: &BackupOptions) -> Classification {
    let path = forward_slashes(rel_path);

    // Checked before anything else so no extras entry can smuggle the token out.
    if path == CREDENTIALS {
        return if options.include_credentials {
            Classification::Include
        } else {
            Classification::Secret
        };
    }

    // Archives this tool writes must never be swept into the next one.
    if path.ends_with(".zip") {
        return Classification::Exclude;
    }

    if matches_extra(&path, &options.extras) {
        return Classification::Include;
    }

    // The whole ~/.agents tree is authored skill sources plus their lock file.
    if path.starts_with("agents/") {
        return Classification::Include;
    }

    let Some(rest) = path.strip_prefix("claude/") else {
        return Classification::Exclude;
    };

    if is_memory_path(rest) {
        return if options.with_memory {
            Classification::Include
        } else {
            Classification::Exclude
        };
    }

    match rest.split_once('/') {
        // Everything under plugins/ is a checkout or a cache except a few small state files.
        Some(("plugins", tail)) => {
            if PLUGIN_FILES.contains(&tail) {
                Classification::Include
            } else {
                Classification::Exclude
            }
        }
        Some((head, _)) if PORTABLE_DIRS.contains(&head) => Classification::Include,
        Some(_) => Classification::Exclude,
        None if PORTABLE_FILES.contains(&rest) => Classification::Include,
        // Root dotfiles are feature markers set by plugins, e.g. a flag that keeps a mode on.
        // They are tiny, unpredictable, and losing one silently changes behavior after a restore.
        None if rest.starts_with('.') && !STATE_DOTFILES.contains(&rest) => Classification::Include,
        None => Classification::Exclude,
    }
}

/// Can anything under this directory be included? A `false` prunes the whole subtree unread.
///
/// CRITICAL: this must stay in step with [`classify_entry`]. A directory refused here is never
/// opened, so a rule that includes a file whose parent is pruned drops that file silently, with
/// no error anywhere. `assert_included` in the tests below guards the pairing.
pub fn should_descend(dir_path: &str, options: &BackupOptions) -> bool {
    let path = forward_slashes(dir_path);

    if options
        .extras
        .iter()
        .any(|extra| extra_reaches_into(&path, extra))
    {
        return true;
    }
    if path == "agents" || path.starts_with("agents/") {
        return true;
    }

    let Some(rest) = path.strip_prefix("claude/") else {
        return path == "claude";
    };

    let mut parts = rest.split('/');
    match parts.next() {
        // Every PLUGIN_FILES entry sits directly under plugins/; the rest is checkouts and caches.
        Some("plugins") => parts.next().is_none(),
        Some("projects") => {
            let _project = parts.next();
            options.with_memory && matches!(parts.next(), None | Some("memory"))
        }
        Some(head) => PORTABLE_DIRS.contains(&head),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> BackupOptions {
        BackupOptions::default()
    }
    fn with_memory() -> BackupOptions {
        BackupOptions {
            with_memory: true,
            ..Default::default()
        }
    }
    fn extras(list: &[&str]) -> BackupOptions {
        BackupOptions {
            extras: list.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    /// An included file is only ever archived if every directory above it is walked into, so
    /// each Include assertion also asserts its whole parent chain is reachable.
    fn assert_included(path: &str, options: &BackupOptions) {
        assert_eq!(
            classify_entry(path, options),
            Classification::Include,
            "{path}"
        );
        let parts: Vec<&str> = path.split('/').collect();
        for depth in 1..parts.len() {
            let dir = parts[..depth].join("/");
            assert!(should_descend(&dir, options), "{dir} prunes {path}");
        }
    }

    fn assert_excluded(path: &str, options: &BackupOptions) {
        assert_eq!(
            classify_entry(path, options),
            Classification::Exclude,
            "{path}"
        );
    }

    #[test]
    fn hand_written_config_is_included() {
        for path in [
            "claude/CLAUDE.md",
            "claude/settings.json",
            "claude/settings.local.json",
            "claude/keybindings.json",
            "claude/agents/skeptic.md",
            "claude/commands/lean.md",
            "claude/hooks/load-repo-rules.mjs",
            "claude/skills/lean-orchestration/SKILL.md",
            "claude/output-styles/terse.md",
            "claude/tools/helper.ts",
        ] {
            assert_included(path, &defaults());
        }
    }

    #[test]
    fn the_agents_skill_source_tree_is_included() {
        assert_included("agents/skills/grilling/SKILL.md", &defaults());
        assert_included("agents/.skill-lock.json", &defaults());
    }

    #[test]
    fn plugin_version_pins_are_carried_but_the_checkouts_and_caches_are_not() {
        for path in [
            "claude/plugins/installed_plugins.json",
            "claude/plugins/known_marketplaces.json",
            "claude/plugins/blocklist.json",
        ] {
            assert_included(path, &defaults());
        }
        for path in [
            "claude/plugins/plugin-catalog-cache.json",
            "claude/plugins/cache/context-mode/1.0.169/SKILL.md",
            "claude/plugins/marketplaces/official/README.md",
            "claude/plugins/repos/official/plugin.json",
        ] {
            assert_excluded(path, &defaults());
        }
    }

    #[test]
    fn a_plugin_feature_marker_dotfile_is_carried() {
        // Losing one of these silently changes behavior after a restore.
        assert_included("claude/.i-have-adhd-always", &defaults());
        assert_included("claude/.claude-sync-include", &defaults());
    }

    #[test]
    fn tool_state_dotfiles_are_not_carried() {
        for path in ["claude/.last-cleanup", "claude/.last-update-result.json"] {
            assert_excluded(path, &defaults());
        }
    }

    #[test]
    fn bulk_generated_state_is_excluded() {
        for path in [
            "claude/projects/c--foo/session-abc.jsonl",
            "claude/security/db.sqlite",
            "claude/context-mode/index.db",
            "claude/file-history/x.json",
            "claude/shell-snapshots/snap-1.sh",
            "claude/sessions/s.json",
            "claude/cache/blob",
            "claude/debug/log.txt",
            "claude/telemetry/events.jsonl",
            "claude/history.jsonl",
            "claude/stats-cache.json",
        ] {
            assert_excluded(path, &defaults());
        }
    }

    #[test]
    fn credentials_are_refused_by_default_and_flagged_rather_than_merely_skipped() {
        assert_eq!(
            classify_entry("claude/.credentials.json", &defaults()),
            Classification::Secret
        );
    }

    #[test]
    fn credentials_are_included_only_behind_the_explicit_opt_in() {
        let options = BackupOptions {
            include_credentials: true,
            ..Default::default()
        };
        assert_eq!(
            classify_entry("claude/.credentials.json", &options),
            Classification::Include
        );
    }

    #[test]
    fn an_extras_entry_cannot_smuggle_out_the_credentials_file() {
        assert_eq!(
            classify_entry(
                "claude/.credentials.json",
                &extras(&["claude/.credentials.json"])
            ),
            Classification::Secret
        );
    }

    #[test]
    fn an_extras_entry_carries_a_file_the_allowlist_would_have_dropped() {
        let options = extras(&["claude/history.jsonl"]);
        assert_included("claude/history.jsonl", &options);
        assert_excluded("claude/stats-cache.json", &options);
    }

    #[test]
    fn an_extras_entry_ending_in_a_slash_carries_the_whole_subtree() {
        let options = extras(&["claude/context-mode/"]);
        assert_included("claude/context-mode/index.db", &options);
        assert_included("claude/context-mode/nested/deep/blob", &options);
        assert_excluded("claude/context-mode-other/x", &options);
        assert!(!should_descend("claude/context-mode-other", &options));
    }

    #[test]
    fn memory_is_excluded_by_default() {
        assert_excluded("claude/projects/c--foo/memory/MEMORY.md", &defaults());
        assert!(!should_descend("claude/projects", &defaults()));
    }

    #[test]
    fn memory_is_included_with_the_flag_while_transcripts_stay_out() {
        assert_included("claude/projects/c--foo/memory/MEMORY.md", &with_memory());
        assert_included(
            "claude/projects/c--foo/memory/nested/note.md",
            &with_memory(),
        );
        assert_excluded("claude/projects/c--foo/session.jsonl", &with_memory());
        assert!(!should_descend(
            "claude/projects/c--foo/todos",
            &with_memory()
        ));
    }

    #[test]
    fn a_memory_directory_outside_projects_is_not_mistaken_for_a_memory_store() {
        assert_included("claude/skills/memory/SKILL.md", &with_memory());
        assert_excluded("claude/cache/memory/blob", &with_memory());
    }

    #[test]
    fn an_archive_is_not_swept_into_the_next_backup() {
        assert_excluded("claude/tools/claude-sync-host-20260819.zip", &defaults());
        assert_excluded("claude/backups/pre-restore-20260819.zip", &defaults());
    }

    #[test]
    fn an_unknown_top_level_file_is_excluded_rather_than_guessed() {
        assert_excluded("claude/some-future-cache.bin", &defaults());
    }

    #[test]
    fn classification_does_not_depend_on_path_separator_style() {
        assert_eq!(
            classify_entry(r"claude\hooks\load-repo-rules.mjs", &defaults()),
            Classification::Include
        );
        assert!(should_descend(r"claude\hooks", &defaults()));
    }

    // The pruning half of the walk. Nothing below these can ever be carried, and opening them
    // anyway is what made a backup stat 15k entries to collect 68 files.
    #[test]
    fn generated_state_directories_are_never_opened() {
        for dir in [
            "claude/plugins/cache",
            "claude/plugins/repos",
            "claude/plugins/marketplaces",
            "claude/projects",
            "claude/shell-snapshots",
            "claude/file-history",
            "claude/todos",
            "claude/statsig",
            "claude/backups",
            "downloads",
        ] {
            assert!(!should_descend(dir, &defaults()), "{dir}");
        }
    }
}
