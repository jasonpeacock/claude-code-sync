//! Machine-specific absolute paths become portable tokens, and back again.
//!
//! This is what stops a restored hook from pointing at a directory that does not exist on the
//! target machine, which is the tool's whole reason to exist.

pub const HOME_TOKEN: &str = "${HOME}";
pub const NODE_TOKEN: &str = "${NODE}";

#[derive(Debug, Clone)]
pub struct PathContext {
    /// Absolute home directory, forward-slashed, no trailing slash.
    pub home: String,
    /// Absolute path to the node binary, forward-slashed.
    pub node: String,
    /// The home directory's other spelling, when it is reached through a symlink
    /// (`/home/you -> /local/home/you`). Tools that resolve paths write that one, so both
    /// become `${HOME}`.
    pub real_home: Option<String>,
    /// Where `~/.claude` really is, when `CLAUDE_CONFIG_DIR` moves it. Archives always spell it
    /// `${HOME}/.claude`, so they restore anywhere, onto a moved config or a default one.
    pub claude_dir: Option<String>,
}

/// How an archive spells the config directory, whatever it is on either machine.
pub const CLAUDE_DIR_SPELLING: &str = "${HOME}/.claude";

pub fn forward_slashes(input: &str) -> String {
    input.replace('\\', "/")
}

/// A path inside a larger command string legitimately ends at one of these.
fn is_boundary(byte: u8) -> bool {
    matches!(
        byte,
        b'/' | b'\\' | b'"' | b'\'' | b' ' | b'\t' | b'\r' | b'\n'
    )
}

/// A path inside a larger string legitimately starts after one of these. Anything else, a
/// letter for one, means the match is the tail of a longer path: `/home/you` inside
/// `/local/home/you` is not this home.
fn starts_a_path(previous: Option<u8>) -> bool {
    previous.map_or(true, |byte| {
        !(byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.') || byte >= 0x80)
    })
}

/// A backslash is a path separator only when what follows could start a path segment.
///
/// This is what keeps shell and regex escapes intact. A permission rule containing
/// `src/app/\(payload\)/admin` has backslashes that are escapes, not separators, and blindly
/// converting them corrupts the rule into a different string that no longer dedupes against
/// itself on the next merge.
fn separator_follows(next: Option<char>) -> bool {
    matches!(next, Some(c) if c.is_alphanumeric() || c == '_' || c == '.' || c == '-')
}

/// Returns the rewritten run and how many bytes of `rest` it covered. The run stops at a quote
/// or whitespace, so the remainder of a command string is never touched.
fn normalize_run(rest: &str) -> (String, usize) {
    let mut out = String::new();
    let mut chars = rest.char_indices().peekable();
    let mut end = rest.len();

    while let Some((index, character)) = chars.next() {
        if matches!(character, '"' | '\'' | ' ' | '\t' | '\r' | '\n') {
            end = index;
            break;
        }
        if character == '\\' {
            let next = chars.peek().map(|(_, c)| *c);
            out.push(if separator_follows(next) { '/' } else { '\\' });
        } else {
            out.push(character);
        }
    }

    (out, end)
}

/// Replace occurrences of `needle` that end at a path boundary, in either separator style.
///
/// Only the matched prefix and the path immediately after it are rewritten. Everything else in
/// the string is copied through byte for byte, which is what makes this safe to run over
/// arbitrary settings values rather than only over paths.
fn replace_prefix(input: &str, needle: &str, token: &str) -> String {
    if needle.is_empty() {
        return input.to_string();
    }

    let variants = [forward_slashes(needle), needle.replace('/', "\\")];
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0usize;

    'scan: while cursor < input.len() {
        for variant in &variants {
            let end = cursor + variant.len();
            let matched = input
                .get(cursor..end)
                .is_some_and(|slice| slice.eq_ignore_ascii_case(variant));

            // The boundary check is load-bearing: without it a home of `C:/Users/Ehsan` would
            // also swallow the unrelated `C:/Users/EhsanBackup`.
            let previous = cursor.checked_sub(1).map(|i| input.as_bytes()[i]);
            if matched
                && starts_a_path(previous)
                && input.as_bytes().get(end).map_or(true, |b| is_boundary(*b))
            {
                out.push_str(token);
                let (run, consumed) = normalize_run(&input[end..]);
                out.push_str(&run);
                cursor = end + consumed;
                continue 'scan;
            }
        }

        let character = input[cursor..]
            .chars()
            .next()
            .expect("cursor is on a char boundary");
        out.push(character);
        cursor += character.len_utf8();
    }

    out
}

/// Accepts a bare path or a whole command string containing several.
///
/// CRITICAL: the node binary is substituted first, so a node installed under the home directory
/// still resolves to `${NODE}` rather than to a `${HOME}` path that does not exist on the target.
pub fn tokenize_path(input: &str, ctx: &PathContext) -> String {
    let mut out = replace_prefix(input, &ctx.node, NODE_TOKEN);
    // Before home: a moved config directory may sit inside home, and must not become
    // `${HOME}/somewhere-else`.
    if let Some(claude_dir) = &ctx.claude_dir {
        out = replace_prefix(&out, claude_dir, CLAUDE_DIR_SPELLING);
    }
    if let Some(real_home) = &ctx.real_home {
        out = replace_prefix(&out, real_home, HOME_TOKEN);
    }
    replace_prefix(&out, &ctx.home, HOME_TOKEN)
}

pub fn resolve_path(input: &str, ctx: &PathContext) -> String {
    let input = match &ctx.claude_dir {
        Some(claude_dir) => respell_claude_dir(input, claude_dir),
        None => input.to_string(),
    };
    input
        .replace(NODE_TOKEN, &forward_slashes(&ctx.node))
        .replace(HOME_TOKEN, &forward_slashes(&ctx.home))
}

/// `${HOME}/.claude` spelled as the moved config directory, wherever it ends a path segment.
fn respell_claude_dir(input: &str, claude_dir: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(at) = rest.find(CLAUDE_DIR_SPELLING) {
        let after = &rest[at + CLAUDE_DIR_SPELLING.len()..];
        out.push_str(&rest[..at]);
        if after.bytes().next().map_or(true, is_boundary) {
            out.push_str(&forward_slashes(claude_dir));
        } else {
            out.push_str(CLAUDE_DIR_SPELLING);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// [`resolve_path`] for a shell command line. A token outside quotes whose path expands to
/// one with a space in it is quoted, so it stays one word: `${NODE} ${HOME}/x.mjs` restored onto
/// `C:/Program Files/nodejs/node.exe` must not run `C:/Program` with `Files/...` as its argument.
/// A token already inside quotes, and every other character, is left as it is.
pub fn resolve_command(input: &str, ctx: &PathContext) -> String {
    let mut out = String::with_capacity(input.len());
    let mut quote: Option<char> = None;
    let mut cursor = 0;
    while cursor < input.len() {
        let rest = &input[cursor..];
        if rest.starts_with(NODE_TOKEN) || rest.starts_with(HOME_TOKEN) {
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
                .unwrap_or(rest.len());
            let word = resolve_path(&rest[..end], ctx);
            if quote.is_none() && word.contains(' ') {
                out.push('"');
                out.push_str(&word);
                out.push('"');
            } else {
                out.push_str(&word);
            }
            cursor += end;
            continue;
        }
        let character = rest.chars().next().expect("cursor is on a char boundary");
        match quote {
            None if character == '"' || character == '\'' => quote = Some(character),
            Some(open) if character == open => quote = None,
            _ => {}
        }
        out.push(character);
        cursor += character.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values are hand-written from a real settings.json, not recomputed the way the
    // implementation computes them.

    fn win() -> PathContext {
        PathContext {
            home: "C:/Users/Ehsan".into(),
            node: "C:/Program Files/nodejs/node.exe".into(),
            real_home: None,
            claude_dir: None,
        }
    }

    #[test]
    fn a_moved_config_directory_travels_as_home_dot_claude() {
        let moved = PathContext {
            claude_dir: Some("/home/dev/cfg/claude".into()),
            ..nix()
        };
        assert_eq!(
            tokenize_path("/home/dev/cfg/claude/hooks/x.mjs", &moved),
            "${HOME}/.claude/hooks/x.mjs"
        );
        assert_eq!(
            tokenize_path("/home/dev/cfg/other", &moved),
            "${HOME}/cfg/other"
        );
        assert_eq!(
            resolve_path("${HOME}/.claude/hooks/x.mjs", &moved),
            "/home/dev/cfg/claude/hooks/x.mjs"
        );
        assert_eq!(
            resolve_path("${HOME}/.claudex", &moved),
            "/home/dev/.claudex"
        );
        assert_eq!(
            resolve_path("${HOME}/.claude/hooks/x.mjs", &nix()),
            "/home/dev/.claude/hooks/x.mjs"
        );
    }

    #[test]
    fn a_command_keeps_a_path_with_a_space_as_one_word() {
        let ctx = PathContext {
            home: "C:/Users/Jo Smith".into(),
            ..win()
        };
        assert_eq!(
            resolve_command("${NODE} ${HOME}/.claude/hooks/x.mjs --flag", &ctx),
            r#""C:/Program Files/nodejs/node.exe" "C:/Users/Jo Smith/.claude/hooks/x.mjs" --flag"#
        );
        // Already quoted: left alone, not quoted twice.
        assert_eq!(
            resolve_command(r#""${NODE}" '${HOME}/x'"#, &ctx),
            r#""C:/Program Files/nodejs/node.exe" 'C:/Users/Jo Smith/x'"#
        );
        // No space, no quotes added.
        assert_eq!(
            resolve_command("${NODE} ${HOME}/x", &nix()),
            "/usr/bin/node /home/dev/x"
        );
    }

    #[test]
    fn a_home_inside_a_longer_path_is_not_this_home() {
        let ctx = nix();
        assert_eq!(
            tokenize_path("/local/home/dev/.aim", &ctx),
            "/local/home/dev/.aim"
        );
        assert_eq!(tokenize_path("x/home/dev/a", &ctx), "x/home/dev/a");
        assert_eq!(tokenize_path("HOME=/home/dev/a", &ctx), "HOME=${HOME}/a");
        assert_eq!(
            tokenize_path("file:///home/dev/a", &ctx),
            "file://${HOME}/a"
        );
    }

    #[test]
    fn both_spellings_of_a_symlinked_home_become_the_token() {
        let ctx = PathContext {
            real_home: Some("/local/home/dev".into()),
            ..nix()
        };
        assert_eq!(
            tokenize_path("/local/home/dev/.aim/x", &ctx),
            "${HOME}/.aim/x"
        );
        assert_eq!(
            tokenize_path("/home/dev/.claude/x", &ctx),
            "${HOME}/.claude/x"
        );
        assert_eq!(
            resolve_path(&tokenize_path("/local/home/dev/w", &ctx), &ctx),
            "/home/dev/w"
        );
    }

    fn nix() -> PathContext {
        PathContext {
            home: "/home/dev".into(),
            node: "/usr/bin/node".into(),
            real_home: None,
            claude_dir: None,
        }
    }

    #[test]
    fn replaces_the_home_prefix_with_a_token() {
        assert_eq!(
            tokenize_path("C:/Users/Ehsan/.claude/hooks/load-repo-rules.mjs", &win()),
            "${HOME}/.claude/hooks/load-repo-rules.mjs"
        );
    }

    #[test]
    fn replaces_the_node_binary_with_a_token() {
        assert_eq!(
            tokenize_path("C:/Program Files/nodejs/node.exe", &win()),
            "${NODE}"
        );
    }

    #[test]
    fn normalizes_windows_backslashes() {
        assert_eq!(
            tokenize_path(r"C:\Users\Ehsan\.claude\agents", &win()),
            "${HOME}/.claude/agents"
        );
    }

    #[test]
    fn matches_the_home_prefix_case_insensitively() {
        assert_eq!(
            tokenize_path("c:/users/ehsan/.claude", &win()),
            "${HOME}/.claude"
        );
    }

    #[test]
    fn leaves_unrelated_absolute_paths_untouched() {
        assert_eq!(tokenize_path("/usr/bin/git", &nix()), "/usr/bin/git");
    }

    #[test]
    fn does_not_treat_a_longer_sibling_directory_as_the_home_prefix() {
        assert_eq!(
            tokenize_path("C:/Users/EhsanBackup/notes.md", &win()),
            "C:/Users/EhsanBackup/notes.md"
        );
    }

    #[test]
    fn expands_home_against_the_target_machine() {
        assert_eq!(
            resolve_path("${HOME}/.claude/hooks/load-repo-rules.mjs", &nix()),
            "/home/dev/.claude/hooks/load-repo-rules.mjs"
        );
    }

    #[test]
    fn expands_node_against_the_target_machine() {
        assert_eq!(resolve_path("${NODE}", &nix()), "/usr/bin/node");
    }

    #[test]
    fn expands_every_token_in_a_compound_command_string() {
        assert_eq!(
            resolve_path(r#""${NODE}" "${HOME}/.claude/hooks/x.mjs""#, &win()),
            r#""C:/Program Files/nodejs/node.exe" "C:/Users/Ehsan/.claude/hooks/x.mjs""#
        );
    }

    #[test]
    fn tokenizes_every_path_in_a_compound_command_string() {
        assert_eq!(
            tokenize_path(
                r#""C:/Program Files/nodejs/node.exe" "C:/Users/Ehsan/.claude/hooks/x.mjs""#,
                &win()
            ),
            r#""${NODE}" "${HOME}/.claude/hooks/x.mjs""#
        );
    }

    // The following four cases are regressions from a restore that corrupted a live
    // settings.json by normalizing separators across a whole string rather than only across
    // the path inside it.

    #[test]
    fn shell_escapes_outside_a_path_are_left_intact() {
        let rule = r#"Bash(cp "c:/Users/Ehsan/Desktop/app/src/\(payload\)/admin/index.tsx")"#;
        assert_eq!(
            tokenize_path(rule, &win()),
            r#"Bash(cp "${HOME}/Desktop/app/src/\(payload\)/admin/index.tsx")"#
        );
    }

    #[test]
    fn a_value_containing_no_path_is_returned_byte_for_byte() {
        for value in ["Bash(npm view:*)", "mcp__ctx_search", r"a\b\(c\)*?"] {
            assert_eq!(tokenize_path(value, &win()), value, "{value}");
        }
    }

    #[test]
    fn a_backslash_path_tokenizes_and_survives_the_round_trip_unchanged() {
        let original = r"c:\Users\Ehsan\.claude\projects\c--foo\memory";
        let token = tokenize_path(original, &win());
        assert_eq!(token, "${HOME}/.claude/projects/c--foo/memory");
        // Round-tripping onto the same machine must not produce a second, different entry.
        assert_eq!(tokenize_path(&resolve_path(&token, &win()), &win()), token);
    }

    #[test]
    fn tokenizing_is_idempotent_so_a_repeated_restore_cannot_duplicate_an_entry() {
        let rule = r#"Bash(cp "c:/Users/Ehsan/Desktop/app/\(x\)/i.tsx")"#;
        let once = tokenize_path(rule, &win());
        let twice = tokenize_path(&resolve_path(&once, &win()), &win());
        assert_eq!(once, twice);
    }

    #[test]
    fn a_windows_path_survives_a_round_trip_through_linux_and_back() {
        let original = "C:/Users/Ehsan/.claude/hooks/load-repo-rules.mjs";
        let on_linux = resolve_path(&tokenize_path(original, &win()), &nix());
        assert_eq!(on_linux, "/home/dev/.claude/hooks/load-repo-rules.mjs");
        assert_eq!(
            resolve_path(&tokenize_path(&on_linux, &nix()), &win()),
            original
        );
    }
}
