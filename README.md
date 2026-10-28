# claude-code-sync

**Move your Claude Code setup to another machine without it silently breaking.**

`claude-code-sync backup` packs your `~/.claude` config into a single zip. `claude-code-sync restore` unpacks it on any other machine, on any OS, and rewrites every machine-specific path inside it so your hooks, skills, agents and permissions actually work there.

One binary. No Node, no Python, no runtime, no admin rights.

*An independent tool. Not affiliated with, endorsed by, or sponsored by Anthropic.*

```console
$ claude-code-sync backup
  skipped secret: claude/.credentials.json  (use --include-credentials to carry it)
68 files, 22 links
141.2 KB -> claude-sync-DESKTOP-P9TI8DK-20260819-043203.zip

$ claude-code-sync restore claude-sync-DESKTOP-P9TI8DK-20260819-043203.zip --dry-run
  create  claude/CLAUDE.md
  create  claude/agents/navigator.md
  create  claude/hooks/load-repo-rules.mjs
  update  claude/settings.json
  link    claude/skills/writing-great-skills -> /home/you/.agents/skills/writing-great-skills
  ...
90 planned (dry run, nothing written)
```

## Why not just copy the folder

Because it breaks in two ways that never print an error.

**Hooks point at absolute paths.** Your `settings.json` stores hook commands as full paths, like `"C:/Program Files/nodejs/node.exe" "C:/Users/you/.claude/hooks/my-hook.mjs"`. Copy that to a Mac, or to the same PC under a different username, and the hook is still listed, still enabled, and never fires again. Nothing warns you. You just lose the behavior and have no reason to suspect it.

**Skills are usually symlinks.** If you install skills from a shared source, entries in `~/.claude/skills/` point into somewhere like `~/.agents/skills/`. A plain zip captures the dangling pointer instead of the content, so you unpack a skills directory full of broken links.

This tool stores those paths as `${HOME}` and `${NODE}` tokens and re-expands them against the target machine, records symlinks separately and recreates them, carries the real skill sources they point at, and normalizes line endings so one archive works on Windows, Linux and macOS alike.

## What it does

- **Path rewriting that survives a round trip.** Absolute paths become tokens on backup and real paths again on restore, and running it twice on the same machine produces the identical file rather than a near-duplicate.
- **Skill links preserved, not flattened.** Two link shapes are recorded and recreated: `~/.claude/skills/<name> -> ~/.agents/skills/<name>`, and `~/.claude/skills -> ~/.agents/skills` as a whole. The `~/.agents` source tree is carried alongside. Every other symlink is reported and not carried, neither as a link nor as what it points at. Where the OS refuses to create a link, it falls back to a copy instead of failing.
- **Cross-OS by default.** Archives store LF and are rewritten to the target's native line ending on restore. Binaries are detected and passed through untouched.
- **Merges rather than clobbers.** `settings.json` is deep merged under a strategy you pick, with arrays unioned and key order preserved, so syncing from one machine does not wipe a permission you added on the other.
- **Never overwrites without a snapshot.** Before every restore, each file the archive would overwrite, credentials included, is copied into a pre-restore snapshot, readable only by you.
- **An archive is treated as untrusted input.** Every entry must be listed in the manifest, match the SHA-256 recorded for it, and land inside `~/.claude` or `~/.agents`. Anything else stops the restore before a single file is written. Any link other than the two skill shapes is skipped.
- **Secrets stay put.** `.credentials.json` is refused by default and cannot be pulled in through the user include list.
- **Curated, not everything.** Transcripts, caches and session state are left behind, and so are `.git`, `node_modules`, `.venv` and `__pycache__` directories wherever they sit (unless `.claude-sync-include` names them), so a backup is about 140 KB rather than hundreds of megabytes.

## Commands

```
claude-code-sync backup  [--with-memory] [--include-credentials] [--out DIR]
claude-code-sync restore <archive.zip> [--merge=STRATEGY] [--dry-run]
claude-code-sync --help | --version
```

Both `--flag=value` and `--flag value` are accepted.

### `backup`

Writes `claude-sync-<host>-<YYYYMMDD-HHMMSS>.zip` to the current directory.

| Flag | Effect |
|---|---|
| `--out DIR` | Write the archive to `DIR` instead, creating it if needed |
| `--with-memory` | Also carry `projects/*/memory/`, your distilled per-project memory files |
| `--include-credentials` | Also carry `.credentials.json`. Refused without this flag, and it prints a warning when used. Do not share such an archive |

### `restore`

Unpacks an archive onto this machine, re-expanding every token against this machine's home and node.

| Flag | Effect |
|---|---|
| `--dry-run` | Print every create, update and link without writing anything. Skips the snapshot too |
| `--merge=STRATEGY` | How to reconcile `settings.json`. One of `incoming`, `existing`, `replace`, `ask`. Defaults to `incoming` |

**Run `--dry-run` first.** It shows the exact plan, and it is the cheapest way to confirm an archive is what you think it is.

### Global

| Flag | Effect |
|---|---|
| `--help`, `-h` | Usage summary. Also the default when run with no command |
| `--version`, `-V` | Print the version |

### Environment

| Variable | Effect |
|---|---|
| `CLAUDE_SYNC_HOME` | Treat this directory as home instead of the real one. Use it to test a restore against a throwaway directory before touching your live config |

On Windows the home directory comes from a Win32 known-folder lookup, **not** from `USERPROFILE`, so setting that variable redirects nothing. `CLAUDE_SYNC_HOME` is the only override.

```sh
CLAUDE_SYNC_HOME=/tmp/fake claude-code-sync restore archive.zip
```

## Merge strategies

Only `settings.json` is merged. Every other file is written as-is.

| Strategy | Behavior |
|---|---|
| `incoming` | Deep merge, the archive wins a conflict. **Default** |
| `existing` | Deep merge, this machine wins a conflict |
| `replace` | Discard this machine's settings entirely |
| `ask` | Prompt for each conflicting key, showing both values |

Under every strategy except `replace`, objects merge key by key and arrays are unioned with local order preserved. Only a differing scalar, or a type mismatch, counts as a conflict. Key order is preserved, so your file keeps its shape and new keys are appended rather than the whole thing being reshuffled.

## What travels

| Carried | Why |
|---|---|
| `CLAUDE.md`, `settings.json`, `settings.local.json`, `keybindings.json` | Authored by you |
| `agents/ commands/ hooks/ skills/ output-styles/ tools/ rules/ statuslines/ themes/` | Authored by you |
| `plugins/installed_plugins.json`, `known_marketplaces.json`, `blocklist.json` | Version and commit pins. `settings.json` records only which plugins are on, not which version, so without these a restore drifts to latest |
| Root-level dotfiles such as `.i-have-adhd-always` | Feature markers set by plugins. Tiny, unpredictable, and losing one silently changes behavior |
| `~/.agents/skills/` and `.skill-lock.json` | The real skill sources, when `~/.claude/skills/` symlinks into them |
| `projects/*/memory/` | Only with `--with-memory`. Distilled over time, never regenerates |

| Left behind | Why |
|---|---|
| `projects/*.jsonl` transcripts | Regenerated, and large |
| `*.tmp`, `*.tmp-*`, `*.swp` scratch files | Half-written by editors and tools; restoring one recreates the debris |
| `plugins/cache/ repos/ marketplaces/`, `plugin-catalog-cache.json` | Re-downloaded from the pins above |
| `security/ cache/ file-history/ shell-snapshots/ sessions/ debug/ telemetry/` | Derived state |
| `.last-cleanup`, `.last-update-result.json` | Tool state, not your choices |
| `.credentials.json` | An auth token. Never in an archive that travels, unless you pass `--include-credentials` |
| Any `.zip` | Stops an archive being swept into the next one |

### Carrying something else

The rules above are an allowlist, so anything unrecognised is dropped rather than swallowed. That keeps a future cache directory out of your backups, but it also means a file nobody anticipated gets missed. The escape hatch is `~/.claude/.claude-sync-include`:

```
# one archive-relative path per line, trailing / for a whole subtree
claude/history.jsonl
claude/context-mode/
```

Paths are archive-relative: `claude/…` is `~/.claude`, `agents/…` is `~/.agents`. The list itself is carried, so it travels with your setup. Two rules are checked before this list and cannot be overridden through it: `.credentials.json` is never carried without `--include-credentials`, and no `.zip` is ever carried.

To see exactly what an archive holds, read `manifest.json` inside the zip. It lists every file with its size and SHA-256, plus the tool version, source OS, hostname and timestamp.

## Install

One line. No runtime, no admin rights. Both installers pull the latest release and check the download against its `SHA256SUMS`, refusing to install on a mismatch.

**Windows** (PowerShell):

```powershell
irm https://raw.githubusercontent.com/ehsan18t/claude-code-sync/main/install.ps1 | iex
```

**Linux and macOS**:

```sh
curl -fsSL https://raw.githubusercontent.com/ehsan18t/claude-code-sync/main/install.sh | sh
```

Then open a new terminal and run `claude-code-sync`.

Windows installs to `%LOCALAPPDATA%\Programs\claude-code-sync` and adds it to your user PATH. Linux and macOS install to `/usr/local/bin`, falling back to `~/.local/bin` when that is not writable. Set `BINDIR` to choose somewhere else.

### Manual download

Prebuilt binaries for every target are on the [latest release](https://github.com/ehsan18t/claude-code-sync/releases/latest), alongside a `SHA256SUMS` file to check them against. Download one, rename it to `claude-code-sync`, and put it anywhere on your PATH.

| Target | Asset |
|---|---|
| Windows x86_64 | `claude-code-sync-windows-x86_64.exe` |
| Windows arm64 | `claude-code-sync-windows-arm64.exe` |
| Linux x86_64 | `claude-code-sync-linux-x86_64` |
| Linux arm64 | `claude-code-sync-linux-arm64` |
| macOS arm64 | `claude-code-sync-macos-arm64` |
| macOS x86_64 | `claude-code-sync-macos-x86_64` |

On Linux and macOS, `chmod +x` it first.

### Uninstall

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/ehsan18t/claude-code-sync/main/install.ps1))) -Uninstall
```

```sh
curl -fsSL https://raw.githubusercontent.com/ehsan18t/claude-code-sync/main/install.sh | sh -s -- --uninstall
```

Both remove the binary and the PATH entry. Neither touches your `~/.claude` config or your backups.

### Homebrew

Not available yet. `brew install claude-code-sync` needs the project in homebrew-core, which requires 75 or more stars, forks or watchers. A personal tap works before then, and the formula is ready at [packaging/homebrew/claude-code-sync.rb](packaging/homebrew/claude-code-sync.rb): create a repo named `homebrew-tap`, copy the formula to `Formula/`, fill in the version and checksums from a release's `SHA256SUMS`, and it becomes `brew install ehsan18t/tap/claude-code-sync`. Until then, the `curl` line above works on macOS.

## How it works

### Path rewriting

Only the matched home or node prefix, and the path immediately following it, are rewritten. Everything else in a string is copied through byte for byte, and a backslash counts as a separator only when what follows could start a path segment.

That last rule matters more than it sounds. A permission rule like `Bash(cp "C:/Users/you/app/src/\(payload\)/index.tsx")` has backslashes that are shell escapes, not separators. Converting them produces a different string that no longer dedupes against itself, so every restore appends another near-duplicate entry to `permissions.allow`. Tokenizing is idempotent here, and there is a test that proves it.

### Line endings

The archive always stores LF. On restore, files are rewritten to the target's native ending: CRLF on Windows, LF elsewhere. Binary files are detected by a NUL byte in the first 8 KB and passed through untouched.

### Safety

- Before anything is overwritten, every file the restore will replace is copied to `~/.claude/backups/pre-restore-<stamp>.zip`. Always, not optionally. Files are replaced by writing beside them and renaming, so a hard link is never written through.
- `--dry-run` writes nothing and skips the snapshot. An option a command does not know, `--dry-run=true` included, is refused rather than ignored.
- Hook and script execute bits survive the round trip. A restored `.credentials.json`, and any archive or snapshot that may hold it, is written owner-only.
- Every entry carries a CRC32, verified on read. A corrupt archive fails loudly.
- Credentials are refused by default, with a warning naming the file that was skipped.

An archive can arrive from anywhere, so restore treats one as untrusted input rather than as its own output:

- An entry whose path climbs out of `~/.claude` or `~/.agents` is refused. A zip naming `claude/../../.bashrc` would otherwise be joined straight onto your home directory, so the check runs on the path components, in either separator style, before anything is written.
- An entry absent from `manifest.json`, or whose bytes do not match the SHA-256 recorded there, is refused. The manifest is the authority on what an archive may write, which makes appending a file to someone else's archive fail loudly instead of silently.
- Only the two skill-link shapes are ever created, judged from how the archive spells them, so no target can lead outside `~/.claude` and `~/.agents`, or to anything but a skill directory. A wider rule let one archive plant `claude/skills/k -> ~/.ssh`, or a link into `projects/`, and the next backup carry what it led to.
- An archive that names one path twice (case ignored on macOS and Windows), or puts a path underneath one of its own links, is refused outright.
- A skill link is made only where both ends are exactly where their names say, with no symlink along the way and the skill already present, and it is written relative, as skill installers write it.
- A write that a symlink on this machine would carry into somewhere backups leave alone, such as a plugin checkout, is skipped and reported.
- A write that would pass through a symlink already on disk and out of the two roots is skipped and reported. If `~/.claude/CLAUDE.md` links into a dotfiles checkout on the target, restore leaves it alone rather than overwriting the file it points at.
- An entry outside `claude/` and `agents/`, or any `.zip` entry, stops the restore instead of being skipped. Archives that expand past 512 MB are refused.
- All of these checks, and the `settings.json` merge, run for the whole archive before the first write, so a rejected archive leaves nothing half-applied.

The manifest is not signed: anyone who can modify an archive can rewrite its manifest too. The hash check catches a file appended carelessly, not a deliberate forgery, so only restore archives you trust. A restored `settings.json` can add hooks, which run commands.

## Building from source

```sh
cargo build --release
cargo test
```

83 tests. The pure logic is covered directly: path tokenization, settings merge across all four strategies, line-ending and binary handling, include/exclude classification, archive round-tripping, and the archive-path rules that reject a crafted zip. Filesystem walking and symlink recreation are covered by a real backup-and-restore round trip into a `CLAUDE_SYNC_HOME` directory rather than by mocks, because that is what catches the failures mocks hide.

## License

MIT
