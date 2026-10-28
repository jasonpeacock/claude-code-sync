use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::archive::{
    Entry, FileRecord, LinkRecord, Manifest, Result, ARCHIVE_VERSION, MANIFEST, MAX_TOTAL_BYTES,
};
use crate::classify::{
    classify_entry, named_by_extras, should_descend, BackupOptions, Classification, CREDENTIALS,
};
use crate::eol::{denormalize_for_disk, normalize_for_archive, LineEnding};
use crate::merge::{find_conflicts, merge_settings, Choice, Conflict, MergeStrategy};
use crate::paths::{forward_slashes, resolve_path, tokenize_path, PathContext};

/// The only file whose contents are machine-specific.
const SETTINGS: &str = "claude/settings.json";

/// JSON files whose strings hold this machine's paths. settings.json is also merged; the rest
/// replace the local copy, with paths respelled for the machine restoring them.
const TOKENIZED: &[&str] = &[
    SETTINGS,
    "claude/settings.local.json",
    "claude/plugins/installed_plugins.json",
    "claude/plugins/known_marketplaces.json",
];

/// Overrides the home directory this tool reads and writes.
///
/// On Windows the home directory comes from a Win32 known-folder lookup, not from `USERPROFILE`,
/// so setting that variable does NOT redirect it. Without an explicit override there is no safe
/// way to exercise a real restore, which is how an early test wrote onto a live config.
pub const HOME_OVERRIDE: &str = "CLAUDE_SYNC_HOME";

/// Always absolute: a relative home (`CLAUDE_SYNC_HOME=../home`) would leave nothing for the
/// containment checks to resolve links against.
pub fn home_dir() -> Result<PathBuf> {
    let home = match std::env::var_os(HOME_OVERRIDE) {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => dirs::home_dir().ok_or("could not determine the home directory")?,
    };
    let home = if home.is_absolute() {
        home
    } else {
        std::env::current_dir()?.join(home)
    };
    // Every containment check resolves paths through home. One that cannot be resolved, a
    // network share for instance, would quietly make everything look outside.
    if resolve_real(&home).is_none() {
        return Err(format!(
            "{} cannot be resolved; a home on a network path is not supported",
            home.display()
        )
        .into());
    }
    Ok(home)
}

/// One archive-relative path per line, `#` for comments, a trailing `/` for a whole subtree.
/// A missing file means no extras, which is the normal case.
pub fn read_include_list() -> Vec<String> {
    let Ok(home) = home_dir() else {
        return Vec::new();
    };
    let path = home.join(".claude").join(crate::classify::INCLUDE_LIST);
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// Falling back to a bare `node` is deliberate: an unresolved `${NODE}` would be written into
/// settings.json verbatim and the hook would never run, whereas `node` resolves via PATH.
pub fn find_node() -> String {
    let binary = if cfg!(windows) { "node.exe" } else { "node" };
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(binary);
            if candidate.is_file() {
                return forward_slashes(&candidate.to_string_lossy());
            }
        }
    }
    "node".into()
}

pub fn current_context() -> Result<PathContext> {
    let home = home_dir()?;
    let spell = |path: &Path| forward_slashes(path.to_string_lossy().trim_end_matches(['/', '\\']));
    let real_home = resolve_real(&home)
        .map(|real| spell(&real))
        .filter(|real| *real != spell(&home));
    Ok(PathContext {
        home: spell(&home),
        node: find_node(),
        real_home,
    })
}

fn roots(home: &Path) -> [(&'static str, PathBuf); 2] {
    [
        ("claude", home.join(".claude")),
        ("agents", home.join(".agents")),
    ]
}

/// Text from an archive or a file name, with control and format characters escaped so it cannot
/// rewrite the terminal. A right-to-left override, for one, reorders everything after it.
pub fn printable(text: &str) -> String {
    let invisible = |c: char| {
        c.is_control()
            || matches!(c, '\u{ad}' | '\u{61c}' | '\u{180e}' | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}'
                | '\u{fff9}'..='\u{fffb}' | '\u{e0000}'..='\u{e007f}')
    };
    text.chars()
        .flat_map(|c| {
            if invisible(c) {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

/// Where a link at `link` pointing at `target` lands. An absolute target replaces the join.
fn link_destination(link: &Path, target: &str) -> PathBuf {
    let target = Path::new(target);
    match link.parent() {
        Some(parent) => parent.join(target),
        None => target.to_path_buf(),
    }
}

/// Resolve a path the way the kernel will: each part that exists now is followed through its
/// links, and the rest is appended as spelled. A dangling link yields `None`, and so does a
/// network path: on Windows, merely opening `\\host\share` sends the user's credentials to
/// whoever answers.
fn resolve_real(path: &Path) -> Option<PathBuf> {
    let mut current = PathBuf::new();
    let mut exists = true;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                use std::path::Prefix;
                if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) {
                    return None;
                }
                current.push(component.as_os_str());
            }
            Component::RootDir => current.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                current.pop();
                exists = fs::symlink_metadata(&current).is_ok();
            }
            Component::Normal(part) => {
                current.push(part);
                if exists {
                    if fs::symlink_metadata(&current).is_ok() {
                        current = fs::canonicalize(&current).ok()?;
                    } else {
                        exists = false;
                    }
                }
            }
        }
    }
    Some(current)
}

/// CRITICAL: the only symlinks this tool carries or makes. A skill installed once under
/// `~/.agents/skills` and linked into `~/.claude/skills`, or `~/.claude/skills` linked there
/// whole. Both ends are named by archive path. Every other link is reported and left out, on
/// backup and on restore alike: each wider rule tried before this one let a crafted archive
/// reach outside the roots, or carry transcripts and credentials under another name.
fn is_skill_link(link: &str, target: &str) -> bool {
    let name = |n: &str| {
        !n.contains('/')
            && !NEVER_CARRIED_DIRS.contains(&n)
            && check_safe(&format!("agents/skills/{n}")).is_ok()
    };
    match (
        link.strip_prefix("claude/skills/"),
        target.strip_prefix("agents/skills/"),
    ) {
        (Some(from), Some(to)) => name(from) && name(to),
        _ => link == "claude/skills" && target == "agents/skills",
    }
}

/// The archive path a link target names, worked out from its spelling alone: `..` is collapsed
/// and the home prefix, in either the spelled or the resolved form, is stripped. Nothing on disk
/// is consulted, so nothing on disk can change the answer.
fn named_archive_path(home: &Path, link: &Path, target: &str) -> Option<String> {
    let mut spelled = PathBuf::new();
    for component in link_destination(link, target).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                spelled.pop();
            }
            other => spelled.push(other.as_os_str()),
        }
    }
    let homes = [Some(home.to_path_buf()), resolve_real(home)];
    let rest = homes
        .iter()
        .flatten()
        .find_map(|home| spelled.strip_prefix(home).ok())?;
    let rest = forward_slashes(&rest.to_string_lossy());
    match rest.split_once('/') {
        Some((".claude", tail)) => Some(format!("claude/{tail}")),
        Some((".agents", tail)) => Some(format!("agents/{tail}")),
        _ => None,
    }
}

/// macOS and Windows ignore case, so there `claude/X` and `claude/x` are one file.
fn fold(path: &str) -> String {
    if cfg!(any(windows, target_os = "macos")) {
        path.to_lowercase()
    } else {
        path.to_string()
    }
}

/// The two restore roots as the filesystem resolves them. A home reached through a symlink
/// (`/home/you -> /local/home/you`) has two spellings, and both resolve to the same place.
struct Roots {
    real: Vec<(&'static str, PathBuf)>,
}

impl Roots {
    fn new(home: &Path) -> Self {
        Self {
            real: roots(home)
                .into_iter()
                .filter_map(|(prefix, root)| resolve_real(&root).map(|real| (prefix, real)))
                .collect(),
        }
    }

    fn holds(&self, real: &Path) -> bool {
        self.real.iter().any(|(_, root)| real.starts_with(root))
    }

    /// CRITICAL: `check_safe` vets how the archive spells a path, not what the filesystem does
    /// with it. A link already on disk (`~/.claude/CLAUDE.md -> ~/dotfiles/CLAUDE.md`) carries a
    /// write straight out of the roots, so containment is decided on the resolved path.
    fn contain(&self, path: &Path) -> bool {
        resolve_real(path).is_some_and(|real| self.holds(&real))
    }

    /// The archive path a resolved location would be carried under, if it is inside the roots.
    /// The most specific root wins: with `~/.agents -> ~/.claude/agents-src`, a path under it is
    /// an `agents/` path, not a `claude/` one.
    fn archive_path(&self, real: &Path) -> Option<String> {
        let mut inside: Vec<&(&str, PathBuf)> = self
            .real
            .iter()
            .filter(|(_, root)| real.starts_with(root))
            .collect();
        inside.sort_by_key(|(_, root)| std::cmp::Reverse(root.components().count()));
        inside.into_iter().next().and_then(|(prefix, root)| {
            let rest = real.strip_prefix(root).ok()?;
            let rest = forward_slashes(&rest.to_string_lossy());
            Some(if rest.is_empty() {
                prefix.to_string()
            } else {
                format!("{prefix}/{rest}")
            })
        })
    }
}

/// Replace a file rather than write into it. Writing in place truncates whatever inode is there,
/// and a hard link, which no path check can see, would carry the write to its other name. The
/// new file is made beside the real destination, so a symlink inside the roots is still written
/// through, and it keeps the old file's permissions.
fn replace_file(destination: &Path, data: &[u8], private: bool) -> std::io::Result<()> {
    let real = resolve_real(destination).unwrap_or_else(|| destination.to_path_buf());
    let previous = fs::metadata(&real).ok();
    let name = real.file_name().unwrap_or_default().to_string_lossy();
    let temporary = real.with_file_name(format!(".{name}.ccs-{}", std::process::id()));
    let written = if private {
        write_private(&temporary, data)
    } else {
        fs::write(&temporary, data).and_then(|()| match previous {
            Some(previous) => fs::set_permissions(&temporary, previous.permissions()),
            None => Ok(()),
        })
    };
    let renamed = written.and_then(|()| fs::rename(&temporary, &real));
    if renamed.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    match renamed {
        // A file bind-mounted on its own (common in containers) cannot be renamed over. With a
        // single link to its inode, writing in place reaches nothing else.
        Err(error) if cannot_rename_over(&error) && single_link(&real) => {
            if private {
                write_private(&real, data)
            } else {
                fs::write(&real, data)
            }
        }
        other => other,
    }
}

#[cfg(unix)]
fn cannot_rename_over(error: &std::io::Error) -> bool {
    // EBUSY for a mount point, EXDEV for a different filesystem.
    matches!(error.raw_os_error(), Some(16 | 18))
}

#[cfg(not(unix))]
fn cannot_rename_over(_: &std::io::Error) -> bool {
    false
}

#[cfg(unix)]
fn single_link(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).is_ok_and(|m| m.nlink() == 1)
}

#[cfg(not(unix))]
fn single_link(_: &Path) -> bool {
    false
}

/// Write a file that must not exist yet: an archive or a snapshot. Never follows or replaces
/// whatever is already at that name, a planted link included. `private` makes it owner-only.
pub fn write_new(path: &Path, data: &[u8], private: bool) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    options.open(path)?.write_all(data)
}

/// Write a file only its owner can read. Used for anything that may hold credentials.
pub fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        // `mode` only applies when the file is created; an existing one keeps its old bits.
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(data)
    }
    #[cfg(not(unix))]
    {
        fs::write(path, data)
    }
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_: &fs::Metadata) -> bool {
    false
}

/// `chmod +x` as far as the file is readable, so the umask still decides who may run it.
#[cfg(unix)]
fn set_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)?.permissions().mode();
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(mode | ((mode & 0o444) >> 2)),
    )
}

#[cfg(not(unix))]
fn set_executable(_: &Path) -> std::io::Result<()> {
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Name the file an io error happened to. `os error 3` on its own points at nothing.
fn at(path: &Path) -> impl Fn(std::io::Error) -> Box<dyn std::error::Error> + '_ {
    move |error| format!("{}: {error}", path.display()).into()
}

/// Refuse an archive path that could write outside the two restore roots.
///
/// CRITICAL: a zip is untrusted input. Left unchecked, an entry named `claude/../../.bashrc` is
/// joined straight onto the home directory and lands outside it, so the check is on the path
/// components rather than on the joined result: `..`, an absolute segment and a drive letter are
/// all refused, in either separator style, whichever OS wrote the archive.
fn check_safe(archive_path: &str) -> Result<()> {
    // Also refused, on every OS since archives travel: names a filesystem rejects or aliases.
    // Windows strips a trailing dot or space, treats `name:stream` as the same file, and maps
    // CON or NUL to a device; an over-long name fails only when written, after other writes.
    let reserved = |part: &str| {
        let stem = part
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit())
    };
    // The aliasing rules only bite where the filesystem aliases: `aux.py` and `notes 10:00.md`
    // are ordinary names on Linux and macOS, and refusing them there would refuse a backup of a
    // perfectly normal config. A drive-letter prefix is refused everywhere.
    let windows_alias = |part: &str| {
        cfg!(windows)
            && (part.contains(':') || part.ends_with('.') || part.ends_with(' ') || reserved(part))
    };
    let bad = archive_path.split(['/', '\\']).any(|part| {
        part.is_empty()
            || part == "."
            || part == ".."
            || Path::new(part).is_absolute()
            || matches!(part.as_bytes(), [_, b':', ..])
            || part.contains('\0')
            || part.len() > 255
            || windows_alias(part)
    });
    if bad {
        return Err(format!(
            "archive entry would write outside ~/.claude and ~/.agents, refusing: {archive_path}"
        )
        .into());
    }
    Ok(())
}

struct Walked {
    files: Vec<(String, PathBuf)>,
    links: Vec<LinkRecord>,
    skipped_secrets: Vec<String>,
}

/// Never config, and big: VCS history, dependency installs, interpreter environments.
const NEVER_CARRIED_DIRS: &[&str] = &[".git", "node_modules", ".venv", "__pycache__"];

/// A skill link found on disk, as the target a link record stores, or `None` for any other
/// link. Judged by where the link really leads, so a chain of links ending in a skill counts.
fn skill_link_record(
    archive_path: &str,
    absolute: &Path,
    home: &Path,
    roots: &Roots,
    ctx: &PathContext,
) -> Option<String> {
    if !fs::metadata(absolute).is_ok_and(|m| m.is_dir()) {
        return None;
    }
    let target = roots.archive_path(&resolve_real(absolute)?)?;
    if !is_skill_link(archive_path, &target) {
        return None;
    }
    let location = location(home, &target)?;
    Some(tokenize_path(
        &forward_slashes(&location.to_string_lossy()),
        ctx,
    ))
}

fn walk(
    root: &Path,
    prefix: &str,
    ctx: &PathContext,
    options: &BackupOptions,
    roots: &Roots,
) -> Result<Walked> {
    let mut found = Walked {
        files: Vec::new(),
        links: Vec::new(),
        skipped_secrets: Vec::new(),
    };
    if !root.exists() {
        return Ok(found);
    }

    let home = home_dir()?;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).map_err(at(&dir))? {
            let absolute = entry.map_err(at(&dir))?.path();
            let relative = absolute.strip_prefix(root).unwrap_or(&absolute);
            let archive_path = format!("{prefix}/{}", forward_slashes(&relative.to_string_lossy()));
            let metadata = fs::symlink_metadata(&absolute).map_err(at(&absolute))?;

            if metadata.is_symlink() {
                // Links are never followed, so nothing outside the roots, and nothing inside
                // them that backups leave out, rides along under a carried name.
                match skill_link_record(&archive_path, &absolute, &home, roots, ctx) {
                    Some(target) => found.links.push(LinkRecord {
                        path: archive_path,
                        target,
                    }),
                    None if classify_entry(&archive_path, options) == Classification::Include
                        || should_descend(&archive_path, options) =>
                    {
                        eprintln!(
                            "  not carried: {} is a symlink; only skill links into \
                             ~/.agents/skills are carried",
                            printable(&archive_path)
                        )
                    }
                    None => {}
                }
                continue;
            }

            if metadata.is_dir() {
                let name = absolute.file_name().unwrap_or_default();
                if NEVER_CARRIED_DIRS.iter().any(|never| name == *never)
                    && !named_by_extras(&archive_path, options)
                {
                    if name != "__pycache__" && should_descend(&archive_path, options) {
                        eprintln!(
                            "  not carried: {}/ (name it in .claude-sync-include to carry it)",
                            printable(&archive_path)
                        );
                    }
                    continue;
                }
                // Pruning here is the difference between opening ~200 entries and ~15000: the
                // caches and transcript stores under ~/.claude dwarf everything carried.
                if should_descend(&archive_path, options) {
                    stack.push(absolute);
                }
            } else if metadata.is_file() {
                // Sockets and FIFOs are not files: reading a FIFO waits forever.
                match classify_entry(&archive_path, options) {
                    Classification::Secret => found.skipped_secrets.push(archive_path),
                    Classification::Include => found.files.push((archive_path, absolute)),
                    Classification::Exclude => {}
                }
            }
        }
    }

    // Sorted globally rather than per directory, so an archive is byte-stable across runs.
    found.files.sort_by(|a, b| a.0.cmp(&b.0));
    found.links.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(found)
}

/// A file's bytes the way an archive stores them: LF line endings, and in settings.json this
/// machine's paths spelled as tokens.
/// `lenient` carries an unparseable settings.json as is. A snapshot needs that, since a broken
/// settings.json is often why someone is restoring; a backup does not, because no restore could
/// then apply the archive it wrote.
fn pack(
    archive_path: &str,
    absolute: &Path,
    raw: &[u8],
    ctx: &PathContext,
    lenient: bool,
) -> Result<Vec<u8>> {
    let data = normalize_for_archive(raw);
    if !TOKENIZED.contains(&archive_path) {
        return Ok(data);
    }
    match serde_json::from_slice::<serde_json::Value>(strip_bom(&data)) {
        Ok(parsed) => {
            let tokenized = map_strings(&parsed, &|s| tokenize_path(s, ctx));
            Ok(format!("{}\n", serde_json::to_string_pretty(&tokenized)?).into_bytes())
        }
        // Only settings.json is merged on restore, so only it has to parse.
        Err(error) if !lenient && archive_path == SETTINGS => {
            Err(format!("{}: {error}", absolute.display()).into())
        }
        Err(error) => {
            eprintln!(
                "  {} is not valid JSON ({error}); carried as is, paths not rewritten, and only \
                 restorable with --merge=replace",
                absolute.display()
            );
            Ok(data)
        }
    }
}

fn zip_with_manifest(manifest: &Manifest, entries: Vec<Entry>) -> Result<Vec<u8>> {
    let mut all = vec![Entry {
        path: MANIFEST.into(),
        data: serde_json::to_vec_pretty(manifest)?,
    }];
    all.extend(entries);
    crate::archive::write_zip(&all)
}

pub fn backup(
    options: &BackupOptions,
    created_at: String,
    host: String,
) -> Result<(Vec<u8>, Manifest)> {
    let ctx = current_context()?;
    let home = home_dir()?;
    let mut manifest = Manifest::new(created_at, host, options);
    let mut entries: Vec<Entry> = Vec::new();
    let link_roots = Roots::new(&home);
    let mut total = 0u64;

    for (prefix, root) in roots(&home) {
        let walked = walk(&root, prefix, &ctx, options, &link_roots)?;
        for secret in &walked.skipped_secrets {
            eprintln!(
                "  skipped secret: {}  (use --include-credentials to carry it)",
                printable(secret)
            );
        }
        manifest.links.extend(walked.links);

        for (archive_path, absolute) in walked.files {
            let raw = fs::read(&absolute).map_err(at(&absolute))?;
            let executable = fs::metadata(&absolute).is_ok_and(|m| is_executable(&m));
            let data = pack(&archive_path, &absolute, &raw, &ctx, false)?;
            // Checked here too, so an archive restore would refuse is never written at all.
            total += data.len() as u64;
            if total > MAX_TOTAL_BYTES {
                return Err(format!(
                    "{}: the archive would pass 512 MB, which restore refuses",
                    absolute.display()
                )
                .into());
            }
            manifest.files.push(FileRecord {
                path: archive_path.clone(),
                sha256: sha256_hex(&data),
                size: data.len(),
                executable,
            });
            entries.push(Entry {
                path: archive_path,
                data,
            });
        }
    }

    if total + serde_json::to_vec_pretty(&manifest)?.len() as u64 > MAX_TOTAL_BYTES {
        return Err("the archive would pass 512 MB, which restore refuses".into());
    }
    Ok((zip_with_manifest(&manifest, entries)?, manifest))
}

pub fn map_strings(value: &serde_json::Value, f: &dyn Fn(&str) -> String) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(s) => Value::String(f(s)),
        Value::Array(items) => Value::Array(items.iter().map(|v| map_strings(v, f)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), map_strings(v, f)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Spell this machine's own paths the way a restored value spells them.
///
/// CRITICAL: merging compares strings, so a local `"C:\Users\you\.claude\hooks\x.mjs"` and the
/// restored `"C:/Users/you/.claude/hooks/x.mjs"` are two different entries and the union appends
/// the second one, leaving the stale hook listed and firing. Round-tripping the local side
/// through the same tokenize-then-resolve the incoming side already went through collapses the
/// pair. Values holding no path come back untouched, which is the tokenizer's guarantee.
pub fn canonicalize(value: &serde_json::Value, ctx: &PathContext) -> serde_json::Value {
    map_strings(value, &|s| resolve_path(&tokenize_path(s, ctx), ctx))
}

/// Some Windows editors save JSON with a UTF-8 byte order mark, which serde_json rejects.
fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)
}

/// This machine's settings.json, spelled the way a restored value spells it. A missing file
/// reads as an empty object, which merges to exactly the incoming side.
fn local_settings(path: &Path, ctx: &PathContext) -> Result<serde_json::Value> {
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let bytes = fs::read(path).map_err(at(path))?;
    let parsed: serde_json::Value = serde_json::from_slice(strip_bom(&bytes))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(canonicalize(&parsed, ctx))
}

fn incoming_settings(entry: &Entry, ctx: &PathContext) -> Result<serde_json::Value> {
    let archived: serde_json::Value =
        serde_json::from_slice(strip_bom(&entry.data)).map_err(|e| {
            format!("{SETTINGS} inside the archive: {e} (--merge=replace writes it as it is)")
        })?;
    Ok(map_strings(&archived, &|s| resolve_path(s, ctx)))
}

/// The conflicts a restore would hit in settings.json, collected before anything is written so
/// the interactive prompt is not tangled into the recursive merge.
pub fn settings_conflicts(entries: &[Entry]) -> Result<Vec<Conflict>> {
    let Some(archived) = entries.iter().find(|e| e.path == SETTINGS) else {
        return Ok(Vec::new());
    };
    let home = home_dir()?;
    let local = home.join(".claude").join("settings.json");
    // Nothing to ask about when restore will leave settings.json alone, as it does when a
    // symlink carries it out of the roots.
    if !local.exists() || !Roots::new(&home).contain(&local) {
        return Ok(Vec::new());
    }
    let ctx = current_context()?;
    Ok(find_conflicts(
        &local_settings(&local, &ctx)?,
        &incoming_settings(archived, &ctx)?,
    ))
}

/// CRITICAL: every entry and link is checked before the first byte is written. Validating in one
/// pass up front is what keeps a rejected archive from leaving a half applied config behind.
fn vet(entries: &[Entry], manifest: &Manifest, home: &Path) -> Result<()> {
    let recorded: HashMap<&str, &str> = manifest
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.sha256.as_str()))
        .collect();
    let outside = |path: &str| -> Box<dyn std::error::Error> {
        format!("archive entry is outside claude/ and agents/, refusing to restore: {path}").into()
    };

    for entry in entries {
        if entry.path == MANIFEST {
            continue;
        }
        check_safe(&entry.path)?;
        if target_path(home, &entry.path).is_none() {
            return Err(outside(&entry.path));
        }

        // The manifest is the authority on what an archive may write. An entry not listed there,
        // or whose bytes do not match the hash recorded for it, was added after the archive was
        // written, so the restore stops rather than the entry being quietly skipped.
        let Some(sha256) = recorded.get(entry.path.as_str()) else {
            return Err(format!(
                "archive entry is not listed in manifest.json, refusing to restore: {}",
                entry.path
            )
            .into());
        };
        if sha256_hex(&entry.data) != *sha256 {
            return Err(format!(
                "archive entry does not match its manifest hash, refusing to restore: {}",
                entry.path
            )
            .into());
        }
    }

    let present: HashSet<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    if let Some(missing) = manifest
        .files
        .iter()
        .find(|f| !present.contains(f.path.as_str()))
    {
        return Err(format!(
            "manifest.json lists {} but the archive does not contain it, refusing to restore",
            missing.path
        )
        .into());
    }

    // One path, one thing. A second link at a path the first has just created fails, and the
    // copy fallback would then write through the first.
    let mut claimed = HashSet::new();
    for path in entries
        .iter()
        .filter(|e| e.path != MANIFEST)
        .map(|e| e.path.as_str())
        .chain(manifest.links.iter().map(|l| l.path.as_str()))
    {
        if !claimed.insert(fold(path)) {
            return Err(format!("archive names {path} more than once, refusing to restore").into());
        }
    }

    // A backup never walks into a link, so nothing it writes sits under one. An archive that
    // does put a path under its own link is steering a write through it.
    for link in &manifest.links {
        check_safe(&link.path)?;
        if target_path(home, &link.path).is_none() {
            return Err(outside(&link.path));
        }
        let under = format!("{}/", fold(&link.path));
        if let Some(nested) = claimed.iter().find(|path| path.starts_with(&under)) {
            return Err(format!(
                "archive puts {nested} under its own link {}, refusing to restore",
                link.path
            )
            .into());
        }
    }
    // Nor can one file sit under another: the second write would fail after the first.
    for path in &claimed {
        let mut above = path.as_str();
        while let Some((parent, _)) = above.rsplit_once('/') {
            if claimed.contains(parent) {
                return Err(format!(
                    "archive puts {path} under {parent}, which it also names, refusing to restore"
                )
                .into());
            }
            above = parent;
        }
    }

    Ok(())
}

/// Where this tool keeps its snapshots. No backup carries anything here, so an archive entry or
/// link that does is aimed at them, or at the next snapshot's name.
fn snapshot_territory(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".zip") || lower == "claude/backups" || lower.starts_with("claude/backups/")
}

/// A snapshot of exactly what this restore will overwrite, read from the paths the archive
/// names rather than walked.
///
/// CRITICAL: a walk would leave out what classify excludes (a `.zip`, a transcript), miss a
/// differently cased spelling on a case-insensitive disk, follow links into trees the archive
/// never touches, and trip the size caps on them. Reading the named paths has none of those gaps.
/// It may hold the credentials file, so the caller writes it owner-only.
pub fn snapshot(archive: &Manifest, created_at: String, host: String) -> Result<Vec<u8>> {
    let ctx = current_context()?;
    let home = home_dir()?;
    let mut manifest = Manifest::new(
        created_at,
        host,
        &BackupOptions {
            with_memory: true,
            ..Default::default()
        },
    );
    let mut entries = Vec::new();

    for record in &archive.files {
        let Some(absolute) = target_path(&home, &record.path) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&absolute) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let raw = fs::read(&absolute).map_err(at(&absolute))?;
        let data = pack(&record.path, &absolute, &raw, &ctx, true)?;
        if record.path.eq_ignore_ascii_case(CREDENTIALS) {
            manifest.includes_credentials = true;
        }
        manifest.files.push(FileRecord {
            path: record.path.clone(),
            sha256: sha256_hex(&data),
            size: data.len(),
            executable: is_executable(&metadata),
        });
        entries.push(Entry {
            path: record.path.clone(),
            data,
        });
    }

    let total: usize = entries.iter().map(|e| e.data.len()).sum();
    if total as u64 > MAX_TOTAL_BYTES {
        eprintln!(
            "  the snapshot holds more than 512 MB, which restore refuses: unzip it by hand to \
             roll back"
        );
    }
    zip_with_manifest(&manifest, entries)
}

pub struct RestoreOptions<'a> {
    pub strategy: MergeStrategy,
    pub dry_run: bool,
    pub resolve: &'a mut dyn FnMut(&Conflict) -> Choice,
}

pub fn read_manifest(entries: &[Entry]) -> Result<Manifest> {
    let entry = entries
        .iter()
        .find(|e| e.path == MANIFEST)
        .ok_or("archive has no manifest.json; refusing to restore")?;
    let manifest: Manifest =
        serde_json::from_slice(&entry.data).map_err(|e| format!("{MANIFEST}: {e}"))?;

    if manifest.tool != "claude-code-sync" {
        return Err("not a claude-code-sync archive".into());
    }
    if manifest.version > ARCHIVE_VERSION {
        return Err(format!(
            "archive version {} is newer than this build understands",
            manifest.version
        )
        .into());
    }
    Ok(manifest)
}

/// Action lines starting with this are links that could be neither created nor copied.
pub const FAILED: &str = "failed";
/// Action lines starting with this were left alone because of what is on this machine, or
/// because the archive pointed them out of the roots.
pub const SKIPPED: &str = "skipped";

pub fn restore(
    entries: &[Entry],
    manifest: &Manifest,
    options: &mut RestoreOptions,
) -> Result<Vec<String>> {
    let ctx = current_context()?;
    let home = home_dir()?;
    let roots = Roots::new(&home);
    let eol = LineEnding::native();
    let mut actions = Vec::new();

    vet(entries, manifest, &home)?;
    // Read before anything is written: the archive may carry its own include list, and that
    // must not widen what this restore will write through a link.
    let carried = BackupOptions {
        with_memory: false,
        include_credentials: false,
        extras: read_include_list(),
    };

    let executable: HashSet<&str> = manifest
        .files
        .iter()
        .filter(|f| f.executable)
        .map(|f| f.path.as_str())
        .collect();

    // CRITICAL: everything that can fail is worked out before the first write, the settings
    // merge included, so a bad settings.json on either side cannot leave a half applied restore.
    let mut writes: Vec<(PathBuf, Vec<u8>, &str)> = Vec::new();
    for entry in entries {
        if entry.path == MANIFEST {
            continue;
        }
        let absolute = target_path(&home, &entry.path).ok_or("vetted entry has no root")?;
        // No backup carries a .zip; one here is aimed at the snapshots in ~/.claude/backups.
        if snapshot_territory(&entry.path) {
            actions.push(format!(
                "{SKIPPED}  {}: a backup never carries a .zip or ~/.claude/backups",
                entry.path
            ));
            continue;
        }
        // A link the user made (`CLAUDE.md -> ~/dotfiles/CLAUDE.md`) is theirs. Writing through
        // it would leave the roots, so the file is left as it is and the skip is reported.
        if !roots.contain(&absolute) {
            let dangling =
                fs::symlink_metadata(&absolute).is_ok() && fs::metadata(&absolute).is_err();
            let reason = if dangling {
                "a dangling symlink is here"
            } else {
                "a symlink here leads outside ~/.claude and ~/.agents"
            };
            actions.push(format!("{SKIPPED}  {}: {reason}", entry.path));
            continue;
        }
        // A link inside the roots can still lead somewhere backups leave alone, a plugin
        // checkout or a transcript store. Writing there would clobber it with stale files.
        let lands = resolve_real(&absolute).and_then(|real| roots.archive_path(&real));
        if let Some(lands) = lands.filter(|lands| fold(lands) != fold(&entry.path)) {
            if classify_entry(&lands, &carried) != Classification::Include {
                actions.push(format!(
                    "{SKIPPED}  {}: a symlink here leads into {lands}, which backups do not carry",
                    entry.path
                ));
                continue;
            }
        }

        let unparseable = entry.path == SETTINGS
            && serde_json::from_slice::<serde_json::Value>(strip_bom(&entry.data)).is_err();
        let data: Cow<[u8]> = if unparseable && options.strategy == MergeStrategy::Replace {
            // Nothing to merge, so it is written as it is: a snapshot taken while settings.json
            // was broken has to be restorable, broken file and all.
            Cow::Borrowed(&entry.data)
        } else if entry.path == SETTINGS {
            let local = if options.strategy == MergeStrategy::Replace {
                serde_json::json!({})
            } else {
                local_settings(&absolute, &ctx)?
            };
            let merged = merge_settings(
                &local,
                &incoming_settings(entry, &ctx)?,
                options.strategy,
                options.resolve,
            );
            Cow::Owned(format!("{}\n", serde_json::to_string_pretty(&merged)?).into_bytes())
        } else if TOKENIZED.contains(&entry.path.as_str()) {
            // Written whole, as before, with this machine's paths. One that does not parse was
            // carried as it was, and is written as it is.
            match serde_json::from_slice::<serde_json::Value>(strip_bom(&entry.data)) {
                Ok(parsed) => {
                    let resolved = map_strings(&parsed, &|s| resolve_path(s, &ctx));
                    Cow::Owned(
                        format!("{}\n", serde_json::to_string_pretty(&resolved)?).into_bytes(),
                    )
                }
                Err(_) => Cow::Borrowed(&entry.data),
            }
        } else {
            Cow::Borrowed(&entry.data)
        };

        let existed = fs::symlink_metadata(&absolute).is_ok();
        actions.push(format!(
            "{}  {}",
            if existed { "update" } else { "create" },
            entry.path
        ));
        writes.push((
            absolute,
            denormalize_for_disk(&data, eol),
            entry.path.as_str(),
        ));
    }

    let mut links: Vec<(PathBuf, PathBuf, String, &str, usize)> = Vec::new();
    for link in &manifest.links {
        let absolute = target_path(&home, &link.path).ok_or("vetted link has no root")?;
        // Anything already there is left alone, a dangling link included: following one to
        // decide would write through it.
        if fs::symlink_metadata(&absolute).is_ok() {
            continue;
        }
        let target = resolve_path(&link.target, &ctx);
        let named = named_archive_path(&home, &absolute, &target);
        let Some(named) = named.filter(|named| is_skill_link(&link.path, named)) else {
            actions.push(format!(
                "{SKIPPED}  {}: only skill links into ~/.agents/skills are recreated ({})",
                link.path,
                printable(&target)
            ));
            continue;
        };
        let location = location(&home, &named).ok_or("vetted link target has no root")?;
        actions.push(format!("link    {} -> {}", link.path, printable(&target)));
        links.push((
            absolute,
            location,
            named,
            link.path.as_str(),
            actions.len() - 1,
        ));
    }

    if options.dry_run {
        return Ok(actions);
    }

    for (absolute, data, path) in writes {
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).map_err(at(parent))?;
        }
        replace_file(&absolute, &data, path.eq_ignore_ascii_case(CREDENTIALS))
            .map_err(at(&absolute))?;
        if executable.contains(path) {
            set_executable(&absolute).map_err(at(&absolute))?;
        }
    }

    // Checked against the filesystem as the writes above left it. Each end has to be exactly
    // where its archive path says, with no symlink along the way: with `~/.claude/skills` itself
    // a link, a `claude/skills/x` link would land somewhere else entirely, and with
    // `~/.agents/skills/x` a link, the new one would lead wherever that one does.
    let roots = Roots::new(&home);
    let at_home = |path: &Path| resolve_real(path).and_then(|real| roots.archive_path(&real));
    for (absolute, location, named, path, action) in links {
        let parent = absolute.parent().unwrap_or(&home);
        let parent_named = path.rsplit_once('/').map(|(parent, _)| parent);
        let reason = if fs::symlink_metadata(&absolute).is_ok() {
            Some("already present")
        } else if at_home(parent).as_deref() != parent_named {
            Some("a symlink on this machine is in the way")
        } else if !fs::symlink_metadata(&location).is_ok_and(|m| m.is_dir())
            || at_home(&location).as_deref() != Some(named.as_str())
        {
            Some("its target is not a plain directory here")
        } else {
            None
        };
        if let Some(reason) = reason {
            actions[action] = format!("{SKIPPED}  {path}: {reason}");
            continue;
        }
        fs::create_dir_all(parent).map_err(at(parent))?;
        // Relative, as skill installers write them, worked out between the real directories:
        // the kernel resolves `..` from where the link really sits, which with `~/.claude`
        // itself a link is not where its spelling says. Checked, and absolute if it is off.
        let (Some(real_parent), Some(real_location)) =
            (resolve_real(parent), resolve_real(&location))
        else {
            actions[action] = format!("{SKIPPED}  {path}: a symlink on this machine is in the way");
            continue;
        };
        let spelled = relative_to(&real_parent, &real_location)
            .filter(|spelled| {
                resolve_real(&real_parent.join(spelled)).as_ref() == Some(&real_location)
            })
            .unwrap_or_else(|| real_location.clone());
        let Err(error) = create_link(&spelled, &absolute, &location) else {
            actions[action] = format!(
                "link    {path} -> {}",
                forward_slashes(&spelled.to_string_lossy())
            );
            continue;
        };
        // Windows without developer mode cannot create links; a real copy is the honest
        // fallback.
        if !copy_instead(&location, &absolute, &roots).map_err(at(&absolute))? {
            actions[action] = format!("{FAILED}  {path}: could not link or copy: {error}");
        }
    }

    Ok(actions)
}

/// Copy what a link would have pointed at, when the OS refused the link. Only into a path still
/// empty, only from inside the roots, never into or out of itself. `false` when refused.
fn copy_instead(source: &Path, destination: &Path, roots: &Roots) -> std::io::Result<bool> {
    if fs::symlink_metadata(destination).is_ok() {
        return Ok(false);
    }
    let (Some(from), Some(to)) = (resolve_real(source), resolve_real(destination)) else {
        return Ok(false);
    };
    // `to` does not exist yet, so it cannot sit above an existing `from`; only the other
    // direction, copying a tree into itself, needs refusing.
    if fs::metadata(&from).is_err()
        || !roots.holds(&from)
        || !roots.holds(&to)
        || to.starts_with(&from)
    {
        return Ok(false);
    }
    copy_tree(&from, destination)?;
    Ok(true)
}

/// `to` relative to the directory `from`, when both share the home directory. Both must be
/// spelled without links or `..`, as [`location`] spells them.
fn relative_to(from: &Path, to: &Path) -> Option<PathBuf> {
    let from: Vec<Component> = from.components().collect();
    let to: Vec<Component> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return None;
    }
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for part in &to[common..] {
        out.push(part.as_os_str());
    }
    Some(out)
}

/// [`target_path`], plus the bare roots `claude` and `agents`, which a link may point at but
/// no file may be written to.
fn location(home: &Path, archive_path: &str) -> Option<PathBuf> {
    match archive_path {
        "claude" => Some(home.join(".claude")),
        "agents" => Some(home.join(".agents")),
        _ => target_path(home, archive_path),
    }
}

fn target_path(home: &Path, archive_path: &str) -> Option<PathBuf> {
    let (prefix, rest) = archive_path.split_once('/')?;
    let root = match prefix {
        "claude" => home.join(".claude"),
        "agents" => home.join(".agents"),
        _ => return None,
    };
    Some(rest.split('/').fold(root, |acc, part| acc.join(part)))
}

/// `source` is where the target lands from the link's directory: a relative target means
/// nothing against the process's working directory, which is what `target.is_dir()` would ask.
#[cfg(windows)]
fn create_link(target: &Path, link: &Path, source: &Path) -> std::io::Result<()> {
    if source.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

#[cfg(not(windows))]
fn create_link(target: &Path, link: &Path, _source: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// Links inside the copied tree are skipped, not followed: following them could leave the roots
/// or loop forever.
fn copy_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    if fs::metadata(source)?.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
        return Ok(());
    }
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_type()?.is_symlink() {
            eprintln!(
                "  not copying nested link {}",
                printable(&entry.path().to_string_lossy())
            );
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.eq_ignore_ascii_case(".credentials.json") || NEVER_CARRIED_DIRS.contains(&&*name) {
            continue;
        }
        copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The copy fallback only runs when the OS refuses a link, which no test machine does, so
    // its guards are exercised directly.
    #[cfg(unix)]
    #[test]
    fn the_copy_fallback_stays_inside_the_roots_and_out_of_itself() {
        let home = std::env::temp_dir().join(format!("ccs-copy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let outside = home.with_extension("outside");
        fs::create_dir_all(home.join(".agents/skills/x")).unwrap();
        fs::write(home.join(".agents/skills/x/SKILL.md"), "# x").unwrap();
        std::os::unix::fs::symlink("/", home.join(".agents/skills/x/root")).unwrap();
        fs::create_dir_all(home.join(".claude/skills")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("id_rsa"), "PRIVATE").unwrap();
        let roots = Roots::new(&home);

        let from_outside = home.join(".claude/skills/stolen");
        assert!(!copy_instead(&outside, &from_outside, &roots).unwrap());
        assert!(!from_outside.exists());

        let into_itself = home.join(".agents/skills/x/again");
        assert!(!copy_instead(&home.join(".agents/skills/x"), &into_itself, &roots).unwrap());

        let copied = home.join(".claude/skills/x");
        assert!(copy_instead(&home.join(".agents/skills/x"), &copied, &roots).unwrap());
        assert_eq!(fs::read_to_string(copied.join("SKILL.md")).unwrap(), "# x");
        assert!(
            fs::symlink_metadata(copied.join("root")).is_err(),
            "a nested link was followed"
        );
        assert!(!copy_instead(&home.join(".agents/skills/x"), &copied, &roots).unwrap());

        fs::remove_dir_all(&home).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn an_archive_path_maps_onto_the_right_root() {
        let home = Path::new("/home/dev");
        assert_eq!(
            target_path(home, "claude/hooks/x.mjs"),
            Some(Path::new("/home/dev/.claude/hooks/x.mjs").to_path_buf())
        );
        assert_eq!(
            target_path(home, "agents/skills/grilling/SKILL.md"),
            Some(Path::new("/home/dev/.agents/skills/grilling/SKILL.md").to_path_buf())
        );
    }

    // A crafted archive is the one input this tool cannot vet by hand, so the traversal cases
    // below are the ones that decide whether restoring a shared archive is safe at all.

    #[test]
    fn an_ordinary_archive_path_is_accepted() {
        for path in [
            "claude/settings.json",
            "claude/hooks/load-repo-rules.mjs",
            "agents/skills/grilling/SKILL.md",
            "claude/projects/c--Users-Ehsan-Desktop-app/memory/MEMORY.md",
        ] {
            assert!(check_safe(path).is_ok(), "{path}");
        }
    }

    #[test]
    fn a_path_climbing_out_of_the_restore_roots_is_refused() {
        for path in [
            "claude/../../.bashrc",
            "claude/../.ssh/authorized_keys",
            r"claude\..\..\.bashrc",
            "claude/hooks/../../../evil.mjs",
            "../evil",
        ] {
            assert!(check_safe(path).is_err(), "{path}");
        }
    }

    #[test]
    fn an_absolute_or_drive_qualified_path_is_refused() {
        for path in [
            "claude//etc/cron.d/evil",
            "claude/C:/Windows/System32/evil.dll",
            r"claude/C:\Windows\evil.dll",
            "claude/./settings.json",
        ] {
            assert!(check_safe(path).is_err(), "{path}");
        }
    }

    #[test]
    fn only_the_two_skill_link_shapes_are_links_this_tool_handles() {
        assert!(is_skill_link("claude/skills/k", "agents/skills/k"));
        assert!(is_skill_link("claude/skills/k", "agents/skills/other"));
        assert!(is_skill_link("claude/skills", "agents/skills"));
        for (link, target) in [
            ("claude/skills/k", "agents/skills"),
            ("claude/skills/k", "agents/skills/k/sub"),
            ("claude/skills/k", "agents"),
            ("claude/skills/k", "claude"),
            ("claude/skills/k", "claude/projects"),
            ("claude/skills/k", "claude/.credentials.json"),
            ("claude/skills/k/sub", "agents/skills/k"),
            ("claude/hooks/k", "agents/skills/k"),
            ("agents/skills/k", "agents/skills/j"),
            ("claude/skills", "agents/skills/k"),
            ("claude/skills/k", "agents/skills/.git"),
            ("claude/skills/node_modules", "agents/skills/k"),
        ] {
            assert!(!is_skill_link(link, target), "{link} -> {target}");
        }
    }

    #[test]
    fn a_link_target_is_named_from_its_spelling_alone() {
        let home = Path::new("/home/dev");
        let link = Path::new("/home/dev/.claude/skills/k");
        for (target, named) in [
            ("/home/dev/.agents/skills/k", Some("agents/skills/k")),
            ("../../.agents/skills/k", Some("agents/skills/k")),
            (
                "/home/dev/.claude/skills/a/../../../.agents/skills/k",
                Some("agents/skills/k"),
            ),
            ("/home/dev/.claude/skills/a/../..", None),
            ("/etc", None),
            ("../../../elsewhere", None),
        ] {
            assert_eq!(
                named_archive_path(home, link, target).as_deref(),
                named,
                "{target}"
            );
        }
    }

    #[test]
    fn a_name_a_filesystem_rejects_or_aliases_is_refused() {
        let long = format!("claude/hooks/{}", "a".repeat(256));
        for path in ["claude/hooks/a\0b", "claude/C:x", long.as_str()] {
            assert!(check_safe(path).is_err(), "{path}");
        }
        // Aliases only on Windows; elsewhere these are ordinary names a backup carries.
        for path in [
            "claude/settings.json.",
            "claude/settings.json ",
            "claude/settings.json::$DATA",
            "claude/hooks/CON",
            "claude/hooks/nul.txt",
            "claude/hooks/com1.js",
            "claude/hooks/notes 10:00.md",
        ] {
            assert_eq!(check_safe(path).is_err(), cfg!(windows), "{path}");
        }
        assert!(check_safe("claude/hooks/console.js").is_ok());
        assert!(check_safe("claude/hooks/com.js").is_ok());
    }

    #[test]
    fn an_unknown_archive_prefix_maps_nowhere() {
        assert_eq!(target_path(Path::new("/home/dev"), "etc/passwd"), None);
        assert_eq!(target_path(Path::new("/home/dev"), "manifest.json"), None);
    }

    #[test]
    fn every_string_in_a_json_tree_is_transformed_and_key_order_is_kept() {
        let value = serde_json::json!({
            "z": "a", "a": ["b", { "c": "d" }], "n": 1, "t": true
        });
        let shouted = map_strings(&value, &|s| s.to_uppercase());
        assert_eq!(shouted["z"], serde_json::json!("A"));
        assert_eq!(shouted["a"], serde_json::json!(["B", { "c": "D" }]));
        assert_eq!(shouted["n"], serde_json::json!(1));
        assert_eq!(shouted["t"], serde_json::json!(true));
        let keys: Vec<&String> = shouted.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["z", "a", "n", "t"]);
    }

    // Regression: machine B's own settings.json spells its hook path with backslashes, the
    // restored value spells it with forward slashes, and the array union appended both, so the
    // stale entry stayed listed and firing.
    #[test]
    fn a_local_backslash_path_canonicalizes_onto_the_restored_spelling() {
        let ctx = PathContext {
            home: "C:/Users/Ehsan".into(),
            node: "C:/Program Files/nodejs/node.exe".into(),
            real_home: None,
        };
        let local = serde_json::json!({
            "hooks": [r"C:\Users\Ehsan\.claude\hooks\x.mjs"],
            "permissions": ["Bash(npm view:*)", r"a\b\(c\)*?"]
        });
        let canonical = canonicalize(&local, &ctx);

        assert_eq!(canonical["hooks"][0], "C:/Users/Ehsan/.claude/hooks/x.mjs");
        // Values holding no path must survive byte for byte, escapes included.
        assert_eq!(canonical["permissions"][0], "Bash(npm view:*)");
        assert_eq!(canonical["permissions"][1], r"a\b\(c\)*?");
    }

    #[test]
    fn canonicalizing_twice_changes_nothing_further() {
        let ctx = PathContext {
            home: "/home/dev".into(),
            node: "/usr/bin/node".into(),
            real_home: None,
        };
        let value = serde_json::json!({ "a": "/home/dev/.claude/hooks/x.mjs", "b": "plain" });
        let once = canonicalize(&value, &ctx);
        assert_eq!(canonicalize(&once, &ctx), once);
    }
}
