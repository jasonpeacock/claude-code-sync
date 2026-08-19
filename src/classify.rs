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
            assert_eq!(
                classify_entry(path, &defaults()),
                Classification::Include,
                "{path}"
            );
        }
    }

    #[test]
    fn the_agents_skill_source_tree_is_included() {
        assert_eq!(
            classify_entry("agents/skills/grilling/SKILL.md", &defaults()),
            Classification::Include
        );
        assert_eq!(
            classify_entry("agents/.skill-lock.json", &defaults()),
            Classification::Include
        );
    }

    #[test]
    fn plugin_version_pins_are_carried_but_the_checkouts_and_caches_are_not() {
        for path in [
            "claude/plugins/installed_plugins.json",
            "claude/plugins/known_marketplaces.json",
            "claude/plugins/blocklist.json",
        ] {
            assert_eq!(
                classify_entry(path, &defaults()),
                Classification::Include,
                "{path}"
            );
        }
        for path in [
            "claude/plugins/plugin-catalog-cache.json",
            "claude/plugins/cache/context-mode/1.0.169/SKILL.md",
            "claude/plugins/marketplaces/official/README.md",
            "claude/plugins/repos/official/plugin.json",
        ] {
            assert_eq!(
                classify_entry(path, &defaults()),
                Classification::Exclude,
                "{path}"
            );
        }
    }

    #[test]
    fn a_plugin_feature_marker_dotfile_is_carried() {
        // Losing one of these silently changes behavior after a restore.
        assert_eq!(
            classify_entry("claude/.i-have-adhd-always", &defaults()),
            Classification::Include
        );
        assert_eq!(
            classify_entry("claude/.claude-sync-include", &defaults()),
            Classification::Include
        );
    }

    #[test]
    fn tool_state_dotfiles_are_not_carried() {
        for path in ["claude/.last-cleanup", "claude/.last-update-result.json"] {
            assert_eq!(
                classify_entry(path, &defaults()),
                Classification::Exclude,
                "{path}"
            );
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
            assert_eq!(
                classify_entry(path, &defaults()),
                Classification::Exclude,
                "{path}"
            );
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
        assert_eq!(
            classify_entry("claude/history.jsonl", &options),
            Classification::Include
        );
        assert_eq!(
            classify_entry("claude/stats-cache.json", &options),
            Classification::Exclude
        );
    }

    #[test]
    fn an_extras_entry_ending_in_a_slash_carries_the_whole_subtree() {
        let options = extras(&["claude/context-mode/"]);
        assert_eq!(
            classify_entry("claude/context-mode/index.db", &options),
            Classification::Include
        );
        assert_eq!(
            classify_entry("claude/context-mode-other/x", &options),
            Classification::Exclude
        );
    }

    #[test]
    fn memory_is_excluded_by_default() {
        assert_eq!(
            classify_entry("claude/projects/c--foo/memory/MEMORY.md", &defaults()),
            Classification::Exclude
        );
    }

    #[test]
    fn memory_is_included_with_the_flag_while_transcripts_stay_out() {
        assert_eq!(
            classify_entry("claude/projects/c--foo/memory/MEMORY.md", &with_memory()),
            Classification::Include
        );
        assert_eq!(
            classify_entry("claude/projects/c--foo/session.jsonl", &with_memory()),
            Classification::Exclude
        );
    }

    #[test]
    fn a_memory_directory_outside_projects_is_not_mistaken_for_a_memory_store() {
        assert_eq!(
            classify_entry("claude/skills/memory/SKILL.md", &with_memory()),
            Classification::Include
        );
        assert_eq!(
            classify_entry("claude/cache/memory/blob", &with_memory()),
            Classification::Exclude
        );
    }

    #[test]
    fn an_archive_is_not_swept_into_the_next_backup() {
        assert_eq!(
            classify_entry("claude/tools/claude-sync-host-20260819.zip", &defaults()),
            Classification::Exclude
        );
        assert_eq!(
            classify_entry("claude/backups/pre-restore-20260819.zip", &defaults()),
            Classification::Exclude
        );
    }

    #[test]
    fn an_unknown_top_level_file_is_excluded_rather_than_guessed() {
        assert_eq!(
            classify_entry("claude/some-future-cache.bin", &defaults()),
            Classification::Exclude
        );
    }

    #[test]
    fn classification_does_not_depend_on_path_separator_style() {
        assert_eq!(
            classify_entry(r"claude\hooks\load-repo-rules.mjs", &defaults()),
            Classification::Include
        );
    }
}
