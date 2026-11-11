//! Stage 3 dead-code excision gates. P5 = K6 (pre-unified lifecycle seams: legacy
//! `launch` / `quick_start*` / `start_agent*` / `restart*` entry points and the
//! `*_with_transport` wrappers, dead layout planners and lifecycle constants) plus K11
//! (state / model residue). P6 = K9 (the unwired `station/lifecycle.rs` operation
//! journal, its seat-store journal methods and codec helpers) — excised, never wired.
//!
//! `excise_*` cases are RED on the Stage 2 accepted baseline (2801280b) and turn GREEN
//! only once the inventoried code has left the production build. `guard_*` cases are
//! GREEN on the baseline and must stay GREEN: they stop over-deletion of the unified
//! start kernel, live lifecycle/station/state kernels, Stage 2 protected items and
//! every test outside the ruled Stage 3 blast radius.
//!
//! Inventory authority: `fixtures/stage3_excision_manifest.tsv`, produced by a
//! binary-rooted reachability probe (the crate compiled as one bin rooted at `main`,
//! every `allow(dead_code)` stripped, rustc's dead_code lint as the oracle). The same
//! probe runs here (`excise_stage3_scope_leaves_no_binary_unreachable_item`), so the
//! cut must close over the whole dead graph and may not orphan any live item.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const MANIFEST: &str = include_str!("fixtures/stage3_excision_manifest.tsv");
const REACH_BASELINE: &str = include_str!("fixtures/stage3_reach_baseline.tsv");
const CENSUS: &str = include_str!("fixtures/stage3_test_census.tsv");

/// Files whose every item is Stage 3 scope (or whose every test retires with it).
/// Leaving an empty shell module behind is not an excision.
const DEAD_FILES: &[&str] = &[
    // K9: the unwired operation journal and its development suite (P6 YAGNI ruling).
    "src/station/lifecycle.rs",
    "tests/station_lifecycle_dev.rs",
    // K6: layout planners with no production caller.
    "src/layout/manager.rs",
    "src/layout/placement.rs",
    "src/layout/tmux_endpoint.rs",
    // K11: platform error helpers never migrated to (state/persist keeps its own).
    "src/platform/errors.rs",
];

/// K9 vocabulary: no executable reference may remain anywhere in src/ or tests/.
const K9_VOCABULARY: &[&str] = &[
    "NativeSeat",
    "OPERATION_CAPABILITIES",
    "ObservedOutcome",
    "patch_scalar",
    "publish_operation",
    "prune_operations",
    "operation_path",
    "understood_operation",
    "same_intent",
];

/// Path-qualified K9 references (generic leaf names are only dead behind these paths).
const K9_PATHS: &[&str] = &["station::lifecycle", "codec::outcome", "lifecycle::NativeSeat"];

/// `StateWriteIntent` arms that nothing constructs (coordinator tick, messaging
/// turn-arm/session-capture, result collection, task repair, self migration).
/// `CoordinatorConptyShim` is NOT here: the Windows-only coordinator/conpty_shim.rs
/// constructs it, so it is invisible to a macOS reachability probe yet live.
const DEAD_INTENT_ARMS: &[&str] = &[
    "CoordinatorTick",
    "MessagingTurnArm",
    "MessagingSessionCapture",
    "ResultCollection",
    "TaskRepair",
    "SelfMigration",
];

/// Live kernels that sit beside retired seams: `(file, item)`; each must still be
/// declared in that file as fn/struct/enum/trait/const/type.
const LIVE_NEIGHBOURS: &[(&str, &str)] = &[
    // Unified start micro-kernel: one declaration/state decision for preview and execution.
    ("src/lifecycle/seat_start.rs", "start"),
    ("src/lifecycle/seat_start.rs", "resolve"),
    ("src/lifecycle/seat_start.rs", "describe_resolved"),
    ("src/lifecycle/seat_start.rs", "stage_sources"),
    ("src/lifecycle/seat_start.rs", "bootstrap_directory"),
    ("src/lifecycle/seat_start.rs", "command_arg"),
    ("src/lifecycle/seat_start.rs", "StartRequest"),
    ("src/lifecycle/seat_start.rs", "StartDescription"),
    ("src/lifecycle/seat_start.rs", "SessionPolicy"),
    // Live seat kernels the legacy wrappers delegated to.
    ("src/lifecycle/restart/agent.rs", "stop_agent"),
    ("src/lifecycle/restart/agent.rs", "reset_agent"),
    ("src/lifecycle/restart/agent.rs", "start_agent_at_paths"),
    ("src/lifecycle/restart/agent.rs", "start_agent_at_paths_with_session"),
    ("src/lifecycle/restart/agent.rs", "stop_agent_at_paths"),
    ("src/lifecycle/restart/remove.rs", "remove_agent"),
    ("src/lifecycle/restart/remove.rs", "remove_agent_flag_requirements"),
    ("src/lifecycle/restart/remove.rs", "remove_agent_at_paths"),
    ("src/lifecycle/restart/rebuild.rs", "restart_with_session_convergence_deadline"),
    ("src/lifecycle/restart/selection.rs", "classify_restart_plan_with_resume_validation"),
    ("src/lifecycle/launch/quick_start.rs", "quick_start_in_workspace_with_backend"),
    ("src/lifecycle/launch.rs", "launch_with_transport_in_workspace"),
    ("src/lifecycle/launch/add_agent.rs", "add_agent"),
    ("src/lifecycle/launch/clone_agent.rs", "clone_agent"),
    ("src/lifecycle/launch/fork_entry.rs", "fork_agent"),
    ("src/lifecycle/launch/fork_agent.rs", "fork_agent_with_transport"),
    ("src/lifecycle/launch/readiness.rs", "classify_leader_binding"),
    ("src/lifecycle/launch/readiness.rs", "selected_team_leader_receiver"),
    ("src/lifecycle/launch/readiness.rs", "launched_team_receiver_is_attached"),
    ("src/lifecycle/lock.rs", "acquire_seat_lifecycle_lock"),
    ("src/lifecycle/lock.rs", "LifecycleLockRequest"),
    ("src/lifecycle/pane_input_lock.rs", "acquire_or_proceed"),
    ("src/lifecycle/pane_input_lock.rs", "acquire_for_write"),
    ("src/lifecycle/seat_observation.rs", "SeatObservation"),
    // Layout survivors of partially retired files.
    ("src/layout/overlay.rs", "assert_overlay_call_site"),
    ("src/layout/sessions.rs", "worker_session_name"),
    ("src/layout/sessions.rs", "assert_topology_invariants"),
    ("src/layout/runtime_sessions.rs", "RuntimeSessions"),
    ("src/layout/runtime_sessions.rs", "from_state"),
    ("src/leader/start.rs", "leader_session_name"),
    // Station: live engine beside the excised journal.
    ("src/station/codec.rs", "NoEffectReason"),
    ("src/station/codec.rs", "read"),
    ("src/station/codec.rs", "authorize_mutation"),
    ("src/station/codec.rs", "Frame"),
    ("src/station/codec.rs", "frame_checksum"),
    ("src/station/codec.rs", "encode"),
    ("src/station/codec.rs", "decode"),
    ("src/station/seat_store.rs", "SeatScope"),
    ("src/station/seat_store.rs", "SeatManifest"),
    ("src/station/seat_store.rs", "SeatStore"),
    ("src/station/seat_store.rs", "SeatLock"),
    ("src/station/seat_store.rs", "lock_path"),
    ("src/station/seat_store.rs", "publish"),
    ("src/station/seat_store.rs", "component"),
    ("src/station/seat_store.rs", "private_dir"),
    ("src/station/seat_store.rs", "open_file"),
    ("src/station/seat_store.rs", "read_json"),
    ("src/station/seat_store.rs", "write_json"),
    ("src/station/seat_store.rs", "sync_directory"),
    ("src/station/seat_store.rs", "random_instance"),
    // State / model kernels beside the residue.
    ("src/state/repository.rs", "StateRepository"),
    ("src/state/repository.rs", "StateWriteIntent"),
    ("src/state/repository.rs", "commit"),
    ("src/state/repository.rs", "commit_messaging_checked"),
    ("src/state/repository.rs", "commit_fork_agent"),
    ("src/state/paths.rs", "CommandScope"),
    ("src/state/paths.rs", "resolve_readonly"),
    ("src/state/persist.rs", "save_runtime_state"),
    ("src/state/persist.rs", "load_runtime_state"),
    ("src/state/persist.rs", "retryable_replace_error"),
    ("src/state/projection.rs", "save_team_scoped_state"),
    ("src/state/projection.rs", "select_runtime_state"),
    ("src/model/paths.rs", "runtime_dir"),
    ("src/model/paths.rs", "logs_dir"),
    ("src/model/paths.rs", "team_workspace"),
    ("src/importer/migration.rs", "schema_diagnosis"),
    ("src/importer/migration.rs", "fix_schema_layout"),
    ("src/importer/migration.rs", "ensure_table_layout"),
    ("src/event_log.rs", "EventLog"),
];

/// Modules the station root must keep declaring (live business messaging engine).
const LIVE_STATION_MODULES: &[&str] = &["agenda", "codec", "seat_store", "projection", "send"];

/// Items that are unreachable from the binary on macOS yet protected: Stage 2 rulings
/// (handoff §2.5) and Windows-held platform code. Stage 3 may not delete them.
const PROTECTED_UNREACHABLE: &[(&str, &str)] = &[
    ("src/provider/approvals/runtime_prompts.rs", "runtime_approval_decision"),
    ("src/provider/types.rs", "ApprovalFingerprint"),
    ("src/leader/lease.rs", "claim_lease_no_incident"),
    ("src/leader/start.rs", "leader_start_plan_after_ambient_authority_check"),
    ("src/provider/session/capture.rs", "pending_session_capture"),
    ("src/platform/process.rs", "ProcessInfo"),
    ("src/platform/process.rs", "process_snapshot"),
    ("src/platform/process.rs", "TerminationOutcome"),
];

/// Earlier stage acceptance suites stay in place.
const SURVIVOR_FILES: &[&str] = &[
    "tests/stage1_excision_red.rs",
    "tests/stage1_preserve_invariants.rs",
    "tests/fixtures/stage1_excision_manifest.tsv",
    "tests/stage2_excision_red.rs",
    "tests/stage2_preserve_invariants.rs",
    "tests/fixtures/stage2_excision_manifest.tsv",
    "tests/station_codec_dev.rs",
    "tests/unified_start_contract_red.rs",
    "src/lifecycle/restart/claude_fork.rs",
    "src/lifecycle/restart/avatar.rs",
];

// ───────────────────────────── manifest ─────────────────────────────

#[derive(Debug, Clone)]
struct Row {
    cluster: String,
    file: String,
    kind: String,
    name: String,
    rule: String,
    check: String,
}

fn manifest() -> Vec<Row> {
    MANIFEST
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            assert_eq!(f.len(), 7, "malformed manifest row: {line}");
            Row {
                cluster: f[0].into(),
                file: f[1].into(),
                kind: f[3].into(),
                name: f[4].into(),
                rule: f[5].into(),
                check: f[6].into(),
            }
        })
        .collect()
}

// ───────────────────────────── source helpers ─────────────────────────────

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn crate_rust_files(subdirs: &[&str]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for subdir in subdirs {
        rust_files(&crate_root().join(subdir), &mut files);
    }
    files.sort();
    files
}

fn relative(path: &Path) -> String {
    path.strip_prefix(crate_root())
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Blank comments, string/char literal contents and raw strings, keeping newlines so
/// offsets and line numbers survive. Lifetimes and labels stay code.
fn mask_rust(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let blank = |c: char| if c == '\n' { '\n' } else { ' ' };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    out.push_str("  ");
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    out.push_str("  ");
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out.push(blank(chars[i]));
                    i += 1;
                }
            }
        } else if (c == 'r' || (c == 'b' && next == Some('r')))
            && !chars
                .get(i.wrapping_sub(1))
                .is_some_and(|p| p.is_alphanumeric() || *p == '_')
            && {
                let start = if c == 'b' { i + 2 } else { i + 1 };
                let mut j = start;
                while chars.get(j) == Some(&'#') {
                    j += 1;
                }
                chars.get(j) == Some(&'"')
            }
        {
            let mut j = if c == 'b' { i + 2 } else { i + 1 };
            let mut hashes = 0;
            while chars[j] == '#' {
                hashes += 1;
                j += 1;
            }
            j += 1;
            for _ in i..j {
                out.push(' ');
            }
            i = j;
            while i < chars.len() {
                if chars[i] == '"' && (1..=hashes).all(|k| chars.get(i + k) == Some(&'#')) {
                    for _ in 0..=hashes {
                        out.push(' ');
                    }
                    i += hashes + 1;
                    break;
                }
                out.push(blank(chars[i]));
                i += 1;
            }
        } else if c == '"' {
            out.push(' ');
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    out.push(' ');
                    if let Some(escaped) = chars.get(i + 1) {
                        out.push(blank(*escaped));
                    }
                    i += 2;
                    continue;
                }
                if chars[i] == '"' {
                    out.push(' ');
                    i += 1;
                    break;
                }
                out.push(blank(chars[i]));
                i += 1;
            }
        } else if c == '\'' {
            let close = if next == Some('\\') {
                (i + 3..chars.len().min(i + 13)).find(|&j| chars[j] == '\'')
            } else if chars.get(i + 2) == Some(&'\'') {
                Some(i + 2)
            } else {
                None
            };
            match close {
                Some(end) => {
                    for _ in i..=end {
                        out.push(' ');
                    }
                    i = end + 1;
                }
                None => {
                    out.push(c);
                    i += 1;
                }
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn masked(path: &Path) -> String {
    mask_rust(&std::fs::read_to_string(path).unwrap())
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Byte offsets of `word` occurrences delimited by non-identifier characters.
fn word_offsets(text: &str, word: &str) -> Vec<usize> {
    text.match_indices(word)
        .filter(|(at, _)| {
            !text[..*at].chars().next_back().is_some_and(is_ident)
                && !text[at + word.len()..].chars().next().is_some_and(is_ident)
        })
        .map(|(at, _)| at)
        .collect()
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count() + 1
}

/// End offset (inclusive) of the brace block that opens at or after `from`, or the
/// next `;` when the item has no body.
fn item_end(text: &str, from: usize) -> usize {
    let open = text[from..].find('{').map(|at| from + at);
    let semi = text[from..].find(';').map(|at| from + at);
    match (open, semi) {
        (Some(open), Some(semi)) if semi < open => semi,
        (Some(open), _) => {
            let mut depth = 0usize;
            for (offset, c) in text[open..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return open + offset;
                        }
                    }
                    _ => {}
                }
            }
            text.len()
        }
        (None, Some(semi)) => semi,
        (None, None) => text.len(),
    }
}

/// Offsets of `fn|struct|enum|trait|const|static|type NAME` declarations.
fn declaration_offsets(text: &str, name: &str) -> Vec<usize> {
    word_offsets(text, name)
        .into_iter()
        .filter(|at| {
            let before = text[..*at].trim_end();
            ["fn", "struct", "enum", "trait", "const", "static", "type"]
                .iter()
                .any(|keyword| {
                    before.ends_with(keyword)
                        && !before[..before.len() - keyword.len()].ends_with(is_ident)
                })
        })
        .collect()
}

/// A whole file compiled only for tests (test modules, test files, test support).
fn is_test_file(relative: &str) -> bool {
    relative.starts_with("tests/")
        || relative.contains("/tests/")
        || relative.ends_with("/tests.rs")
        || relative.ends_with("_tests.rs")
        || relative.contains("/test_support/")
}

/// Byte ranges of items gated by `#[cfg(test)]` / `#[cfg(all(test, ...))]`.
fn cfg_test_regions(text: &str) -> Vec<(usize, usize)> {
    let mut regions = Vec::new();
    for (at, _) in text.match_indices("#[cfg(") {
        let rest = &text[at + 6..];
        let gated = rest.starts_with("test)") || rest.starts_with("all(test");
        if gated {
            let attr_end = at + text[at..].find(']').unwrap_or(0);
            regions.push((at, item_end(text, attr_end + 1)));
        }
    }
    regions
}

fn in_regions(regions: &[(usize, usize)], offset: usize) -> bool {
    regions.iter().any(|(start, end)| *start <= offset && offset <= *end)
}

struct Source {
    relative: String,
    text: String,
    test_regions: Vec<(usize, usize)>,
}

impl Source {
    fn load(path: &Path) -> Self {
        let text = masked(path);
        let test_regions = cfg_test_regions(&text);
        Self {
            relative: relative(path),
            text,
            test_regions,
        }
    }
    fn is_test_offset(&self, offset: usize) -> bool {
        is_test_file(&self.relative) || in_regions(&self.test_regions, offset)
    }
}

fn sources(subdirs: &[&str]) -> &'static [Source] {
    static SRC: OnceLock<Vec<Source>> = OnceLock::new();
    static ALL: OnceLock<Vec<Source>> = OnceLock::new();
    let cell = if subdirs == ["src"] { &SRC } else { &ALL };
    cell.get_or_init(|| {
        crate_rust_files(subdirs)
            .iter()
            .map(|path| Source::load(path))
            .collect()
    })
}

fn src_sources() -> &'static [Source] {
    sources(&["src"])
}

fn all_sources() -> &'static [Source] {
    sources(&["src", "tests"])
}

/// `file:line` of every declaration of `name` within `files`, optionally skipping
/// declarations compiled only for tests.
fn declarations(name: &str, files: &dyn Fn(&str) -> bool, include_test: bool) -> Vec<String> {
    let mut hits = Vec::new();
    for source in src_sources().iter().filter(|s| files(&s.relative)) {
        for at in declaration_offsets(&source.text, name) {
            if include_test || !source.is_test_offset(at) {
                hits.push(format!("{}:{}", source.relative, line_of(&source.text, at)));
            }
        }
    }
    hits
}

/// Every executable (masked) reference to one of `words` under src/ and tests/.
fn code_references(words: &[&str]) -> Vec<String> {
    let mut hits = Vec::new();
    for source in all_sources() {
        for word in words {
            let occurrences = if word.contains("::") {
                source.text.match_indices(word).map(|(at, _)| at).collect()
            } else {
                word_offsets(&source.text, word)
            };
            for at in occurrences {
                hits.push(format!("{}:{} {word}", source.relative, line_of(&source.text, at)));
            }
        }
    }
    hits
}

fn assert_no_hits(label: &str, hits: &[String]) {
    assert!(
        hits.is_empty(),
        "{label}: {} site(s) remain; first 40:\n{}",
        hits.len(),
        hits.iter().take(40).cloned().collect::<Vec<_>>().join("\n")
    );
}

fn module_tree(file: &str) -> String {
    // src/lifecycle/restart/agent.rs -> src/lifecycle/
    let mut parts = file.splitn(3, '/');
    let first = parts.next().unwrap_or_default();
    let second = parts.next().unwrap_or_default();
    if parts.next().is_some() {
        format!("{first}/{second}/")
    } else {
        file.to_string()
    }
}

fn rows_violating(cluster: &[&str], rule: &str) -> Vec<String> {
    let mut violations = Vec::new();
    for row in manifest()
        .into_iter()
        .filter(|row| cluster.contains(&row.cluster.as_str()) && row.rule == rule)
    {
        let file = row.file.clone();
        let tree = module_tree(&row.file);
        let hits = match (rule, row.check.as_str()) {
            ("ABSENT", "crate") => declarations(&row.name, &|_| true, true),
            ("ABSENT", _) => declarations(&row.name, &|f| f == file, true),
            ("NOT_PRODUCTION", "crate") => declarations(&row.name, &|_| true, false),
            ("NOT_PRODUCTION", _) => declarations(&row.name, &|f| f.starts_with(&tree), false),
            _ => Vec::new(),
        };
        for hit in hits {
            violations.push(format!("{} {} `{}` ({}) declared at {hit}", row.cluster, row.kind, row.name, row.file));
        }
    }
    violations
}

// ───────────────────────────── reachability probe ─────────────────────────────

/// `(file, name)` of every dead_code diagnostic when the crate is compiled as a single
/// binary rooted at `main` with every `allow(dead_code)` removed.
fn reach_probe() -> &'static Result<Vec<(String, String)>, String> {
    static PROBE: OnceLock<Result<Vec<(String, String)>, String>> = OnceLock::new();
    PROBE.get_or_init(run_reach_probe)
}

fn copy_tree(from: &Path, to: &Path, skip: &dyn Fn(&Path) -> bool) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let path = entry.path();
        if skip(&path) {
            continue;
        }
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&path, &target, skip)?;
        } else if entry.file_type()?.is_file() {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

fn strip_dead_code_allowances(dir: &Path) -> std::io::Result<usize> {
    let attr = regex::Regex::new(r"#!?\[(?:allow|expect)\(\s*dead_code\s*\)\]").unwrap();
    let first = regex::Regex::new(r"\bdead_code\s*,\s*").unwrap();
    let later = regex::Regex::new(r",\s*dead_code\b").unwrap();
    let mut files = Vec::new();
    rust_files(dir, &mut files);
    let mut changed = 0;
    for path in files {
        let text = std::fs::read_to_string(&path)?;
        let stripped = later
            .replace_all(&first.replace_all(&attr.replace_all(&text, ""), ""), "")
            .into_owned();
        if stripped != text {
            std::fs::write(&path, stripped)?;
            changed += 1;
        }
    }
    Ok(changed)
}

fn run_reach_probe() -> Result<Vec<(String, String)>, String> {
    let started = std::time::Instant::now();
    let repo = crate_root().join("../..").canonicalize().map_err(|e| e.to_string())?;
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("stage3-reach");
    let tree = work.join("tree");
    let _ = std::fs::remove_dir_all(&tree);
    std::fs::create_dir_all(&tree).map_err(|e| e.to_string())?;
    for file in ["Cargo.toml", "Cargo.lock"] {
        std::fs::copy(repo.join(file), tree.join(file)).map_err(|e| format!("{file}: {e}"))?;
    }
    let crates = repo.join("crates");
    let skip = |path: &Path| {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let parent_is_main = path
            .parent()
            .is_some_and(|p| p.ends_with("crates/team-agent"));
        name == "target" || name == ".git" || (parent_is_main && name == "tests")
    };
    copy_tree(&crates, &tree.join("crates"), &skip).map_err(|e| e.to_string())?;
    // Fixtures that production code embeds with include_str!/include_bytes!.
    copy_tree(
        &crates.join("team-agent/tests/fixtures"),
        &tree.join("crates/team-agent/tests/fixtures"),
        &|_| false,
    )
    .map_err(|e| e.to_string())?;
    let src = tree.join("crates/team-agent/src");
    let lib = std::fs::read_to_string(src.join("lib.rs")).map_err(|e| e.to_string())?;
    let main = std::fs::read_to_string(src.join("main.rs")).map_err(|e| e.to_string())?;
    let mut root = String::new();
    for line in lib.lines() {
        let line = if line == "#[cfg(test)]" && lib.contains("#[cfg(test)]\nextern crate self as team_agent;") {
            continue;
        } else if let Some(rest) = line.strip_prefix("pub mod ") {
            format!("mod {rest}")
        } else if let Some(rest) = line.strip_prefix("pub(crate) mod ") {
            format!("mod {rest}")
        } else if let Some(rest) = line.strip_prefix("pub use ") {
            format!("use {rest}")
        } else {
            line.to_string()
        };
        root.push_str(&line);
        root.push('\n');
    }
    if !root.contains("extern crate self as team_agent;") {
        root.push_str("extern crate self as team_agent;\n");
    }
    for line in main.lines().filter(|line| !line.starts_with("//!")) {
        root.push_str(&line.replace("team_agent::", "crate::"));
        root.push('\n');
    }
    std::fs::write(src.join("stage3_reach.rs"), root).map_err(|e| e.to_string())?;
    strip_dead_code_allowances(&src).map_err(|e| e.to_string())?;
    let manifest_path = tree.join("crates/team-agent/Cargo.toml");
    let cargo_toml = std::fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?;
    let bin = "[[bin]]\nname = \"stage3-reach\"\npath = \"src/stage3_reach.rs\"\n\n[[bin]]\nname = \"team-agent\"";
    if !cargo_toml.contains("[[bin]]\nname = \"team-agent\"") {
        return Err("crates/team-agent/Cargo.toml no longer declares the team-agent [[bin]]".into());
    }
    std::fs::write(&manifest_path, cargo_toml.replacen("[[bin]]\nname = \"team-agent\"", bin, 1))
        .map_err(|e| e.to_string())?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args([
            "check",
            "--offline",
            "--locked",
            "-p",
            "team-agent",
            "--bin",
            "stage3-reach",
            "--message-format=json",
        ])
        .current_dir(&tree)
        .env("CARGO_TARGET_DIR", work.join("target"))
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        .output()
        .map_err(|e| format!("spawn cargo: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(40).collect();
        return Err(format!(
            "reachability build failed ({}):\n{}",
            output.status,
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        ));
    }
    let mut seen = BTreeSet::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if message["reason"] != "compiler-message" || message["message"]["code"]["code"] != "dead_code" {
            continue;
        }
        for span in message["message"]["spans"].as_array().into_iter().flatten() {
            if span["is_primary"] != true {
                continue;
            }
            let file = span["file_name"].as_str().unwrap_or_default();
            let Some(file) = file.split("crates/team-agent/").nth(1) else {
                continue;
            };
            let text = &span["text"][0];
            let source: Vec<char> = text["text"].as_str().unwrap_or_default().chars().collect();
            let lo = text["highlight_start"].as_u64().unwrap_or(1).saturating_sub(1) as usize;
            let hi = text["highlight_end"].as_u64().unwrap_or(1).saturating_sub(1) as usize;
            let name: String = source.get(lo..hi.min(source.len())).unwrap_or_default().iter().collect();
            seen.insert((file.to_string(), span["line_start"].as_u64().unwrap_or(0), name));
        }
    }
    eprintln!(
        "stage3 reachability probe: {} dead_code item(s) in {:.1}s",
        seen.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(seen.into_iter().map(|(file, _, name)| (file, name)).collect())
}

fn reach_items() -> &'static [(String, String)] {
    match reach_probe() {
        Ok(items) => items,
        Err(error) => panic!("binary-rooted reachability probe did not run: {error}"),
    }
}

fn counts<'a>(items: impl Iterator<Item = (&'a str, &'a str)>) -> BTreeMap<(String, String), usize> {
    let mut out = BTreeMap::new();
    for (file, name) in items {
        *out.entry((file.to_string(), name.to_string())).or_insert(0) += 1;
    }
    out
}

// ───────────────────────────── test census ─────────────────────────────

/// Explicit `#[test]` fns (name, ignored) found under src/ and tests/.
fn explicit_tests() -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for source in all_sources() {
        let text = &source.text;
        for (at, _) in text.match_indices("#[test]") {
            // Attributes directly above `#[test]` (e.g. #[ignore], #[serial]).
            let mut ignored = false;
            let mut cursor = at;
            loop {
                let before = text[..cursor].trim_end();
                if !before.ends_with(']') {
                    break;
                }
                let Some(open) = before.rfind("#[") else { break };
                if before[open..].contains('\n') && !before[open..].trim_start().starts_with("#[") {
                    break;
                }
                ignored |= before[open..].starts_with("#[ignore");
                cursor = open;
            }
            // Attributes between `#[test]` and the fn, then the fn name.
            let mut rest = &text[at + "#[test]".len()..];
            loop {
                let trimmed = rest.trim_start();
                if trimmed.starts_with("#[") {
                    ignored |= trimmed.starts_with("#[ignore");
                    let close = trimmed.find(']').unwrap_or(trimmed.len() - 1);
                    rest = &trimmed[close + 1..];
                } else {
                    rest = trimmed;
                    break;
                }
            }
            for prefix in ["pub(crate) ", "pub ", "async ", "unsafe "] {
                rest = rest.strip_prefix(prefix).unwrap_or(rest).trim_start();
            }
            if let Some(after) = rest.strip_prefix("fn ") {
                let name: String = after.trim_start().chars().take_while(|c| is_ident(*c)).collect();
                if !name.is_empty() {
                    out.push((name, ignored));
                }
            }
        }
    }
    out
}

struct CensusRow {
    name: String,
    must_survive: usize,
    must_survive_unignored: usize,
    class: String,
    base_files: String,
}

fn census() -> Vec<CensusRow> {
    CENSUS
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            assert_eq!(f.len(), 5, "malformed census row: {line}");
            CensusRow {
                name: f[0].into(),
                must_survive: f[1].parse().unwrap(),
                must_survive_unignored: f[2].parse().unwrap(),
                class: f[3].into(),
                base_files: f[4].into(),
            }
        })
        .collect()
}

// ───────────────────────────── excision (RED → GREEN) ─────────────────────────────

#[test]
fn excise_stage3_whole_dead_files_are_deleted() {
    let present: Vec<_> = DEAD_FILES
        .iter()
        .filter(|file| crate_root().join(file).exists())
        .collect();
    assert!(
        present.is_empty(),
        "files that hold only Stage 3 scope are still present ({} of {}): {present:#?}",
        present.len(),
        DEAD_FILES.len()
    );
}

#[test]
fn excise_k9_station_root_no_longer_declares_the_journal_module() {
    let station = masked(&crate_root().join("src/station/mod.rs"));
    let declared: Vec<usize> = word_offsets(&station, "lifecycle")
        .into_iter()
        .filter(|at| station[..*at].trim_end().ends_with("mod"))
        .collect();
    assert!(
        declared.is_empty(),
        "src/station/mod.rs still declares `mod lifecycle` (K9 operation journal) at line(s) {:?}",
        declared.iter().map(|at| line_of(&station, *at)).collect::<Vec<_>>()
    );
}

#[test]
fn excise_k9_operation_journal_vocabulary_has_no_code_reference() {
    let mut words: Vec<&str> = K9_VOCABULARY.to_vec();
    words.extend_from_slice(K9_PATHS);
    assert_no_hits("K9 operation journal still referenced", &code_references(&words));
}

#[test]
fn excise_k9_manifest_items_are_absent() {
    assert_no_hits("K9 items still declared", &rows_violating(&["K9"], "ABSENT"));
}

/// Leader/astra ruling: the journal file goes, its live seat-manifest validation does not.
#[test]
fn excise_k9_live_manifest_validation_is_owned_by_seat_store() {
    let seat_store = masked(&crate_root().join("src/station/seat_store.rs"));
    let declared: Vec<usize> = declaration_offsets(&seat_store, "validate_manifest")
        .into_iter()
        .filter(|at| !in_regions(&cfg_test_regions(&seat_store), *at))
        .collect();
    assert_eq!(
        declared.len(),
        1,
        "`validate_manifest` (live seat-manifest invariant) must be declared once in src/station/seat_store.rs, \
         its live owner, instead of the excised journal module"
    );
}

#[test]
fn excise_k6_pre_unified_lifecycle_seams_leave_the_production_build() {
    assert_no_hits(
        "K6 legacy entry points / transport wrappers still declared outside #[cfg(test)]",
        &rows_violating(&["K6", "K11"], "NOT_PRODUCTION"),
    );
}

#[test]
fn excise_k6_dead_layout_and_lifecycle_items_are_deleted() {
    assert_no_hits("K6 items still declared", &rows_violating(&["K6"], "ABSENT"));
}

#[test]
fn excise_k11_state_and_model_residue_is_deleted() {
    assert_no_hits("K11 items still declared", &rows_violating(&["K11"], "ABSENT"));
}

#[test]
fn excise_k11_state_write_intent_drops_never_constructed_arms() {
    let path = crate_root().join("src/state/repository.rs");
    let text = masked(&path);
    let mut hits = Vec::new();
    if let Some(at) = declaration_offsets(&text, "StateWriteIntent").first() {
        let end = item_end(&text, *at);
        let body = &text[*at..=end.min(text.len() - 1)];
        for arm in DEAD_INTENT_ARMS {
            for offset in word_offsets(body, arm) {
                hits.push(format!(
                    "src/state/repository.rs:{} StateWriteIntent::{arm}",
                    line_of(&text, at + offset)
                ));
            }
        }
    }
    let qualified: Vec<String> = DEAD_INTENT_ARMS
        .iter()
        .map(|arm| format!("StateWriteIntent::{arm}"))
        .collect();
    let qualified: Vec<&str> = qualified.iter().map(String::as_str).collect();
    hits.extend(code_references(&qualified));
    assert_no_hits("never-constructed StateWriteIntent arms remain", &hits);
}

#[test]
fn excise_stage3_scope_leaves_no_binary_unreachable_item() {
    let rows = manifest();
    let scope_files: BTreeSet<&str> = rows.iter().map(|row| row.file.as_str()).collect();
    let retained: BTreeSet<(&str, &str)> = rows
        .iter()
        .filter(|row| row.rule == "RETAINED")
        .map(|row| (row.file.as_str(), row.name.as_str()))
        .collect();
    let violations: Vec<String> = reach_items()
        .iter()
        .filter(|(file, name)| {
            scope_files.contains(file.as_str()) && !retained.contains(&(file.as_str(), name.as_str()))
        })
        .map(|(file, name)| format!("{file} `{name}`"))
        .collect();
    assert_no_hits(
        "Stage 3 scope files still compile items unreachable from the binary (delete them, or gate a ruled harness with #[cfg(test)])",
        &violations,
    );
}

// ───────────────────────────── guards (GREEN → GREEN) ─────────────────────────────

#[test]
fn guard_stage3_excision_orphans_no_live_item() {
    let baseline = counts(
        REACH_BASELINE
            .lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .flat_map(|line| {
                let f: Vec<&str> = line.split('\t').collect();
                let n: usize = f[2].parse().unwrap();
                std::iter::repeat_n((f[0], f[1]), n)
            }),
    );
    let current = counts(reach_items().iter().map(|(f, n)| (f.as_str(), n.as_str())));
    let orphaned: Vec<String> = current
        .iter()
        .filter(|(key, count)| baseline.get(*key).copied().unwrap_or(0) < **count)
        .map(|((file, name), count)| {
            format!("{file} `{name}` x{count} (baseline x{})", baseline.get(&(file.clone(), name.clone())).copied().unwrap_or(0))
        })
        .collect();
    assert_no_hits(
        "items reachable from the binary at 2801280b became unreachable (a live caller was cut or a live path rerouted)",
        &orphaned,
    );
}

#[test]
fn guard_unified_start_is_the_cli_seat_activation_route() {
    let adapters = masked(&crate_root().join("src/cli/adapters.rs"));
    let intent = masked(&crate_root().join("src/cli/intent.rs"));
    let cli = masked(&crate_root().join("src/cli/mod.rs"));
    let body = |text: &str, name: &str| -> String {
        let at = declaration_offsets(text, name)
            .into_iter()
            .chain(
                word_offsets(text, name)
                    .into_iter()
                    .filter(|at| text[..*at].trim_end().ends_with("mod")),
            )
            .next()
            .unwrap_or_else(|| panic!("`{name}` is no longer declared"));
        text[at..=item_end(text, at).min(text.len() - 1)].to_string()
    };
    let compact = |s: String| s.split_whitespace().collect::<String>();
    let request = compact(body(&adapters, "cmd_start_request"));
    assert!(
        request.contains("seat_start::start("),
        "cmd_start_request must execute through lifecycle::seat_start::start"
    );
    let start_agent = compact(body(&adapters, "cmd_start_agent"));
    assert!(
        start_agent.contains("cmd_start_request(") && start_agent.contains("SessionPolicy::"),
        "agent start must route through the unified start request with an explicit session policy"
    );
    let intent_flat: String = intent.split_whitespace().collect();
    for arm in [
        "Self::UnifiedStart{request,..}=>cmd_start_request(request,true)",
        "Self::StartAgent(a)=>cmd_start_agent(a)",
        "Self::RestartTeam(a)=>cmd_restart(a)",
        "Self::CreateTeam(a)=>cmd_quick_start(a)",
    ] {
        assert!(intent_flat.contains(arm), "cli intent dispatch lost `{arm}`");
    }
    let port = compact(body(&cli, "lifecycle_port"));
    for call in [
        "quick_start_in_workspace_with_backend(",
        "restart_with_session_convergence_deadline(",
        "contract_runtime::commands::restart(",
        "contract_runtime::commands::stop_agent(",
    ] {
        assert!(port.contains(call), "cli::lifecycle_port lost its live call `{call}`");
    }
    let seat_start = masked(&crate_root().join("src/lifecycle/seat_start.rs"));
    let policy = compact(body(&seat_start, "SessionPolicy"));
    for variant in ["Preserve", "Auto", "Fresh"] {
        assert!(policy.contains(variant), "SessionPolicy lost `{variant}`");
    }
}

#[test]
fn guard_live_kernels_beside_retired_seams_survive() {
    let mut missing = Vec::new();
    for (file, item) in LIVE_NEIGHBOURS {
        let path = crate_root().join(file);
        if !path.exists() || declaration_offsets(&masked(&path), item).is_empty() {
            missing.push(format!("{file} :: {item}"));
        }
    }
    assert_no_hits("live kernels deleted alongside Stage 3 scope", &missing);
}

#[test]
fn guard_live_station_engine_modules_stay_declared() {
    let station = masked(&crate_root().join("src/station/mod.rs"));
    let missing: Vec<&str> = LIVE_STATION_MODULES
        .iter()
        .copied()
        .filter(|module| {
            !word_offsets(&station, module)
                .into_iter()
                .any(|at| station[..at].trim_end().ends_with("mod"))
        })
        .collect();
    assert!(missing.is_empty(), "src/station/mod.rs lost live modules: {missing:?}");
    let inbox = masked(&crate_root().join("src/inbox_store.rs"));
    for call in ["codec::decode", "codec::encode", "codec::authorize_mutation"] {
        assert!(inbox.contains(call), "inbox_store no longer uses the live station codec `{call}`");
    }
}

/// GREEN before and after the relocation: exactly one production `validate_manifest`
/// in the station, still enforced on every seat publication and lifecycle target check.
#[test]
fn guard_seat_manifest_validation_stays_wired_to_its_live_callers() {
    let declared = declarations("validate_manifest", &|f| f.starts_with("src/station/"), false);
    assert_eq!(declared.len(), 1, "expected one production validate_manifest under src/station, found {declared:?}");
    let seat_store = masked(&crate_root().join("src/station/seat_store.rs"));
    let publish = declaration_offsets(&seat_store, "publish")
        .into_iter()
        .map(|at| seat_store[at..=item_end(&seat_store, at).min(seat_store.len() - 1)].to_string())
        .find(|body| body.contains("manifest"))
        .expect("SeatLock::publish is no longer declared");
    assert!(publish.contains("validate_manifest("), "SeatLock::publish no longer validates the manifest it publishes");
    let projection = masked(&crate_root().join("src/station/projection.rs"));
    let at = declaration_offsets(&projection, "check_target")
        .into_iter()
        .next()
        .expect("projection::check_target is no longer declared");
    let check_target = &projection[at..=item_end(&projection, at).min(projection.len() - 1)];
    assert!(check_target.contains("validate_manifest("), "projection::check_target no longer validates the seat manifest");
    let lock = masked(&crate_root().join("src/lifecycle/lock.rs"));
    assert!(lock.contains("projection::check_target("), "lifecycle locking no longer checks the station target");
}

#[test]
fn guard_protected_unreachable_items_are_not_swept_up() {
    let mut missing = Vec::new();
    for (file, item) in PROTECTED_UNREACHABLE {
        let path = crate_root().join(file);
        if !path.exists() || declaration_offsets(&masked(&path), item).is_empty() {
            missing.push(format!("{file} :: {item}"));
        }
    }
    let repository = masked(&crate_root().join("src/state/repository.rs"));
    if word_offsets(&repository, "CoordinatorConptyShim").is_empty() {
        missing.push("src/state/repository.rs :: StateWriteIntent::CoordinatorConptyShim (Windows conpty shim writer)".into());
    }
    let resume = masked(&crate_root().join("src/provider/session/resume.rs"));
    if word_offsets(&resume, "SessionCaptureIncomplete").is_empty() {
        missing.push("src/provider/session/resume.rs :: SessionCaptureIncomplete".into());
    }
    for file in SURVIVOR_FILES {
        if !crate_root().join(file).exists() {
            missing.push((*file).to_string());
        }
    }
    assert_no_hits("Stage 2 protected / Windows-held / earlier-stage items removed", &missing);
}

#[test]
fn guard_tests_outside_the_ruled_blast_radius_survive_without_new_ignores() {
    let mut present: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (name, ignored) in explicit_tests() {
        let entry = present.entry(name).or_insert((0, 0));
        entry.0 += 1;
        if !ignored {
            entry.1 += 1;
        }
    }
    let mut lost = Vec::new();
    let mut newly_ignored = Vec::new();
    for row in census().iter().filter(|row| row.must_survive > 0) {
        let (count, unignored) = present.get(&row.name).copied().unwrap_or((0, 0));
        if count < row.must_survive {
            let class = if row.class.starts_with('R') { "REPOINT obligation" } else { "protected" };
            lost.push(format!("{} ({class}; base {} x{}, now x{count}) {}", row.name, row.class, row.must_survive, row.base_files));
        } else if unignored < row.must_survive_unignored {
            newly_ignored.push(format!("{} (unignored base x{}, now x{unignored})", row.name, row.must_survive_unignored));
        }
    }
    assert!(
        lost.is_empty() && newly_ignored.is_empty(),
        "tests outside the Stage 3 RETIRE list disappeared ({}) or became #[ignore] ({}).\n\
         Only class-X rows may retire; R rows must be repointed to the live seam under the same name.\n\
         lost (first 40):\n{}\nnewly ignored (first 20):\n{}",
        lost.len(),
        newly_ignored.len(),
        lost.iter().take(40).cloned().collect::<Vec<_>>().join("\n"),
        newly_ignored.iter().take(20).cloned().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn guard_fixtures_are_consistent_with_each_other() {
    let rows = manifest();
    assert!(rows.len() >= 190, "manifest lost rows: {}", rows.len());
    let mut problems = Vec::new();
    let baseline: BTreeSet<(String, String)> = REACH_BASELINE
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            (f[0].to_string(), f[1].to_string())
        })
        .collect();
    let mut seen = BTreeSet::new();
    for row in &rows {
        if !["K6", "K9", "K11"].contains(&row.cluster.as_str())
            || !["ABSENT", "NOT_PRODUCTION", "REACH", "RETAINED"].contains(&row.rule.as_str())
            || !["file", "crate"].contains(&row.check.as_str())
            || !row.file.starts_with("src/")
            || !row.name.chars().all(is_ident)
        {
            problems.push(format!("malformed: {row:?}"));
        }
        if !baseline.contains(&(row.file.clone(), row.name.clone())) {
            problems.push(format!("not in the reach baseline: {} {}", row.file, row.name));
        }
        if !seen.insert((row.file.clone(), row.name.clone(), row.kind.clone(), row.rule.clone()))
            && !["REACH", "RETAINED"].contains(&row.rule.as_str())
        {
            problems.push(format!("duplicate: {} {}", row.file, row.name));
        }
    }
    let census = census();
    let retire = census.iter().filter(|row| row.class.contains('X')).count();
    let repoint = census.iter().filter(|row| row.class.starts_with('R')).count();
    if census.len() < 3000 || retire != 68 || repoint != 32 {
        problems.push(format!("census shape changed: {} names, {retire} retire, {repoint} repoint", census.len()));
    }
    assert!(problems.is_empty(), "fixture problems: {problems:#?}");
}

#[test]
fn guard_source_helpers_ignore_comments_literals_and_see_cfg_test() {
    let text = mask_rust(
        "// NativeSeat\nlet a = \"NativeSeat\"; /* NativeSeat /* nested */ */\n\
         let b = r#\"NativeSeat\"#; let c = 'x'; let d = '\\''; let e = '\"';\n\
         fn f<'a>(_: &'a NativeSeat) {}\nenum E { NativeSeatMismatch }\n",
    );
    assert_eq!(word_offsets(&text, "NativeSeat").len(), 1, "{text}");
    assert_eq!(line_of(&text, word_offsets(&text, "NativeSeat")[0]), 4);
    let gated = mask_rust(
        "pub fn live() {}\n#[cfg(test)]\npub(crate) fn start_agent_with_transport() { inner(); }\n\
         #[cfg(test)]\nmod tests { fn restart() {} }\n#[cfg(all(test, unix))]\nfn quick_start() {}\nfn launch() {}\n",
    );
    let regions = cfg_test_regions(&gated);
    let gated_at = |name: &str| declaration_offsets(&gated, name).into_iter().all(|at| in_regions(&regions, at));
    assert!(gated_at("start_agent_with_transport"));
    assert!(gated_at("restart"));
    assert!(gated_at("quick_start"));
    assert!(!gated_at("launch"));
    assert!(!gated_at("live"));
    assert!(declaration_offsets("pub(crate) struct SeatLock {", "SeatLock").len() == 1);
    assert!(declaration_offsets("let x: SeatLock = y;", "SeatLock").is_empty());
    assert!(is_test_file("src/lifecycle/tests/core.rs") && is_test_file("tests/x.rs"));
    assert!(!is_test_file("src/lifecycle/restart/agent.rs"));
    assert_eq!(module_tree("src/lifecycle/restart/agent.rs"), "src/lifecycle/");
}
