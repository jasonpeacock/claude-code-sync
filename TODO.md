# TODO

Issues found by an adversarial review of the source against README.md (2026-10-28). Each item
was checked by three independent skeptics. Items marked *(unconfirmed)* were upheld by only one
of the three.

Mark an item `[x]` when it is fixed, and note how.

## Restore safety (untrusted archive)

- [x] **S1. Link targets are never validated.** `vet()` checks only `link.path`. A crafted archive
  can plant `claude/skills/k -> ${HOME}/.ssh` or any absolute path. (`src/app.rs` vet, restore
  link loop)
  Fixed: `vet` resolves each link target (relative ones against the link's directory) and refuses any outside the roots.
  Revised after review: an outside target is now skipped and reported instead of refusing the archive, so older archives still restore. A target that climbs `..` out through another of the archive's links is refused (`resolve_real` follows real links rather than collapsing `..` by spelling).
- [x] **S2. Chained links write outside the roots.** After S1 plants a link, a later entry or link
  under it (`claude/skills/k/authorized_keys`) passes `check_safe` and is written through the link.
  The same applies to links that already exist on disk.
  Fixed: `vet` refuses any entry or link placed under one of the archive's own links; `check_lands_inside` covers links already on disk.
- [x] **S3. `fs::write` follows symlinks already on disk.** If `~/.claude/CLAUDE.md` links into
  `~/dotfiles`, restoring that file overwrites the file outside the roots. (`src/app.rs` restore)
  Fixed: `check_lands_inside` canonicalizes the deepest existing part of every destination and refuses one that resolves outside the roots. Note: a stow-style `~/.claude/CLAUDE.md -> ~/dotfiles/...` on the target now stops the restore.
  Revised after review: such files are now skipped and reported, not a whole-restore failure, so a stow user's same-machine round trip works.
- [x] **S4. Copy fallback copies anything.** When a link cannot be created (Windows without
  developer mode), `copy_tree` copies whatever the unchecked target names into `~/.claude`, so
  SSH keys or credentials end up in the next backup. It also has no cycle guard.
  Fixed: the copy fallback only copies from a source that really resolves inside the roots, and skips nested links (no cycles).
- [x] **S5. Pre-restore snapshot is incomplete.** It always uses `include_credentials: false` and
  the target machine's own include list, so an archive made with `--include-credentials` or with
  extra include paths overwrites files the snapshot never captured. (`src/main.rs` restore)
  Fixed: `snapshot_options` adds every manifest path to the snapshot's extras and carries credentials when the archive does. The snapshot is written `0600`, and only after a full dry run has vetted the archive.
- [x] **S6. No decompression size limit.** A zip bomb exhausts memory, even under `--dry-run`.
  (`src/archive.rs` read_zip)
  Fixed: `read_zip` caps entries at 64 MB and the archive at 512 MB.
- [x] **S7. Entries outside `claude/` and `agents/` are skipped silently** instead of stopping the
  restore, contrary to README:44. (`src/app.rs` restore)
  Fixed: `vet` refuses entries and links outside `claude/` and `agents/`.
- [x] **S8. Half-applied restores.** Failures after `vet()` (invalid archived `settings.json`, a
  corrupt or BOM-prefixed local `settings.json`, IO errors) leave a partly written config. A corrupt
  local `settings.json` makes every restore fail, even `--merge=replace`.
  Mostly fixed: restore plans every write, including both sides of the `settings.json` merge, before writing anything, and `--merge=replace` no longer parses local settings. An IO error partway through can still leave a partial restore (the snapshot covers it).
  Revised after review: the snapshot no longer fails on an unparseable local `settings.json`; it is carried as is.
- [x] **S9. Restored files are created with default permissions**, so a restored
  `.credentials.json`, and archives that hold credentials, are usually world-readable.
  Fixed on Unix: restored `.credentials.json`, credential-bearing archives and snapshots are written `0600`.
- [x] **S10. Manifest hash check authenticates nothing.** Whoever appends a file can rewrite
  `manifest.json` too, so README's "appending a file fails loudly" only stops naive tampering.
  `vet` also never checks that every listed file is present or that `size` matches.
  Partly fixed: `vet` now refuses a manifest listing a file the archive lacks. README now says the manifest is unsigned. Signing is not planned.
- [x] **S11. Dry-run output prints archive-controlled names unescaped**, so terminal control
  sequences can hide planned actions. (`src/main.rs`)
  Fixed: action lines and conflict keys are printed with control characters escaped.
- [x] **S12. An existing dangling link at a link path** breaks the restore or writes through to
  the old target.
  Fixed: anything already at a link path, a dangling link included, is left alone (`symlink_metadata`, not `exists`).
- [x] **S13. Existing links are never updated** to a new target. (Relative targets are now resolved
  against the link's directory for every check and for the copy fallback.)
  Superseded by the simplified link policy: the code it describes is gone.
- [ ] **S14.** *(unconfirmed)* An archive can add hooks that run arbitrary commands, even under
  `--merge=existing`, and `--dry-run` does not show the settings change.

## Backup correctness

- [x] **B1. Symlinked config loses its content.** Links pointing anywhere other than `~/.agents`
  (stow or chezmoi dotfiles) are stored as pointers only and restore as dangling links. A linked
  `settings.json` is neither path-rewritten nor merged. (`src/app.rs` walk)
  Fixed: a link pointing outside the roots is followed on backup and its content is carried as files, with a cycle guard. A dangling one is skipped with a message.
- [x] **B2. A symlinked top-level directory** (`~/.claude/skills -> ...`, also hooks/, commands/,
  agents/) is classified as a plain file, excluded, and dropped with no message.
  (`src/classify.rs`)
  Fixed: a directory link inside the roots is recorded when its directory would be walked; one pointing outside is followed (see B1).
- [x] **B3. Executable bits are lost** on backup and restore, so directly invoked hook and skill
  scripts fail with "Permission denied". (`src/archive.rs`, `src/app.rs`)
  Fixed: `FileRecord.executable` is recorded on backup and applied on restore (Unix).
- [ ] **B4. The whole `~/.agents` tree is carried**, not just `skills/` and `.skill-lock.json`,
  and its walk is never pruned. (`src/classify.rs`) Partly addressed: `.git`, `node_modules`,
  `.venv` and `__pycache__` are no longer carried anywhere.
- [ ] **B5. Only the exact name `.credentials.json` is a secret.** The root-dotfile catch-all
  carries copies or similar secret-bearing dotfiles. (`src/classify.rs`) Partly fixed: a
  `.credentials.json` at any depth, in any case, is now a secret.
- [x] **B6. A missing `~/.claude`, or a wrong `CLAUDE_SYNC_HOME`,** writes an empty archive and
  exits 0.
  Fixed for backup: a missing `~/.claude` is an error. (Snapshots still allow it.)
- [ ] **B7. `CLAUDE_CONFIG_DIR` is ignored**, so users who relocate their config get an empty
  backup and a restore to the wrong place.
- [ ] **B8. `--with-memory` restores per-project memory under the source machine's encoded
  project path**, where Claude Code on the target never looks.
- [ ] **B9. A planted `.claude-sync-include` silently widens every later backup.**
- [x] **B10. The `.zip is never carried` rule is case-sensitive.** (`src/classify.rs`)
  Fixed (V35).
- [x] **B11. A `settings.json` that starts with a UTF-8 BOM** makes backup and restore fail.
  Fixed: `strip_bom` before every parse.
- [ ] **B12. Manifest records the archive format version, not the tool version.**
- [x] **B13. `rules/`, `statuslines/` and `themes/` are not carried.** They are authored config, and
  `settings.json` can reference a statusline script by path, so the restored statusline breaks.
  Found on a real `~/.claude` (2026-10-28). (`src/classify.rs` PORTABLE_DIRS)
  Fixed: carried as authored directories.
- [x] **B14. The root-dotfile rule carries stale temp files** such as `.settings.json.tmp-<pid>-0`
  (a truncated settings write), and restore recreates them on the target. (`src/classify.rs`)
  Fixed: `*.tmp`, `*.tmp-*`, `*.swp`, `*.swo` and this tool's own `.ccs-` temp files are never carried.

## Path rewriting

- [x] **P1. Only `settings.json` is rewritten.** `settings.local.json`, `installed_plugins.json`
  and `known_marketplaces.json` keep the source machine's absolute paths and overwrite the local
  files whole. (`src/app.rs` SETTINGS)
  Fixed: `settings.local.json`, `installed_plugins.json` and `known_marketplaces.json` are tokenized on backup and respelled on restore (`TOKENIZED`). They still replace the local copy rather than merge.
- [ ] **P2. `${NODE}`/`${HOME}` are expanded without quoting**, so a Unix hook restored on Windows
  splits at the space in `C:/Program Files`. (`src/paths.rs` resolve_path)
- [x] **P3. Prefix matching has no left boundary**, so the home or node prefix is replaced inside
  unrelated longer paths. (`src/paths.rs`)
  Seen on a real config: with `$HOME=/home/u` reached via `/home/u -> /local/home/u`,
  `/local/home/u/.aim/cc-plugins` became `/local${HOME}/.aim/cc-plugins`. Tokenizing should try
  the canonical home spelling too.
  Fixed: a left boundary is required, and the resolved spelling of home (`PathContext.real_home`) is tokenized too.
- [ ] **P4. Fallback `node = "node"` tokenizes every bare word "node"** into `${NODE}` when node
  is not on PATH. (`src/app.rs` find_node)
- [ ] **P5. Windows homes in Claude Code's POSIX form** (`/c/Users/...`, `//c/Users/...`) are
  never tokenized. (`src/paths.rs`)
- [ ] **P6. Right-boundary set misses `) : ; ,`**, so a home path at the end of a permission rule
  is not tokenized. (`src/paths.rs`)
- [ ] **P7. A literal `${HOME}`/`${NODE}` the user wrote** is expanded on every restore, even in
  the target machine's own values.
- [ ] **P8. Home of `/` or a drive root** collapses to `""` or `C:`, so either nothing or every
  path on the drive is tokenized.
- [ ] **P9. Normalization stops at the first space**, so backslashes after a space survive and
  the canonicalize dedupe fails. (`src/paths.rs`)
- [ ] **P10. Only the first node on PATH is tokenized**; other node paths keep the source
  machine's absolute path.

## Merge

- [x] **M1. Arrays of objects (hooks) are unioned by whole value**, so a changed hook is
  duplicated instead of replaced. (`src/merge.rs`)
  Fixed: array items with the same `command`, or hook groups with the same `matcher`, are one entry and merge recursively under the chosen strategy.
- [ ] **M2. Removals never carry across**, so a revoked allow rule comes back.
- [x] **M3. `--merge=ask` takes the archive's value** on any reply not starting with "l",
  including Enter and end-of-input. (`src/main.rs` ask_for)
  Fixed: only `l` or `a` decides; Enter re-asks; end of input stops the restore before anything is written.
- [ ] **M4. Ask answers are matched by dotted path**, which is ambiguous when a key contains a
  dot.
- [ ] **M5. On case-insensitive filesystems a differently-cased `settings.json`** skips the merge
  and replaces local settings wholesale.
- [x] **M6. The pre-restore snapshot cannot roll a restore back**, because restoring it is itself
  a merge.
  Fixed: `rollback` restores a snapshot with `--merge=replace` and removes what the restore created, which snapshots now record (`Manifest.created`). It takes its own snapshot, so it can be undone.

## Line endings

- [x] **E1. Every text file becomes CRLF on Windows**, which breaks shell hook scripts and
  changes LF files on a same-machine round trip. (`src/eol.rs`)
  Fixed: `FileRecord.crlf` records CRLF sources; only those are written CRLF, and only on Windows.
- [ ] **E2. Binary detection only checks the first 8000 bytes for a NUL byte**, so small or
  text-headed binaries are treated as text and corrupted. (`src/eol.rs`)
- [ ] **E3. Lone CR bytes are silently turned into LF.** (`src/eol.rs`)

## CLI

- [x] **C1. `--dry-run=true` runs a real restore.** `--x=value` is stored as a value, so the
  boolean flag is never set. (`src/main.rs` Args::parse)
  Fixed: `Args::check` refuses a value on a boolean flag.
- [x] **C2. Unknown flags are ignored silently**, so a typo of `--dry-run` performs a real
  restore.
  Fixed: `Args::check` refuses unknown options per command.
- [x] **C3. Failed link recreation is reported as applied** and the command exits 0.
  Fixed: a link that can be neither created nor copied is reported as `failed` and the command exits 1.
- [x] **C4. A dangling `--out` or `--out=`** silently writes to the current directory; `--out`
  swallows the next flag.
  Fixed: an empty value, or one starting with `-`, is refused.
- [x] **C5. Archive and snapshot names are only unique to the second.** (An existing file is no
  longer overwritten: `write_new` fails instead.)
  Fixed: a name already taken gets `-2`, `-3`, and so on; snapshots are ordered by stamp, then attempt.
- [ ] **C6. Parser gaps:** first value wins. (Extra positionals are now refused; `--` now ends
  options.)

## Docs and tests

- [x] **D1. README's test count is wrong.** (README.md:228)
  Fixed: README says 76 tests.
- [x] **D2. No test creates a symlink**, so the riskiest restore path is untested.
  Fixed: `tests/restore_safety.rs` covers links, crafted archives, exec bits and the snapshot (Unix).

## Found reviewing the fixes (2026-10-28)

A second adversarial review of the S/B/C fixes above, with reproductions.

- [x] **V1. Duplicate link paths made the copy fallback write through the first link**, outside
  the roots. Fixed: `vet` refuses any path named twice across entries and links, case ignored.
- [x] **V2. Link targets were checked lexically**, so `claude/x -> ${HOME}/.claude/a/..` escaped
  once the archive's own `claude/a` existed. Fixed: `resolve_real`, plus a re-check of every link
  against the filesystem just before it is made.
- [x] **V3. Links were planned before files were written**, so with `~/.claude/skills ->
  ~/.agents/skills` on the target the copy fallback copied `SKILL.md` onto itself and emptied it.
  Fixed: re-check before linking; never copy into or out of itself.
- [x] **V4. Real-path link targets were refused when home is reached through a symlink** and the
  target did not exist yet. Fixed: `resolve_real` canonicalizes the existing prefix.
- [x] **V5. A stow user's same-machine round trip aborted the whole restore.** Fixed: skip and
  report (see S3).
- [x] **V6. Followed links carried `.git`, `node_modules`, or all of home.** Fixed: those are not
  descended in followed trees, and a link to home or a root's ancestor is not followed.
- [x] **V7. Older archives with any outside link were refused whole.** Fixed: skipped (see S1).
- [x] **V8. The cycle guard dropped a second alias of the same directory.** Fixed: cycles are
  detected along the current path only.
- [x] **V9. `copy_tree` into its own source recursed until the path was too long.** Fixed: refused.
- [x] **V10. An unreadable directory behind a followed link failed every backup and snapshot.**
  Fixed: skipped with a message.
- [x] **V11. A home spelled with `..` refused writes into a missing `~/.agents`.** Fixed by V4.
- [x] **V12. `printable` missed bidi and format characters, and errors printed archive paths raw.**
  Fixed.
- [x] **V14. The snapshot walked the whole tree**, so it tripped the size caps on files the
  archive never touches, followed links, and left out `.zip` files and anything classify excludes.
  Fixed: `snapshot()` reads exactly the paths the archive names, and nothing else.
- [x] **V15. Archive `.zip` entries could overwrite earlier pre-restore snapshots.** Fixed: `vet`
  refuses any `.zip` entry.
- [x] **V16. A differently cased path on macOS/Windows overwrote files the snapshot missed**,
  including credentials. Fixed by V14 (the snapshot reads through the archive's spelling) and
  case-insensitive credentials checks.
- [x] **V17. The duplicate-path check folded case on Linux**, refusing legitimate archives. Fixed:
  case is folded only on macOS and Windows.
- [x] **V18. A link inside the roots to another link was judged by its final target**, so venv
  interpreter links were carried as binary copies. Fixed: `resolve_link_target` judges the
  immediate target. `.git`, `node_modules`, `.venv` and `__pycache__` are never carried.
- [x] **V19. A link inside the roots to a corner that is not carried** (`plugins/marketplaces`)
  was stored as a bare pointer and lost its content. Revised by V54: such links are now reported
  and not carried, since following one into `claude/projects` carries transcripts.
- [x] **V20. Unreadable files, sockets and FIFOs behind followed links** failed or hung backup.
  Fixed: only regular files are carried; read errors behind followed links are skipped.
- [x] **V21. A link through another link created later in the same restore** could resolve
  outside the roots. Fixed: such links are skipped ("passes through another link").
- [x] **V22. Snapshot misses files written through an in-root symlink.** Fixed by V14.
- [x] **V23. Vetting touched network paths (UNC/NTLM on Windows).** Fixed: `resolve_real` refuses
  non-disk prefixes without touching them.
- [x] **V24. Quadratic extras matching in the snapshot.** Fixed by V14.
- [x] **V25. `--merge=ask` printed archive values unescaped.** Fixed.
- [x] **V26. A relative home disabled symlink resolution.** Fixed: `home_dir` is always absolute.
- [x] **V27. No test reached the copy fallback.** Fixed: `copy_instead` is unit-tested directly.
- [x] **V28. Windows `create_link` judged a relative target against the CWD.** Fixed.
- [x] **V29. `--out=-dir` was refused and `--` was an unknown option.** Fixed.
- [x] **V30. Writes truncated hard-linked files in place.** Fixed: `replace_file` writes beside
  the destination and renames over it, keeping its permissions.
- [x] **V31. The copy fallback could copy `.credentials.json` under a carried name** (Windows
  without developer mode). Fixed: a link is only made to a target a backup carries in its own
  right (`carried_target`).
- [x] **V32. A shared-skills target with a new skill's relative `..` link refused the whole
  archive.** Fixed: the static `..` refusal is gone. Links are made in dependency order, each once
  every archive link along its target exists, and judged against the real filesystem then.
- [x] **V33. The `..` and pending-link guards compared case exactly** on macOS/Windows. Fixed:
  `under()` folds case there, and V32 removes the guessing the guard existed for.
- [x] **V34. A chain of archive links (a -> b -> real) skipped `a`.** Fixed by V32.
- [x] **V35. `.ZIP` was carried by backup and refused by restore.** Fixed: both ignore case.
- [x] **V36. A large file behind a followed link failed the whole backup.** Fixed: skipped with a
  message.
- [x] **V37. A snapshot entry over 64 MB cannot be restored by the tool.** Mitigated: the snapshot
  keeps it and a warning says to unzip it by hand.
- [x] **V38. `.git`/`node_modules`/`.venv`/`__pycache__` were dropped even when the include list
  named them.** Fixed.
- [x] **V39. A link that was not made was still reported as applied.** Fixed: its action line is
  rewritten (`skipped ... already present`, or the reason).
- [x] **V40. A dangling in-root symlink was reported as leading outside.** Fixed: reported as
  dangling.
- [x] **V41. `printable` missed U+061C, U+FFF9-FFFB and tag characters.** Fixed.
- [x] **V42. A copy-fallback test case never reached its guard**, and the guard was unreachable.
  Fixed: both removed.
- [x] **V43. A `..` after a missing component was judged by spelling**, so a later link at that
  component turned an accepted link into an escape, and the next backup carried `~/.ssh`. Fixed:
  restore makes each link to the location it vetted, spelled from home with no `..` and no links
  along it. Backup records link targets the same way (`${HOME}/...`). This also settles Windows
  collapsing `..` by spelling.
- [x] **V44. Credentials checks were case-sensitive.** Fixed: `classify_entry` ignores case, and
  `carried_target` judges an existing target by its real on-disk name.
- [x] **V45. A same-machine restore overwrote a plugin checkout through an in-root link.** Fixed:
  a write whose real destination is somewhere backups do not carry is skipped.
- [x] **V46. Backup of an invalid settings.json exited 0** with an archive no restore can apply.
  Fixed: backup fails; only the snapshot carries it as is.
- [x] **V47. Links to system paths were flattened** (binaries copied, trees walked). Fixed: only
  links into the home directory are followed; others are reported as not carried.
- [x] **V48. Rename failed on a bind-mounted file (EBUSY/EXDEV).** Fixed: falls back to writing in
  place when the file has a single link.
- [x] **V49. Relative links inside followed trees were recorded verbatim.** Fixed by V43.
- [x] **V50. Old archives with an uppercase `.ZIP` were refused whole.** Fixed: `.zip` entries are
  skipped and reported.
- [x] **V51. A UNC home silently made everything look outside.** Fixed: `home_dir` fails loudly.
- [x] **V52. `.git`/`node_modules`/`.venv` were dropped without a message.** Fixed.
- [x] **V53. Link vetting read an include list the same restore had just written.** Fixed: read
  before any write.
- [x] **V54. An archive's own `withMemory` flag let a planted link make every later backup carry
  all transcripts.** Fixed: link targets are judged with this machine's options, never the
  archive's flags; a directory target must be carried whole (`carried_whole`); backup never
  follows a link inside the roots.
- [x] **V55. The copy fallback could copy all of `~/.claude`, credentials included, through a
  link to a bare root.** Fixed by V54; `copy_tree` also skips any `.credentials.json`, and backup
  treats one at any depth as a secret.
- [x] **V56. Links to include-list extras were skipped on a fresh machine.** Fixed: a file the
  archive itself carried counts as carried.
- [x] **V57. Links named `*.zip` or under `claude/backups` could be aimed at the next snapshot.**
  Fixed: skipped; archives and snapshots are written with `create_new`, never through a link.
- [x] **V58. A snapshot taken while settings.json was invalid could not be restored.** Fixed:
  `--merge=replace` writes an unparseable settings.json as it is.
- [x] **V59. Relative skill links were restored as absolute.** Fixed: written relative to the
  link's real directory.
- [x] **V60. Overlapping roots (`~/.agents` inside `~/.claude`) skipped every agents/ entry.**
  Fixed: the most specific root wins.
- [x] **V61. Over-long names and Windows aliases (`name.`, `name:stream`, `CON`) passed vetting**
  and could fail or alias after other writes. Fixed in `check_safe`.
- [x] **V62. Backup messages printed on-disk names raw.** Fixed: `printable` moved into the
  library and used for every path printed.
- [x] **V63. The 64 MB per-entry cap refused a large but compressible history.jsonl.** Fixed: only
  the 512 MB archive-wide cap remains; 64 MB now only skips binaries reached through links.
- [x] **V64. Links pointing outside home (dotfiles in `/workplace`) are no longer carried at all.**
  Accepted for now: restore never creates links out of the roots, so the pointer would be skipped
  anyway. Reported on stderr during backup.
  Superseded by the simplified link policy: the code it describes is gone.
- [x] **V65. Links in older archives into corners backups leave out (plugin marketplace skills)**
  are no longer recreated. Accepted: same reason as V54.
  Superseded by the simplified link policy: the code it describes is gone.
- [x] **V13. Backup reads the whole tree followed through outside links**, so a link to a large
  checkout makes a large archive. Backup now fails at the same 64 MB / 512 MB caps restore uses.
  Superseded by the simplified link policy: the code it describes is gone.

## Simplified link policy (2026-10-28)

Six review rounds each found new high-severity holes in general symlink handling, so it was cut
down to the README's use case. Backup records, and restore recreates, only
`claude/skills/<name> -> agents/skills/<name>` and `claude/skills -> agents/skills`. Every other
link is reported and not carried; no link is ever followed. This supersedes the link parts of
S1-S4, B1, B2, V2-V4, V18, V19, V21, V31-V34, V43, V49, V54-V56, V59, V64 and V65: the code they
describe is gone. The deferral loop, the pending-link reasoning, link-following on backup and the
carried-target judgement were all removed.

- [x] **L1. Links other than skill links are not carried** (dotfiles setups, links into
  `~/.agents/commands`, venv interpreter links). Accepted: reported on stderr during backup.
- [x] **L2. The read-side 64 MB per-entry cap from V63 had never been removed.** Fixed now.
- [x] **L3. A relative skill link was spelled from the paths as named**, so with `~/.claude`
  itself a link (stow) it led somewhere else. Fixed: computed between the real directories and
  checked before it is made; absolute if the check fails.
- [x] **L4. A skill link could be made into a `~/.agents/skills` that is itself a link out of the
  roots.** Fixed: the target end must also resolve to exactly its archive path.
- [x] **L5. `.git`, `node_modules`, `.venv` and `__pycache__` passed as skill names**, and the
  copy fallback would carry them under a skill's name. Fixed in `is_skill_link` and `copy_tree`.
- [x] **L6. Windows name rules refused ordinary Linux names** (`aux.py`, `notes 10:00.md`).
  Fixed: applied only on Windows.
- [x] **L7. An entry under another file entry passed the dry run and failed mid-restore.** Fixed
  in `vet`.
- [x] **L8. `--merge=ask` asked about a settings.json the restore then skipped.** Fixed. The summary
  now counts skipped actions separately from applied ones.
- [x] **L9. Backup counted raw rather than packed bytes against the 512 MB cap**, and the snapshot
  had no cap warning. Fixed.
- [ ] **L10. The 512 MB cap now also limits the user's own backups.** Open: could be made
  configurable.
- [ ] **L11. Case-variant names from a Linux archive refuse the whole restore on macOS/Windows.**
  Open: could skip the second spelling instead.

## Installers, release and CI (deferred)

Not being fixed in this fork: we install with `cargo install --path .` and do not ship the
installers or release workflow.

- [ ] **R1. Release action pinned to a mutable tag** while holding `contents:write`, so it can
  swap binaries and `SHA256SUMS` together. (`.github/workflows/release.yml`)
- [ ] **R2. `SHA256SUMS` is unsigned and same-origin as the binary**, so it detects corruption but
  not tampering. (`install.sh`)
- [ ] **R3. `install.sh` installs an unverified binary when no sha256 tool is present**,
  contradicting the README.
- [ ] **R4. `install.sh` escalates with `sudo`** instead of falling back to `~/.local/bin`,
  contradicting "no admin rights".
- [ ] **R5. `install.ps1 -Uninstall` never removes the PATH entry** (`-split` parsed in command
  mode).
- [ ] **R6. `install.ps1 -Uninstall` recursively deletes whatever `-InstallDir` names.**
- [ ] **R7. `install.ps1` rewrites the user PATH through .NET**, expanding `%VAR%` entries and
  storing them as `REG_SZ`.
- [ ] **R8. The documented PowerShell uninstall runs `exit` inside a scriptblock**, closing the
  user's console.
- [ ] **R9. Linux release binaries are built against glibc 2.39** and fail on older distros.
- [ ] **R10. Weak version glob lets the dispatch input inject shell** through
  `${{ steps.resolve.outputs.tag }}`. (`.github/workflows/release.yml`)
- [ ] **R11. Release tag can differ from the binary's version**; only a warning is emitted.
- [ ] **R12. Builds do not pass `--locked`** and there is no dependency audit.
- [ ] **R13. CI tests only on Linux**, so Windows-only code paths never run a test.
- [ ] **R14.** *(unconfirmed)* CI has no workflow-level `permissions` and checkout persists the
  token while cargo runs third-party build scripts.
- [ ] **R15.** *(unconfirmed)* Installer curl and wget fetches do not enforce HTTPS on redirects.
