//! S1a StateRepository RED contracts.
//!
//! References:
//! - `.team/artifacts/s1a-state-repository-design.md` section 5 baseline
//!   allowlist and section 8 RED1-RED5.
//!
//! User story: every state write says what semantic intent it has before S1b
//! migrates write clusters. S1a must not move state truth, change schemas, or
//! alter helper behavior; it only adds the repository facade and hardens the
//! direct-save governance gate.

#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/state_save_allowlist.rs"]
mod state_save_allowlist;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use state_save_allowlist::{
    AllowedStateSaveCall, ALLOWED_STATE_SAVE_CALLS, BASELINE_DIRECT_SAVE_COUNT,
};

const REPOSITORY_PATH: &str = "state/repository.rs";
const FORBIDDEN_INTENT_VARIANTS: &[&str] = &[
    "Other",
    "Unknown",
    "RawSave",
    "LegacySave",
    "SaveRuntimeState",
    "SaveTeamScopedState",
];

#[test]
fn red1_baseline_allowlist_classifies_all_current_direct_state_saves() {
    let calls = scan_product_state_saves();
    let allowlist = allowlist_by_key();
    let mut failures = Vec::new();

    if !repo_root()
        .join("crates/team-agent/src")
        .join(REPOSITORY_PATH)
        .exists()
    {
        failures.push(format!(
            "StateRepository skeleton is missing at src/{REPOSITORY_PATH}; S1a allowlist is not actionable without the write facade"
        ));
    }
    if ALLOWED_STATE_SAVE_CALLS.len() != BASELINE_DIRECT_SAVE_COUNT {
        failures.push(format!(
            "contract allowlist row count drifted: expected {BASELINE_DIRECT_SAVE_COUNT}, got {}",
            ALLOWED_STATE_SAVE_CALLS.len()
        ));
    }
    if calls.len() != BASELINE_DIRECT_SAVE_COUNT {
        failures.push(format!(
            "current direct save call count must match G0 baseline {BASELINE_DIRECT_SAVE_COUNT}; got {}",
            calls.len()
        ));
    }

    let mut seen = BTreeSet::new();
    for call in &calls {
        let key = call.key();
        seen.insert(key.clone());
        match allowlist.get(&key).and_then(|rows| rows.first()).copied() {
            Some(allowed) => {
                if allowed.intent.is_empty()
                    || allowed.migration_phase.is_empty()
                    || allowed.reason.is_empty()
                {
                    failures.push(format!(
                        "allowlist row has empty intent/phase/reason for {key}: {allowed:?}"
                    ));
                }
                if is_external_writer(&call.path) && allowed.intent == "repository_internal" {
                    failures.push(format!(
                        "external writer {key} cannot be classified as repository_internal"
                    ));
                }
            }
            None => failures.push(format!(
                "unclassified direct state save: {}:{} fn={} callee={} snippet={}",
                call.path, call.line, call.containing_fn, call.callee_family, call.snippet
            )),
        }
    }

    for (key, rows) in &allowlist {
        let current_count = calls.iter().filter(|call| call.key() == *key).count();
        if current_count != rows.len() {
            let evidence = rows
                .iter()
                .map(|row| row.evidence_line.to_string())
                .collect::<Vec<_>>()
                .join(",");
            failures.push(format!(
                "allowlist key occurrence count drifted: {key} expected={} current={} evidence_lines={evidence}",
                rows.len(),
                current_count
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "RED1: every one of the {BASELINE_DIRECT_SAVE_COUNT} G0 direct state-save callsites must be classified by path+function+callee family, with repository skeleton present.\n{}",
        failures.join("\n")
    );
}

#[test]
fn red2_new_direct_state_saves_are_blocked_outside_repository_or_allowlist() {
    let calls = scan_product_state_saves();
    let allowlist = allowlist_by_key();
    let mut failures = Vec::new();

    if !repo_root()
        .join("crates/team-agent/src")
        .join(REPOSITORY_PATH)
        .exists()
    {
        failures.push(format!(
            "state/repository.rs is missing; new direct-save gate cannot distinguish repository internals from legacy allowlist"
        ));
    }

    let mut line_hints = Vec::new();
    for call in &calls {
        if is_repository_internal_path(&call.path) {
            continue;
        }
        let key = call.key();
        let Some(rows) = allowlist.get(&key) else {
            failures.push(format!(
                "new or unallowlisted direct save is blocked: {}:{} fn={} callee={} snippet={}",
                call.path, call.line, call.containing_fn, call.callee_family, call.snippet
            ));
            continue;
        };
        if !rows.iter().any(|row| row.evidence_line == call.line) {
            let evidence = rows
                .iter()
                .map(|row| row.evidence_line.to_string())
                .collect::<Vec<_>>()
                .join(",");
            line_hints.push(format!(
                "line drift only: {key} evidence_lines={evidence} current_line={}",
                call.line
            ));
        }
    }

    for hint in line_hints {
        println!("RED2_HINT {hint}");
    }

    assert!(
        failures.is_empty(),
        "RED2: new direct save_runtime_state*/save_team_scoped_state* callsites must be blocked unless they are in state/repository.rs or the S1a allowlist key.\n{}",
        failures.join("\n")
    );
}

#[test]
fn red3_state_write_intent_has_no_escape_bucket_and_covers_allowlist_intents() {
    let repository = repository_source_or_panic("RED3");
    let enum_block = block_after(&repository, "enum StateWriteIntent").unwrap_or_else(|| {
        panic!(
            "RED3: repository must declare `StateWriteIntent`; source={}",
            repository
        )
    });
    let normalized_enum = normalize(&enum_block);
    let mut failures = Vec::new();

    for forbidden in FORBIDDEN_INTENT_VARIANTS {
        if normalized_enum.contains(&normalize(forbidden)) {
            failures.push(format!(
                "StateWriteIntent must not expose escape bucket/helper-name variant `{forbidden}`; enum={enum_block}"
            ));
        }
    }
    for intent in required_intents() {
        if !normalized_enum.contains(&normalize(&intent)) {
            failures.push(format!(
                "StateWriteIntent is missing allowlist intent variant `{intent}`"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "RED3: StateWriteIntent must be a closed semantic catalog, with no Other/Unknown/RawSave escape and all non-internal allowlist intents covered.\n{}",
        failures.join("\n")
    );
}

#[test]
fn red4_repository_dispatches_representative_intents_to_existing_helpers() {
    let repository = repository_source_or_panic("RED4");
    let expectations = [
        (
            "ClaimLeader",
            "helper_write_root",
            "ClaimLeader must dispatch to the existing root helper",
        ),
        (
            "CoordinatorTick",
            "save_team_scoped_state",
            "coordinator tick must dispatch to the existing scoped helper",
        ),
        (
            "StopAgent",
            "save_team_scoped_state_with_lifecycle_topology_authority",
            "stop-agent must keep lifecycle topology-authority scoped save behavior",
        ),
        (
            "ResetAgent",
            "save_team_scoped_state_with_tombstone_lifecycle_topology_authority",
            "reset-agent discard-session must keep tombstone topology behavior",
        ),
        (
            "ResultCollection",
            "save_team_scoped_state_reapplying_after_conflict",
            "result collection with a team must keep scoped reapply behavior",
        ),
        (
            "ResultCollection",
            "save_runtime_state_reapplying_after_conflict",
            "result collection without a team must keep root reapply behavior",
        ),
    ];
    let mut failures = Vec::new();
    for (intent, helper, reason) in expectations {
        if !source_mentions_helper_near_intent(&repository, intent, helper) {
            failures.push(format!(
                "{reason}: repository must include intent `{intent}` and helper `{helper}`"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "RED4: S1a repository must be behavior-neutral for representative intents by dispatching to the same legacy helper families.\n{}",
        failures.join("\n")
    );
}

#[test]
fn red5_repository_skeleton_does_not_introduce_schema_or_b3_path_migration() {
    let repository = repository_source_or_panic("RED5");
    let normalized = normalize(&repository);
    let mut failures = Vec::new();

    for forbidden in [
        "schema_version",
        "SCHEMA_VERSION",
        "TeamRuntimePaths",
        "runtime/teams",
        ".team/runtime/teams",
        "canonical per-team",
        "migrate_state",
        "B3",
    ] {
        if normalized.contains(&normalize(forbidden)) {
            failures.push(format!(
                "repository skeleton must not introduce schema/path migration term `{forbidden}`"
            ));
        }
    }
    for required in [
        "StateRepository",
        "load_workspace",
        "load_team",
        "save_reapplying",
    ] {
        if !normalized.contains(&normalize(required)) {
            failures.push(format!(
                "repository skeleton must expose `{required}` without changing schema"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "RED5: S1a is a facade/no-schema-migration slice; repository must expose the skeleton while avoiding B3 path/schema migration terms.\n{}",
        failures.join("\n")
    );
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd)]
struct DirectSaveCall {
    path: String,
    containing_fn: String,
    callee_family: String,
    line: usize,
    snippet: String,
}

impl DirectSaveCall {
    fn key(&self) -> String {
        format!(
            "{}::{}::{}",
            self.path, self.containing_fn, self.callee_family
        )
    }
}

fn scan_product_state_saves() -> Vec<DirectSaveCall> {
    let src = repo_root().join("crates/team-agent/src");
    let mut calls = Vec::new();
    collect_state_save_calls(&src, &mut calls).expect("scan product source state-save callsites");
    calls.sort();
    calls
}

fn collect_state_save_calls(dir: &Path, out: &mut Vec<DirectSaveCall>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if path.components().any(|part| part.as_os_str() == "tests") {
                continue;
            }
            collect_state_save_calls(&path, out)?;
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            scan_file(&path, out)?;
        }
    }
    Ok(())
}

fn scan_file(path: &Path, out: &mut Vec<DirectSaveCall>) -> std::io::Result<()> {
    let text = fs::read_to_string(path)?;
    let relative = path
        .strip_prefix(repo_root().join("crates/team-agent/src"))
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    let mut pending_cfg_test = false;
    let mut skip_test_depth: Option<i32> = None;
    let mut containing_fn = "module".to_string();

    for (index, line) in text.lines().enumerate() {
        let code = line.split("//").next().unwrap_or("").trim();
        if let Some(depth) = skip_test_depth.as_mut() {
            *depth += brace_delta(code);
            if *depth <= 0 {
                skip_test_depth = None;
            }
            continue;
        }
        if code.contains("#[cfg(test)]") {
            pending_cfg_test = true;
            continue;
        }
        if pending_cfg_test {
            if code.contains('{') {
                let depth = brace_delta(code);
                if depth > 0 {
                    skip_test_depth = Some(depth);
                }
                pending_cfg_test = false;
                continue;
            }
            if !code.is_empty() && !code.starts_with("#[") {
                pending_cfg_test = false;
            }
        }
        if let Some(name) = fn_name(code) {
            containing_fn = name.to_string();
        }
        if code.is_empty()
            || code.contains("fn save_runtime_state")
            || code.contains("fn save_team_scoped_state")
        {
            continue;
        }
        if let Some(callee_family) = state_save_callee(code) {
            out.push(DirectSaveCall {
                path: relative.clone(),
                containing_fn: containing_fn.clone(),
                callee_family,
                line: index + 1,
                snippet: code.split_whitespace().collect::<Vec<_>>().join(" "),
            });
        }
    }
    Ok(())
}

fn fn_name(code: &str) -> Option<&str> {
    let marker = "fn ";
    let start = code.find(marker)? + marker.len();
    let rest = &code[start..];
    let len = rest
        .chars()
        .take_while(|ch| *ch == '_' || ch.is_ascii_alphanumeric())
        .map(char::len_utf8)
        .sum::<usize>();
    if len == 0 {
        None
    } else {
        Some(&rest[..len])
    }
}

fn state_save_callee(code: &str) -> Option<String> {
    for prefix in ["save_runtime_state", "save_team_scoped_state"] {
        let mut offset = 0;
        while let Some(found) = code[offset..].find(prefix) {
            let start = offset + found;
            let before_ok =
                start == 0 || !code[..start].chars().next_back().is_some_and(is_ident_char);
            let after_prefix = &code[start + prefix.len()..];
            let token_tail_len = after_prefix
                .chars()
                .take_while(|ch| is_ident_char(*ch))
                .map(char::len_utf8)
                .sum::<usize>();
            let after_token = &after_prefix[token_tail_len..];
            if before_ok && after_token.trim_start().starts_with('(') {
                return Some(format!("{prefix}{}", &after_prefix[..token_tail_len]));
            }
            offset = start + prefix.len();
        }
    }
    None
}

fn is_ident_char(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

fn brace_delta(code: &str) -> i32 {
    let opens = code.chars().filter(|ch| *ch == '{').count() as i32;
    let closes = code.chars().filter(|ch| *ch == '}').count() as i32;
    opens - closes
}

fn allowlist_by_key() -> BTreeMap<String, Vec<&'static AllowedStateSaveCall>> {
    let mut map = BTreeMap::new();
    for row in ALLOWED_STATE_SAVE_CALLS {
        let key = allowed_key(row);
        map.entry(key).or_insert_with(Vec::new).push(row);
    }
    map
}

fn allowed_key(row: &AllowedStateSaveCall) -> String {
    format!("{}::{}::{}", row.path, row.containing_fn, row.callee_family)
}

fn required_intents() -> BTreeSet<String> {
    ALLOWED_STATE_SAVE_CALLS
        .iter()
        .filter_map(|row| {
            if row.intent == "repository_internal" {
                None
            } else {
                Some(row.intent.to_string())
            }
        })
        .collect()
}

fn is_external_writer(path: &str) -> bool {
    !matches!(
        path,
        "state/persist.rs" | "state/projection.rs" | REPOSITORY_PATH
    )
}

fn is_repository_internal_path(path: &str) -> bool {
    matches!(
        path,
        "state/persist.rs" | "state/projection.rs" | REPOSITORY_PATH
    )
}

fn repository_source_or_panic(red: &str) -> String {
    let path = repo_root()
        .join("crates/team-agent/src")
        .join(REPOSITORY_PATH);
    std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{red}: missing StateRepository skeleton at {}; error={error}",
            path.display()
        )
    })
}

fn block_after(source: &str, marker: &str) -> Option<String> {
    let start = source.find(marker)?;
    let after = &source[start..];
    let end = after.find("\n}\n").map(|offset| start + offset + 3)?;
    Some(source[start..end].to_string())
}

fn source_mentions_helper_near_intent(source: &str, intent: &str, helper: &str) -> bool {
    let mut offset = 0;
    while let Some(found) = source[offset..].find(intent) {
        let start = offset + found;
        let end = source.len().min(start + 1400);
        if normalize(&source[start..end]).contains(&normalize(helper)) {
            return true;
        }
        offset = start + intent.len();
    }
    false
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("team-agent crate should live under crates/team-agent")
        .to_path_buf()
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
