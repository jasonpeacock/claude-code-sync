//! Combining the target machine's settings.json with the one from the archive.
//!
//! serde_json is built with `preserve_order`, so merging never reshuffles a user's file:
//! existing keys keep their position and new incoming keys are appended.

use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStrategy {
    /// Deep merge, the archive wins a conflict.
    Incoming,
    /// Deep merge, this machine wins a conflict.
    Existing,
    /// Discard this machine's settings entirely.
    Replace,
    /// Prompt for each conflicting key.
    Ask,
}

impl MergeStrategy {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "incoming" => Some(Self::Incoming),
            "existing" => Some(Self::Existing),
            "replace" => Some(Self::Replace),
            "ask" => Some(Self::Ask),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Existing,
    Incoming,
}

#[derive(Debug, Clone)]
pub struct Conflict {
    /// Dotted key path, e.g. `permissions.defaultMode`.
    pub path: String,
    pub existing: Value,
    pub incoming: Value,
}

/// What makes two array items the same entry, when that is not their whole value. A hook entry is
/// its command, and a hook group is its matcher, absent or not. Without this a hook whose timeout
/// changed on one machine is a different value, and the union lists it twice, so it fires twice.
fn identity(item: &Value) -> Option<(&'static str, &Value)> {
    let Value::Object(map) = item else {
        return None;
    };
    if let Some(command) = map.get("command") {
        return Some(("command", command));
    }
    if map.contains_key("hooks") {
        return Some(("matcher", map.get("matcher").unwrap_or(&Value::Null)));
    }
    None
}

/// Local items first, in their order. An incoming item that is the same entry as a local one is
/// merged into it, so a conflict inside it is decided by the strategy like any other; anything
/// else new is appended.
fn union_arrays(
    existing: &[Value],
    incoming: &[Value],
    strategy: MergeStrategy,
    resolve: &mut dyn FnMut(&Conflict) -> Choice,
    path: &str,
) -> Value {
    let mut merged = existing.to_vec();
    for item in incoming {
        if merged.contains(item) {
            continue;
        }
        let same = identity(item).and_then(|wanted| {
            merged
                .iter()
                .position(|candidate| identity(candidate) == Some(wanted))
        });
        match same {
            Some(index) => {
                let child = format!("{path}[{index}]");
                merged[index] = merge_value(&merged[index], item, strategy, resolve, &child);
            }
            None => merged.push(item.clone()),
        }
    }
    Value::Array(merged)
}

fn merge_value(
    existing: &Value,
    incoming: &Value,
    strategy: MergeStrategy,
    resolve: &mut dyn FnMut(&Conflict) -> Choice,
    path: &str,
) -> Value {
    if let (Value::Object(left), Value::Object(right)) = (existing, incoming) {
        let mut merged = Map::new();
        // Existing keys first, in their original order, so the file keeps its shape.
        for (key, left_value) in left {
            let child = child_path(path, key);
            match right.get(key) {
                Some(right_value) => {
                    merged.insert(
                        key.clone(),
                        merge_value(left_value, right_value, strategy, resolve, &child),
                    );
                }
                None => {
                    merged.insert(key.clone(), left_value.clone());
                }
            }
        }
        for (key, right_value) in right {
            if !left.contains_key(key) {
                merged.insert(key.clone(), right_value.clone());
            }
        }
        return Value::Object(merged);
    }

    if let (Value::Array(left), Value::Array(right)) = (existing, incoming) {
        return union_arrays(left, right, strategy, resolve, path);
    }

    // Scalars, and any type mismatch between the two sides, are conflicts.
    if existing == incoming {
        return existing.clone();
    }

    match strategy {
        MergeStrategy::Existing => existing.clone(),
        MergeStrategy::Ask => {
            let conflict = Conflict {
                path: path.to_string(),
                existing: existing.clone(),
                incoming: incoming.clone(),
            };
            match resolve(&conflict) {
                Choice::Existing => existing.clone(),
                Choice::Incoming => incoming.clone(),
            }
        }
        _ => incoming.clone(),
    }
}

fn child_path(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_string()
    } else {
        format!("{parent}.{key}")
    }
}

/// `resolve` is consulted only under [`MergeStrategy::Ask`], and only for genuine conflicts.
pub fn merge_settings(
    existing: &Value,
    incoming: &Value,
    strategy: MergeStrategy,
    resolve: &mut dyn FnMut(&Conflict) -> Choice,
) -> Value {
    if strategy == MergeStrategy::Replace {
        return incoming.clone();
    }
    merge_value(existing, incoming, strategy, resolve, "")
}

/// Collect the conflicts a merge would hit, without deciding any of them.
pub fn find_conflicts(existing: &Value, incoming: &Value) -> Vec<Conflict> {
    let mut found = Vec::new();
    let mut collect = |c: &Conflict| {
        found.push(c.clone());
        Choice::Incoming
    };
    merge_value(existing, incoming, MergeStrategy::Ask, &mut collect, "");
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Shapes mirror a real settings.json: a scalar, a nested object, an array inside it,
    // and a flat map.

    fn local() -> Value {
        json!({
            "model": "sonnet",
            "effortLevel": "high",
            "permissions": { "allow": ["Bash(cargo:*)"], "defaultMode": "auto" },
            "enabledPlugins": { "rust-analyzer-lsp@official": true }
        })
    }

    fn incoming() -> Value {
        json!({
            "model": "opus[1m]",
            "tui": "fullscreen",
            "permissions": { "allow": ["Bash(npm view:*)", "Bash(cargo:*)"] },
            "enabledPlugins": { "superpowers@official": false }
        })
    }

    fn never_asked(_: &Conflict) -> Choice {
        panic!("resolver must not be consulted outside the ask strategy")
    }

    #[test]
    fn replace_discards_the_local_file_entirely() {
        let merged = merge_settings(
            &local(),
            &incoming(),
            MergeStrategy::Replace,
            &mut never_asked,
        );
        assert_eq!(merged, incoming());
    }

    #[test]
    fn incoming_wins_a_scalar_conflict() {
        let merged = merge_settings(
            &local(),
            &incoming(),
            MergeStrategy::Incoming,
            &mut never_asked,
        );
        assert_eq!(merged["model"], json!("opus[1m]"));
    }

    #[test]
    fn existing_wins_a_scalar_conflict() {
        let merged = merge_settings(
            &local(),
            &incoming(),
            MergeStrategy::Existing,
            &mut never_asked,
        );
        assert_eq!(merged["model"], json!("sonnet"));
    }

    #[test]
    fn a_local_only_key_survives_every_merge_strategy() {
        for strategy in [MergeStrategy::Incoming, MergeStrategy::Existing] {
            let merged = merge_settings(&local(), &incoming(), strategy, &mut never_asked);
            assert_eq!(
                merged["effortLevel"],
                json!("high"),
                "lost under {strategy:?}"
            );
            assert_eq!(
                merged["permissions"]["defaultMode"],
                json!("auto"),
                "lost nested under {strategy:?}"
            );
        }
    }

    #[test]
    fn an_incoming_only_key_is_added_under_every_merge_strategy() {
        for strategy in [MergeStrategy::Incoming, MergeStrategy::Existing] {
            let merged = merge_settings(&local(), &incoming(), strategy, &mut never_asked);
            assert_eq!(
                merged["tui"],
                json!("fullscreen"),
                "missing under {strategy:?}"
            );
        }
    }

    #[test]
    fn arrays_are_unioned_with_local_order_kept_and_duplicates_dropped() {
        let merged = merge_settings(
            &local(),
            &incoming(),
            MergeStrategy::Incoming,
            &mut never_asked,
        );
        assert_eq!(
            merged["permissions"]["allow"],
            json!(["Bash(cargo:*)", "Bash(npm view:*)"])
        );
    }

    fn hooks(timeout: u64, command: &str) -> Value {
        json!({ "hooks": { "PreToolUse": [
            { "matcher": "Bash", "hooks": [ { "type": "command", "command": command, "timeout": timeout } ] }
        ] } })
    }

    #[test]
    fn a_changed_hook_merges_into_the_same_hook_rather_than_firing_twice() {
        let merged = merge_settings(
            &hooks(5, "a.sh"),
            &hooks(30, "a.sh"),
            MergeStrategy::Incoming,
            &mut never_asked,
        );
        assert_eq!(merged, hooks(30, "a.sh"));
        let kept = merge_settings(
            &hooks(5, "a.sh"),
            &hooks(30, "a.sh"),
            MergeStrategy::Existing,
            &mut never_asked,
        );
        assert_eq!(kept, hooks(5, "a.sh"));
    }

    #[test]
    fn a_new_hook_joins_its_matchers_group_and_a_new_matcher_is_appended() {
        let merged = merge_settings(
            &hooks(5, "a.sh"),
            &hooks(5, "b.sh"),
            MergeStrategy::Incoming,
            &mut never_asked,
        );
        let group = &merged["hooks"]["PreToolUse"];
        assert_eq!(group.as_array().unwrap().len(), 1);
        assert_eq!(group[0]["hooks"].as_array().unwrap().len(), 2);

        let other = json!({ "hooks": { "PreToolUse": [ { "matcher": "Edit", "hooks": [] } ] } });
        let merged = merge_settings(
            &hooks(5, "a.sh"),
            &other,
            MergeStrategy::Incoming,
            &mut never_asked,
        );
        assert_eq!(merged["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_hook_conflict_is_asked_about_by_its_position() {
        let conflicts = find_conflicts(&hooks(5, "a.sh"), &hooks(30, "a.sh"));
        let paths: Vec<&str> = conflicts.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, vec!["hooks.PreToolUse[0].hooks[0].timeout"]);
    }

    #[test]
    fn nested_maps_merge_key_by_key_rather_than_replacing_the_whole_map() {
        let merged = merge_settings(
            &local(),
            &incoming(),
            MergeStrategy::Incoming,
            &mut never_asked,
        );
        assert_eq!(
            merged["enabledPlugins"],
            json!({ "rust-analyzer-lsp@official": true, "superpowers@official": false })
        );
    }

    #[test]
    fn ask_consults_the_resolver_only_for_genuine_conflicts() {
        let paths: Vec<String> = find_conflicts(&local(), &incoming())
            .into_iter()
            .map(|c| c.path)
            .collect();
        assert_eq!(paths, vec!["model".to_string()]);
    }

    #[test]
    fn ask_honours_a_resolver_that_keeps_the_local_value() {
        let merged = merge_settings(&local(), &incoming(), MergeStrategy::Ask, &mut |_| {
            Choice::Existing
        });
        assert_eq!(merged["model"], json!("sonnet"));
    }

    #[test]
    fn ask_still_adds_non_conflicting_incoming_keys() {
        let merged = merge_settings(&local(), &incoming(), MergeStrategy::Ask, &mut |_| {
            Choice::Existing
        });
        assert_eq!(merged["tui"], json!("fullscreen"));
    }

    #[test]
    fn a_type_mismatch_counts_as_a_conflict() {
        let found = find_conflicts(
            &json!({ "hooks": { "SessionStart": [1, 2] } }),
            &json!({ "hooks": "disabled" }),
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "hooks");
    }

    #[test]
    fn merging_preserves_the_key_order_of_the_local_file() {
        let merged = merge_settings(
            &local(),
            &incoming(),
            MergeStrategy::Incoming,
            &mut never_asked,
        );
        let keys: Vec<&String> = merged.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            vec![
                "model",
                "effortLevel",
                "permissions",
                "enabledPlugins",
                "tui"
            ]
        );
    }
}
