//! Stage 1 excision (K1 legacy SQLite message engine, K10 legacy inject/submit
//! transport) must not change current single-point CLI behavior. Every case is
//! GREEN on the 0.5.129 baseline (df796be5) and must stay GREEN after excision.
//!
//! Properties use a seeded SplitMix64 generator (no new test dependencies).
//! Reproduce a failure with `STAGE1_PROP_SEED=<seed>`; widen the search with
//! `STAGE1_PROP_CASES=<n>`. Each failing case prints its seed and case index.
#![cfg(unix)]
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used, clippy::panic)]

#[path = "e2e/framework.rs"]
mod framework;

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};
use team_agent::model::yaml::{self, Value as Yaml};

// ───────────────────────────── generator ─────────────────────────────

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn range(&mut self, low: usize, high: usize) -> usize {
        low + self.below(high - low + 1)
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
    fn slug(&mut self, len: usize) -> String {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        (0..len).map(|_| *self.pick(ALPHABET) as char).collect()
    }

    /// Message bodies that stress byte-exact persistence: quotes, backslashes,
    /// newlines, tabs, CJK, emoji, combining marks, ESC, flag-looking words,
    /// JSON and shell metacharacters. Always starts with a letter so the CLI
    /// parser sees a positional message, never a flag.
    fn content(&mut self) -> String {
        const PIECES: &[&str] = &[
            "alpha",
            "Beta",
            " ",
            "  ",
            "\n",
            "\n\n",
            "\t",
            "\"",
            "'",
            "\\",
            "\\n",
            "{\"k\":[1,2]}",
            "--mailbox",
            " -n 3",
            "$(rm -rf /)",
            "`id`",
            ";|&",
            "中文消息",
            "🚀",
            "e\u{301}",
            "\u{1b}[31mred\u{1b}[0m",
            "\u{1b}",
            "ss1:",
            "%s%n",
            "<tag/>",
            "a,b,c",
            "*",
            "..",
            "/tmp/x",
            "\r",
            "line\r\nbreak",
            "∑≠",
            "ok",
        ];
        let mut text = String::from(*self.pick(&["Task", "hello", "Ω", "x", "Reply"]));
        for _ in 0..self.range(0, 12) {
            text.push_str(*self.pick(PIECES));
        }
        if self.chance(10) {
            text.push_str(&"long-".repeat(self.range(50, 400)));
        }
        text
    }

    fn message_id(&mut self) -> String {
        const ALPHABET: &[u8] =
            b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-";
        let len = self.range(1, 40);
        format!(
            "m{}",
            (0..len)
                .map(|_| *self.pick(ALPHABET) as char)
                .collect::<String>()
        )
    }
}

fn prop_seed() -> u64 {
    std::env::var("STAGE1_PROP_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0x5717_E0C1_2026_1105)
}

fn prop_cases(default: usize) -> usize {
    std::env::var("STAGE1_PROP_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Run `body` for `cases` independent seeded cases; report the reproducer on failure.
fn for_each_case(name: &str, default_cases: usize, mut body: impl FnMut(&mut Rng, usize)) {
    let seed = prop_seed();
    for case in 0..prop_cases(default_cases) {
        let mut rng = Rng(seed ^ (case as u64).wrapping_mul(0xA24B_AED4_963E_E407));
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&mut rng, case)));
        if let Err(panic) = outcome {
            eprintln!("property `{name}` failed: STAGE1_PROP_SEED={seed} case={case}");
            std::panic::resume_unwind(panic);
        }
    }
}

// ───────────────────────────── fixtures ─────────────────────────────

const POISON: &[u8] = b"poison: the retired SQLite message engine must never open this file";
const AGENTS: &[&str] = &["w1", "w2", "w3"];
const TEAM: &str = "alpha";

struct Ws {
    root: PathBuf,
    poisoned: bool,
    path_env: String,
}

impl Ws {
    /// A runtime snapshot with stopped seats: `--mailbox` delivery needs no live
    /// pane, so these properties run without tmux and without any SQLite file.
    fn new(poisoned: bool) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "stage1-inv-{}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join(".team/runtime")).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        let root = root.canonicalize().unwrap();
        let agents: serde_json::Map<String, Value> = AGENTS
            .iter()
            .map(|id| (id.to_string(), json!({"status": "stopped"})))
            .collect();
        std::fs::write(
            root.join(".team/runtime/state.json"),
            serde_json::to_vec(&json!({
                "active_team_key": TEAM,
                "teams": {TEAM: {"team_id": TEAM, "status": "stopped", "agents": agents}}
            }))
            .unwrap(),
        )
        .unwrap();
        if poisoned {
            std::fs::write(root.join(".team/runtime/team.db"), POISON).unwrap();
        }
        Self {
            root,
            poisoned,
            path_env: "/usr/bin:/bin".into(),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command
            .args(args)
            .arg("--workspace")
            .arg(&self.root)
            .current_dir(&self.root)
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("PATH", &self.path_env);
        command
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn send(&self, to: &str, content: &str, extra: &[&str]) -> (Output, Value) {
        let mut args = vec!["send", to, content, "--json"];
        args.extend_from_slice(extra);
        let output = self.cli(&args);
        let value = parse(&output);
        (output, value)
    }

    fn inbox(&self, recipient: &str) -> Vec<Value> {
        let output = self.cli(&["inbox", recipient, "-n", "100000", "--json"]);
        assert!(
            output.status.success(),
            "inbox {recipient}: {}",
            text(&output)
        );
        parse(&output)["messages"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// Every file under `.team` except CLI error logs (an allowed refusal receipt).
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(at).into_iter().flatten().flatten() {
                let path = entry.path();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                if relative.starts_with(".team/logs") {
                    continue;
                }
                if path.is_dir() {
                    out.insert(relative, b"<dir>".to_vec());
                    walk(root, &path, out);
                } else {
                    out.insert(relative, std::fs::read(&path).unwrap_or_default());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.root, &self.root.join(".team"), &mut out);
        out
    }

    /// Regular data files only: no record or state bytes, ignoring empty
    /// directories and advisory lock files a refused append may leave behind.
    fn records(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        self.snapshot()
            .into_iter()
            .filter(|(path, bytes)| {
                bytes != b"<dir>" && path.extension().is_none_or(|ext| ext != "lock")
            })
            .collect()
    }

    fn assert_no_legacy_artifacts(&self) {
        assert_no_legacy_artifacts(&self.root, self.poisoned);
    }
}

impl Drop for Ws {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The retired engine left three observable footprints: an SQLite file (plus
/// WAL/SHM/journal), the `mirror_files` directory and FIFO result waiters.
fn assert_no_legacy_artifacts(root: &Path, poisoned: bool) {
    let db = root.join(".team/runtime/team.db");
    if poisoned {
        assert_eq!(
            std::fs::read(&db).unwrap(),
            POISON,
            "planted team.db was opened or rewritten"
        );
    } else {
        assert!(
            !db.exists(),
            "a current CLI path created the retired SQLite store {}",
            db.display()
        );
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = root.join(format!(".team/runtime/team.db{suffix}"));
        assert!(
            !sidecar.exists(),
            "SQLite sidecar appeared: {}",
            sidecar.display()
        );
    }
    assert!(
        !root.join(".team/messages").exists(),
        "the inert mirror_files directory was materialized"
    );
    fn walk(at: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(at).into_iter().flatten().flatten() {
            let kind = entry.file_type().unwrap();
            if kind.is_fifo() {
                found.push(entry.path());
            } else if kind.is_dir() {
                walk(&entry.path(), found);
            }
        }
    }
    let mut fifos = Vec::new();
    walk(&root.join(".team"), &mut fifos);
    assert!(fifos.is_empty(), "FIFO result waiters appeared: {fifos:?}");
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn parse(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("stdout is not one JSON value ({error}): {}", text(output)))
}

fn contents(messages: &[Value]) -> Vec<String> {
    messages
        .iter()
        .map(|m| m["content"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn assert_not_a_crash(output: &Output) {
    assert_ne!(
        output.status.code(),
        Some(101),
        "CLI panicked: {}",
        text(output)
    );
    assert!(
        output.status.code().is_some(),
        "CLI was killed by a signal: {}",
        text(output)
    );
    assert!(
        !text(output).contains("panicked at"),
        "CLI panicked: {}",
        text(output)
    );
}

// ───────────────────── a) persistence & schema shape ─────────────────────

fn write_team(dir: &Path, rng: &mut Rng) -> Vec<String> {
    std::fs::create_dir_all(dir.join("agents")).unwrap();
    let mut team = format!(
        "---\nname: t-{}\nobjective: stage1 schema property\n",
        rng.slug(6)
    );
    if rng.chance(50) {
        team.push_str(&format!("worker_to_worker: {}\n", rng.chance(50)));
    }
    if rng.chance(30) {
        team.push_str("fast: true\n");
    }
    team.push_str("---\nTeam body.\n");
    std::fs::write(dir.join("TEAM.md"), team).unwrap();
    let mut ids = Vec::new();
    for index in 0..rng.range(1, 5) {
        let id = format!("{}{index}", rng.pick(&["worker", "dev", "rev", "qa"]));
        std::fs::write(
            dir.join(format!("agents/{id}.md")),
            format!(
                "---\nname: {id}\nrole: role {index}\nprovider: fake\nmodel: fake\nauth_mode: subscription\n\
                 dangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\nBody {index}.\n"
            ),
        )
        .unwrap();
        ids.push(id);
    }
    ids
}

fn map_get<'a>(value: &'a Yaml, key: &str) -> &'a Yaml {
    value
        .get(key)
        .unwrap_or_else(|| panic!("missing key {key}"))
}

fn map_mut<'a>(value: &'a mut Yaml, key: &str) -> &'a mut Yaml {
    let Yaml::Map(fields) = value else {
        panic!("not a map while looking for {key}")
    };
    &mut fields
        .iter_mut()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("missing key {key}"))
        .1
}

fn message_store_keys(spec: &Yaml) -> Vec<String> {
    map_get(map_get(spec, "communication"), "message_store")
        .as_map()
        .expect("message_store must stay a map")
        .iter()
        .map(|(key, _)| key.clone())
        .collect()
}

fn temp_dir(label: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "stage1-{label}-{}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn prop_compiled_specs_always_emit_the_exact_inert_message_store_block() {
    for_each_case("compiled message_store shape", 24, |rng, _| {
        let dir = temp_dir("compile");
        write_team(&dir, rng);
        let spec = team_agent::compiler::compile_team(&dir).expect("compile valid team");
        team_agent::model::spec::validate_spec(&spec, &dir).expect("compiled spec must validate");
        assert_eq!(message_store_keys(&spec), ["sqlite", "mirror_files"]);
        let store = map_get(map_get(&spec, "communication"), "message_store");
        assert!(map_get(store, "sqlite")
            .as_str()
            .is_some_and(|v| !v.is_empty()));
        assert!(map_get(store, "mirror_files")
            .as_str()
            .is_some_and(|v| !v.is_empty()));
        // A persisted spec reloads, revalidates and keeps the same block.
        let reloaded = yaml::loads(&yaml::dumps(&spec)).expect("dumped spec reloads");
        team_agent::model::spec::validate_spec(&reloaded, &dir).expect("reloaded spec validates");
        assert_eq!(
            map_get(map_get(&reloaded, "communication"), "message_store"),
            store,
            "dump/load must not alter the persisted message_store block"
        );
        assert_no_legacy_artifacts(&dir, false);
        std::fs::remove_dir_all(dir).unwrap();
    });
}

#[test]
fn prop_message_store_keys_are_required_and_exact_while_values_stay_inert() {
    for_each_case("message_store validation", 48, |rng, _| {
        let dir = temp_dir("schema");
        write_team(&dir, rng);
        let base = team_agent::compiler::compile_team(&dir).unwrap();
        let poison = dir.join("poison.db");
        std::fs::write(&poison, POISON).unwrap();
        let missing_dir = dir.join(format!("never-{}/deep/team.db", rng.slug(5)));
        let validate = |spec: &Yaml| {
            team_agent::model::spec::validate_spec(spec, &dir).map_err(|e| e.to_string())
        };
        let mut spec = base.clone();
        let store = map_mut(map_mut(&mut spec, "communication"), "message_store");
        match rng.below(5) {
            0 => {
                // Any string value is accepted; the engine it once named is never opened.
                let candidates = [
                    poison.display().to_string(),
                    missing_dir.display().to_string(),
                    String::new(),
                    rng.content(),
                    "/dev/null".to_string(),
                ];
                for key in ["sqlite", "mirror_files"] {
                    *map_mut(store, key) = Yaml::Str(rng.pick(&candidates).clone());
                }
                validate(&spec).expect("message_store values are inert strings");
            }
            1 => {
                let key = *rng.pick(&["sqlite", "mirror_files"]);
                let Yaml::Map(fields) = store else {
                    unreachable!()
                };
                fields.retain(|(k, _)| k != key);
                let error = validate(&spec).unwrap_err();
                assert!(
                    error.contains(&format!(
                        "/communication/message_store/{key}: missing required field"
                    )),
                    "{error}"
                );
            }
            2 => {
                let len = rng.range(1, 8);
                let extra = format!("x_{}", rng.slug(len));
                let Yaml::Map(fields) = store else {
                    unreachable!()
                };
                fields.push((extra.clone(), Yaml::Str(rng.content())));
                let error = validate(&spec).unwrap_err();
                assert!(
                    error.contains(&format!(
                        "/communication/message_store/{extra}: unknown field"
                    )),
                    "{error}"
                );
            }
            3 => {
                *store = rng
                    .pick(&[
                        Yaml::Null,
                        Yaml::Bool(true),
                        Yaml::Str("sqlite".into()),
                        Yaml::List(vec![]),
                    ])
                    .clone();
                let error = validate(&spec).unwrap_err();
                assert!(
                    error.contains("/communication/message_store: must be an object"),
                    "{error}"
                );
            }
            _ => {
                let Yaml::Map(fields) = map_mut(&mut spec, "communication") else {
                    unreachable!()
                };
                fields.retain(|(k, _)| k != "message_store");
                let error = validate(&spec).unwrap_err();
                assert!(
                    error.contains("/communication/message_store: missing required field"),
                    "{error}"
                );
            }
        }
        assert_eq!(
            std::fs::read(&poison).unwrap(),
            POISON,
            "validation opened the named database"
        );
        assert!(
            !missing_dir.parent().unwrap().exists(),
            "validation created the named database path"
        );
        std::fs::remove_dir_all(dir).unwrap();
    });
}

/// Real fake-provider team: `start`/`start-agent` on an existing workspace
/// revalidate the persisted spec, whose `message_store` block names an SQLite
/// file that must never be created or opened.
#[test]
fn start_on_existing_workspace_revalidates_message_store_spec_without_the_sqlite_engine() {
    let ws = framework::TestWorkspace::new("s1-start").with_fake_spec(&["w1"]);
    let root = ws.path().to_str().unwrap().to_string();
    let started = framework::run_ta(
        &ws,
        &[
            "start",
            "--from",
            &root,
            "--workspace",
            &root,
            "--team",
            "s1start",
            "--json",
        ],
    );
    if let Some(socket) = ws.read_state()["tmux_socket"].as_str() {
        ws.register_owned_tmux_socket(Path::new(socket));
    }
    assert!(
        framework::quick_start_workers_available(&started),
        "{} {}",
        started.stdout,
        started.stderr
    );
    let specs: Vec<PathBuf> = std::fs::read_dir(ws.path().join(".team/runtime"))
        .unwrap()
        .flatten()
        .map(|entry| entry.path().join("team.spec.yaml"))
        .filter(|path| path.is_file())
        .collect();
    assert_eq!(
        specs.len(),
        1,
        "exactly one persisted runtime spec: {specs:?}"
    );
    let persisted = yaml::loads(&std::fs::read_to_string(&specs[0]).unwrap()).unwrap();
    assert_eq!(message_store_keys(&persisted), ["sqlite", "mirror_files"]);
    let db = ws.path().join(".team/runtime/team.db");
    let poisoned = !db.exists();
    if poisoned {
        std::fs::write(&db, POISON).unwrap();
    }
    let before = std::fs::read(&db).unwrap();
    for args in [
        vec!["stop-agent", "w1", "--json"],
        vec!["start-agent", "w1", "--json"],
        vec!["start", "--json"],
    ] {
        let out = framework::run_ta(&ws, &args);
        assert!(out.is_success(), "{args:?}: {} {}", out.stdout, out.stderr);
    }
    assert!(
        ws.read_state()["agents"]["w1"]["pane_id"].is_string(),
        "restarted seat must own a pane"
    );
    let sent = framework::run_ta(&ws, &["send", "w1", "stage1 start probe", "--json"]);
    assert!(sent.is_success(), "{} {}", sent.stdout, sent.stderr);
    assert_eq!(
        std::fs::read(&db).unwrap(),
        before,
        "team.db was opened/rewritten by start or send"
    );
    assert_no_legacy_artifacts(ws.path(), poisoned);
    let stopped = framework::run_ta(&ws, &["stop", "--json"]);
    assert!(
        stopped.is_success(),
        "{} {}",
        stopped.stdout,
        stopped.stderr
    );
}

// ─────────────────── b) message delivery & idempotency ───────────────────

#[test]
fn prop_retries_with_one_message_id_store_exactly_one_record() {
    for_each_case("message-id idempotency", 16, |rng, _| {
        let ws = Ws::new(rng.chance(50));
        let to = *rng.pick(&["w1", "w2", "w3", "leader"]);
        let content = rng.content();
        let id = rng.message_id();
        let attempts = rng.range(2, 5);
        let mut receipts = Vec::new();
        for attempt in 0..attempts {
            let (output, value) = ws.send(to, &content, &["--mailbox", "--message-id", &id]);
            assert!(
                output.status.success(),
                "attempt {attempt}: {}",
                text(&output)
            );
            assert_eq!(value["stored"], true);
            assert_eq!(
                value["duplicate"],
                attempt > 0,
                "attempt {attempt}: {value}"
            );
            receipts.push(value);
        }
        for receipt in &receipts {
            assert_eq!(receipt["message_id"], json!(id));
            assert_eq!(receipt["record_ref"], receipts[0]["record_ref"]);
            assert_eq!(receipt["dedupe_key"], receipts[0]["dedupe_key"]);
        }
        let inbox = ws.inbox(to);
        assert_eq!(
            contents(&inbox),
            [content.clone()],
            "exactly one byte-exact record"
        );
        assert_eq!(inbox[0]["seq"], 1);
        ws.assert_no_legacy_artifacts();
    });
}

#[test]
fn prop_conflicting_retry_refuses_and_writes_nothing() {
    for_each_case("message-id conflict", 12, |rng, _| {
        let ws = Ws::new(rng.chance(50));
        let to = *rng.pick(AGENTS);
        let id = rng.message_id();
        let first = rng.content();
        let (output, _) = ws.send(to, &first, &["--mailbox", "--message-id", &id]);
        assert!(output.status.success(), "{}", text(&output));
        let before = ws.snapshot();
        let other = format!("{first}{}", rng.content());
        let output = ws.cli(&[
            "send",
            to,
            &other,
            "--json",
            "--mailbox",
            "--message-id",
            &id,
        ]);
        assert_not_a_crash(&output);
        assert!(
            !output.status.success(),
            "a different payload under the same id must refuse: {}",
            text(&output)
        );
        assert_eq!(parse(&output)["ok"], false);
        assert!(
            text(&output).contains("request_id_conflict"),
            "{}",
            text(&output)
        );
        assert_eq!(ws.snapshot(), before, "a refused conflict wrote state");
        assert_eq!(contents(&ws.inbox(to)), [first]);
        ws.assert_no_legacy_artifacts();
    });
}

#[test]
fn prop_record_ref_tokens_replay_idempotently_and_tampered_tokens_fail_closed() {
    for_each_case("dedupe token replay", 12, |rng, _| {
        let ws = Ws::new(rng.chance(50));
        let to = *rng.pick(AGENTS);
        let content = rng.content();
        let (output, receipt) = ws.send(to, &content, &["--mailbox"]);
        assert!(output.status.success(), "{}", text(&output));
        let token = receipt["dedupe_key"].as_str().unwrap().to_string();
        assert!(token.starts_with("ss1:"), "{token}");
        let before = ws.snapshot();
        let (output, replay) = ws.send(to, &content, &["--mailbox", "--message-id", &token]);
        assert!(output.status.success(), "{}", text(&output));
        assert_eq!(replay["duplicate"], true);
        assert_eq!(replay["record_ref"], receipt["record_ref"]);
        assert_eq!(ws.snapshot(), before, "token replay appended data");
        // Tamper with one field: malformed, out-of-range or mismatched references refuse.
        let mut parts: Vec<String> = token.split(':').map(str::to_string).collect();
        match rng.below(5) {
            0 => parts[1] = "0".into(),
            1 => parts[2] = (rng.range(2, 9999)).to_string(),
            2 => parts[3] = (rng.range(1, 99_999)).to_string(),
            3 => parts[5] = "f".repeat(64),
            _ => parts[5].truncate(rng.range(0, 63)),
        }
        let tampered = parts.join(":");
        let output = ws.cli(&[
            "send",
            to,
            &content,
            "--json",
            "--mailbox",
            "--message-id",
            &tampered,
        ]);
        assert_not_a_crash(&output);
        assert!(
            !output.status.success(),
            "tampered token {tampered} must refuse: {}",
            text(&output)
        );
        assert_eq!(ws.snapshot(), before, "tampered token wrote state");
        assert_eq!(contents(&ws.inbox(to)), [content]);
        ws.assert_no_legacy_artifacts();
    });
}

#[test]
fn prop_inbox_keeps_per_recipient_order_exact_content_contiguous_seq_and_scope() {
    for_each_case("ordered scoped delivery", 10, |rng, _| {
        let ws = Ws::new(rng.chance(50));
        let recipients = ["w1", "w2", "w3", "leader"];
        let mut expected: BTreeMap<&str, Vec<String>> =
            recipients.iter().map(|r| (*r, Vec::new())).collect();
        for _ in 0..rng.range(4, 14) {
            let content = rng.content();
            let mut targets: Vec<&str> = (0..rng.range(1, 3))
                .map(|_| *rng.pick(&recipients))
                .collect();
            let joined = targets.join(",");
            let (output, value) = ws.send(&joined, &content, &["--mailbox"]);
            assert!(output.status.success(), "{joined}: {}", text(&output));
            targets.sort_unstable();
            targets.dedup();
            let stored = if targets.len() > 1 {
                value["stored"].as_u64().unwrap()
            } else {
                1
            };
            assert_eq!(
                stored as usize,
                targets.len(),
                "fanout stores once per unique recipient: {value}"
            );
            for target in targets {
                expected.get_mut(target).unwrap().push(content.clone());
            }
        }
        for recipient in recipients {
            let inbox = ws.inbox(recipient);
            assert_eq!(
                contents(&inbox),
                expected[recipient],
                "{recipient} inbox order/content"
            );
            for (index, record) in inbox.iter().enumerate() {
                assert_eq!(
                    record["seq"],
                    json!(index + 1),
                    "{recipient} seq must be contiguous"
                );
                assert_eq!(record["recipient"], recipient);
                assert_eq!(record["team"], TEAM);
                assert_eq!(record["sender"], "leader");
            }
        }
        ws.assert_no_legacy_artifacts();
    });
}

fn invalid_target(rng: &mut Rng) -> String {
    let len = rng.range(1, 6);
    let ghost = format!("ghost{}", rng.slug(len));
    match rng.below(9) {
        0 => ghost,
        1 => format!("{ghost}/w1"),
        2 => format!("{}:{}", rng.slug(3), rng.slug(2)),
        3 => format!("../{ghost}"),
        4 => format!("{TEAM}/{ghost}"),
        5 => format!("幽灵{}", rng.slug(2)),
        6 => format!("{} {}", rng.slug(2), rng.slug(2)),
        7 => format!(
            "{}::{ghost}/w1",
            std::env::temp_dir()
                .join(format!("stage1-absent-{}", rng.slug(8)))
                .display()
        ),
        _ => format!("w1{}", rng.pick(&["!", "#", "@x", "\u{1b}", "%", "*"])),
    }
}

#[test]
fn prop_any_invalid_target_refuses_the_whole_batch_atomically_without_panic() {
    for_each_case("atomic batch refusal", 24, |rng, _| {
        let ws = Ws::new(rng.chance(50));
        // Seed some history so "no write" covers existing segments too.
        if rng.chance(50) {
            assert!(ws.send("w1", "seed", &["--mailbox"]).0.status.success());
        }
        let before = ws.snapshot();
        let mut targets: Vec<String> = (0..rng.range(0, 3))
            .map(|_| rng.pick(&["w1", "w2", "w3", "leader"]).to_string())
            .collect();
        let invalid = invalid_target(rng);
        targets.insert(rng.below(targets.len() + 1), invalid.clone());
        let joined = targets.join(",");
        let output = ws.cli(&["send", &joined, &rng.content(), "--json", "--mailbox"]);
        assert_not_a_crash(&output);
        assert!(
            !output.status.success(),
            "batch {joined:?} with {invalid:?} must refuse: {}",
            text(&output)
        );
        let value = parse(&output);
        assert_eq!(value["ok"], false, "{value}");
        if value.get("status").is_some() {
            assert_eq!(value["status"], "refused", "{value}");
            assert_eq!(value["stored"], false, "{value}");
            assert!(
                value["invalid_targets"]
                    .as_array()
                    .is_some_and(|t| !t.is_empty()),
                "{value}"
            );
        }
        let actionable = value["action"]
            .as_str()
            .or(value["hint"].as_str())
            .unwrap_or_default();
        assert!(
            !actionable.trim().is_empty(),
            "refusal must carry an action: {value}"
        );
        assert_eq!(
            ws.snapshot(),
            before,
            "refused batch {joined:?} wrote state"
        );
        ws.assert_no_legacy_artifacts();
    });
}

#[test]
fn concurrent_sends_serialize_per_recipient_and_collapse_one_message_id() {
    let ws = Ws::new(true);
    let racing: Vec<_> = (0..12)
        .map(|_| {
            ws.command(&[
                "send",
                "w1",
                "same racing payload",
                "--json",
                "--mailbox",
                "--message-id",
                "race-1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
        })
        .chain((0..12).map(|n| {
            ws.command(&[
                "send",
                "w2",
                &format!("distinct payload {n}"),
                "--json",
                "--mailbox",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
        }))
        .collect();
    for child in racing {
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{}", text(&output));
    }
    assert_eq!(
        contents(&ws.inbox("w1")),
        ["same racing payload"],
        "one id under contention stores once"
    );
    let w2 = ws.inbox("w2");
    assert_eq!(w2.len(), 12);
    let seqs: Vec<u64> = w2.iter().map(|m| m["seq"].as_u64().unwrap()).collect();
    assert_eq!(
        seqs,
        (1..=12).collect::<Vec<_>>(),
        "concurrent appends keep contiguous seq"
    );
    let mut seen: Vec<String> = contents(&w2);
    seen.sort();
    let mut want: Vec<String> = (0..12).map(|n| format!("distinct payload {n}")).collect();
    want.sort();
    assert_eq!(seen, want, "no lost or interleaved record");
    ws.assert_no_legacy_artifacts();
}

#[test]
fn prop_inbox_reads_are_pure_and_traversal_recipients_refuse() {
    for_each_case("pure inbox reads", 8, |rng, _| {
        let ws = Ws::new(rng.chance(50));
        for _ in 0..rng.range(0, 4) {
            assert!(ws
                .send(*rng.pick(AGENTS), &rng.content(), &["--mailbox"])
                .0
                .status
                .success());
        }
        let before = ws.snapshot();
        let recipient = rng.pick(&["w1", "w2", "w3", "leader", "ghost"]).to_string();
        let limit = rng.range(0, 5).to_string();
        let first = ws.cli(&["inbox", &recipient, "-n", &limit, "--json"]);
        let second = ws.cli(&["inbox", &recipient, "-n", &limit, "--json"]);
        assert!(first.status.success(), "{}", text(&first));
        assert_eq!(first.stdout, second.stdout, "reads must be repeatable");
        for escape in ["../escape", "../../etc", "w1/../../x"] {
            let output = ws.cli(&["inbox", escape, "--json"]);
            assert_not_a_crash(&output);
            assert!(
                !output.status.success(),
                "inbox {escape} must refuse: {}",
                text(&output)
            );
        }
        assert_eq!(ws.snapshot(), before, "inbox reads mutated the workspace");
        assert!(!ws.root.join("escape").exists() && !ws.root.join(".team/escape").exists());
        ws.assert_no_legacy_artifacts();
    });
}

/// A live raw-mode pane recording every byte it receives; it is both the leader
/// and `w1` of a single-team snapshot, so any paste into either is observable.
struct Receiver {
    tmux: PathBuf,
    socket: PathBuf,
    received: PathBuf,
}

impl Receiver {
    fn start(ws: &mut Ws) -> Self {
        let tmux = tmux_binary();
        ws.path_env = format!("{}:/usr/bin:/bin", tmux.parent().unwrap().display());
        let socket = ws.root.join("t.sock");
        let received = ws.root.join("received");
        let ready = ws.root.join("ready");
        let script = ws.root.join("receiver.py");
        std::fs::write(
            &script,
            format!(
                "import os, tty\ntty.setraw(0)\nos.write(1, b'\\x1b[?2004h')\n\
                 out = open({received:?}, 'ab', buffering=0)\nopen({ready:?}, 'w').close()\n\
                 while True:\n    out.write(os.read(0, 65536))\n",
                received = received.display().to_string(),
                ready = ready.display().to_string()
            ),
        )
        .unwrap();
        let created = Command::new(&tmux)
            .arg("-S")
            .arg(&socket)
            .args([
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-s",
                "s1recv",
            ])
            .arg(format!("python3 '{}'", script.display()))
            .env_remove("TMUX")
            .output()
            .unwrap();
        assert!(created.status.success(), "{}", text(&created));
        let receiver = Self {
            tmux,
            socket: socket.clone(),
            received,
        };
        let pane = String::from_utf8(created.stdout)
            .unwrap()
            .trim()
            .to_string();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            assert!(Instant::now() < deadline, "receiver did not start");
            std::thread::sleep(Duration::from_millis(10));
        }
        std::fs::write(
            ws.root.join(".team/runtime/state.json"),
            serde_json::to_vec(&json!({
                "active_team_key": TEAM,
                "teams": {TEAM: {"team_id": TEAM, "status": "stopped", "tmux_socket": socket,
                    "leader": {"pane_id": pane},
                    "agents": {"w1": {"status": "running", "pane_id": pane}, "w2": {"status": "stopped"}, "w3": {"status": "stopped"}}}}
            }))
            .unwrap(),
        )
        .unwrap();
        receiver
    }

    fn pasted(&self, needle: &str) -> usize {
        let bytes = std::fs::read(&self.received).unwrap_or_default();
        bytes
            .windows(needle.len())
            .filter(|w| *w == needle.as_bytes())
            .count()
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        let _ = Command::new(&self.tmux)
            .arg("-S")
            .arg(&self.socket)
            .arg("kill-server")
            .output();
    }
}

/// Physical delivery into a live pane: a retried send (same message id, then
/// its record token) is answered from the durable record and never pastes again.
#[test]
fn physical_retry_never_submits_the_same_message_twice() {
    let mut ws = Ws::new(true);
    let receiver = Receiver::start(&mut ws);
    let payload = format!("stage1 physical once {}", std::process::id());
    let pasted = || receiver.pasted(&payload);
    let (output, first) = ws.send("w1", &payload, &["--message-id", "phys-1"]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(first["delivery"], "physical", "{first}");
    let deadline = Instant::now() + Duration::from_secs(5);
    while pasted() == 0 {
        assert!(
            Instant::now() < deadline,
            "first send never reached the pane"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let token = first["dedupe_key"].as_str().unwrap().to_string();
    for retry in ["phys-1", token.as_str()] {
        let (output, value) = ws.send("w1", &payload, &["--message-id", retry]);
        assert!(output.status.success(), "{}", text(&output));
        assert_eq!(value["duplicate"], true, "{value}");
        assert_ne!(
            value["delivery"], "physical",
            "retry {retry} was physically resubmitted: {value}"
        );
    }
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(pasted(), 1, "the pane received the message more than once");
    assert_eq!(contents(&ws.inbox("w1")), [payload]);
    ws.assert_no_legacy_artifacts();
}

/// The public `tmux` resolved from the caller's PATH (or `CONTRACT_TMUX` in CI).
fn tmux_binary() -> PathBuf {
    std::env::var_os("CONTRACT_TMUX")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|dir| dir.join("tmux"))
                .find(|path| path.is_file())
                .expect("tmux is required for the physical delivery invariant")
        })
}

// ───────────────────────── c) fail-closed safety ─────────────────────────

#[test]
fn prop_retired_verbs_refuse_with_a_named_replacement_and_no_state_change() {
    const RETIRED: &[(&str, &str)] = &[
        ("attach-leader", "claim-leader"),
        ("takeover", "claim-leader"),
        ("coordinator", "schedule"),
    ];
    for_each_case("retired verbs", 12, |rng, _| {
        let ws = Ws::new(rng.chance(50));
        assert!(ws.send("w1", "seed", &["--mailbox"]).0.status.success());
        let before = ws.snapshot();
        let (verb, replacement) = *rng.pick(RETIRED);
        let mut args = vec![verb.to_string()];
        if rng.chance(50) {
            args.push("--json".into());
        }
        // `coordinator` parses its (empty) option set first; extra words get a usage refusal.
        if verb != "coordinator" {
            for _ in 0..rng.range(0, 2) {
                args.push(
                    rng.pick(&["--confirm", "w1", "--team", TEAM, "--yes"])
                        .to_string(),
                );
            }
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = ws.cli(&refs);
        assert_not_a_crash(&output);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {}", text(&output));
        let message = text(&output);
        assert!(
            message.contains("retired") && message.contains(replacement),
            "{args:?}: {message}"
        );
        assert_eq!(ws.snapshot(), before, "{args:?} mutated the workspace");
        ws.assert_no_legacy_artifacts();
    });
}

#[test]
fn removed_verbs_are_unknown_commands_without_side_effects() {
    let ws = Ws::new(false);
    let before = ws.snapshot();
    for verb in [
        "results",
        "watch",
        "peek",
        "e2e",
        "allow-peer-talk",
        "validate",
        "identity",
        "sessions",
        "wait-ready",
        "preflight",
    ] {
        for json in [false, true] {
            let mut args = vec![verb];
            if json {
                args.push("--json");
            }
            let output = ws.cli(&args);
            assert_not_a_crash(&output);
            assert!(
                !output.status.success(),
                "{verb} must not run: {}",
                text(&output)
            );
            assert!(
                text(&output).contains("Unknown command"),
                "{verb}: {}",
                text(&output)
            );
            assert!(
                text(&output).contains("team-agent --help"),
                "{verb}: refusal must point to help"
            );
        }
    }
    assert_eq!(ws.snapshot(), before);
    ws.assert_no_legacy_artifacts();
}

#[test]
fn wait_refuses_immediately_without_fifo_database_or_state_change() {
    for json in [false, true] {
        let ws = Ws::new(json);
        let before = ws.snapshot();
        let started = Instant::now();
        let mut args = vec!["wait", "--task", "task-stage1"];
        if json {
            args.push("--json");
        }
        let output = ws.cli(&args);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "wait blocked instead of refusing"
        );
        assert_not_a_crash(&output);
        assert!(
            !output.status.success(),
            "wait must refuse: {}",
            text(&output)
        );
        if json {
            assert_eq!(parse(&output)["ok"], false);
        }
        assert_eq!(ws.snapshot(), before, "wait mutated the workspace");
        ws.assert_no_legacy_artifacts();
    }
}

#[test]
fn send_usage_refusals_are_fail_closed() {
    let ws = Ws::new(true);
    let before = ws.records();
    for args in [
        vec!["send", "w1", "", "--json"],
        vec!["send", "--pane", "%1", "hi", "--json"],
        vec!["send", "w1", "hi", "--pane", "%1", "--json"],
        vec![
            "send",
            "w1",
            "hi",
            "--message-id",
            "ss1:not-a-token",
            "--mailbox",
            "--json",
        ],
        vec!["send", "w1,,w2", "hi", "--mailbox", "--json"],
        vec!["send", "--to-leader", "ghost-leader", "hi", "--json"],
    ] {
        let output = ws.cli(&args);
        assert_not_a_crash(&output);
        assert!(!output.status.success(), "{args:?}: {}", text(&output));
        assert_eq!(parse(&output)["ok"], false, "{args:?}");
    }
    assert_eq!(
        ws.records(),
        before,
        "usage refusals wrote records or state"
    );
    ws.assert_no_legacy_artifacts();
}

#[test]
fn prop_mcp_surface_keeps_three_tools_and_unknown_tools_fail_closed() {
    use team_agent::mcp_server::{handle_mcp, TeamOrchestratorTools};
    use team_agent::model::ids::{AgentId, TeamKey};
    const RETIRED: &[&str] = &[
        "assign_task",
        "update_state",
        "stop_agent",
        "reset_agent",
        "add_agent",
        "clone_agent",
        "fork_agent",
        "request_human",
        "stuck_list",
        "stuck_cancel",
        "wait_for_result",
        "get_result",
        "collect_results",
        "ack_message",
        "mark_delivered",
        "check_inbox",
        "inject",
        "rescue_enter",
    ];
    for_each_case("mcp unknown tools", 16, |rng, case| {
        let ws = Ws::new(rng.chance(50));
        let agent = *rng.pick(&["leader", "w1", "w2"]);
        let tools = TeamOrchestratorTools::with_identity(
            &ws.root,
            Some(AgentId::new(agent)),
            Some(TeamKey::new(TEAM)),
        );
        let before = ws.snapshot();
        let rpc = |request: Value| {
            serde_json::to_value(handle_mcp(&tools, &request).unwrap().unwrap()).unwrap()
        };
        let listed = rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}));
        let mut names: Vec<String> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect();
        names.sort();
        assert_eq!(names, ["get_team_status", "report_result", "send_message"]);
        let name = if rng.chance(70) {
            rng.pick(RETIRED).to_string()
        } else {
            format!("{}_{}", rng.slug(4), rng.slug(3))
        };
        let arguments = rng
            .pick(&[
                json!({}),
                json!({"to": "w1", "content": "x"}),
                json!({"task_id": "t", "agent_id": agent}),
                Value::Null,
            ])
            .clone();
        let frame = rpc(
            json!({"jsonrpc": "2.0", "id": case + 2, "method": "tools/call", "params": {"name": name, "arguments": arguments}}),
        );
        assert_eq!(frame["result"]["isError"], true, "{name}: {frame}");
        let body: Value =
            serde_json::from_str(frame["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["ok"], false, "{name}: {body}");
        assert_eq!(body["reason"], "unknown_tool", "{name}: {body}");
        assert_eq!(
            ws.snapshot(),
            before,
            "unknown tool {name} had side effects"
        );
        ws.assert_no_legacy_artifacts();
    });
}

/// Short, team-qualified and workspace-qualified TO forms are co-equal: each enters
/// the same logical resolver and lands in the same recipient inbox, in order.
#[test]
fn prop_short_and_fully_qualified_targets_land_in_the_same_inbox() {
    for_each_case("co-equal TO forms", 8, |rng, _| {
        let ws = Ws::new(rng.chance(50));
        let agent = *rng.pick(AGENTS);
        let mut expected = Vec::new();
        for _ in 0..rng.range(3, 6) {
            let target = match rng.below(3) {
                0 => agent.to_string(),
                1 => format!("{TEAM}/{agent}"),
                _ => format!("{}::{TEAM}/{agent}", ws.root.display()),
            };
            let content = rng.content();
            let (output, value) = ws.send(&target, &content, &["--mailbox"]);
            assert!(output.status.success(), "{target}: {}", text(&output));
            assert_eq!(value["recipient"], agent, "{target}: {value}");
            expected.push(content);
        }
        assert_eq!(contents(&ws.inbox(agent)), expected);
        ws.assert_no_legacy_artifacts();
    });
}

/// A worker seat's MCP send can never widen its spawn-time team or workspace scope,
/// whatever the address says; refusals write nothing anywhere.
#[test]
fn mcp_seat_send_cannot_widen_team_or_workspace_scope() {
    use team_agent::mcp_server::{Scope, TeamOrchestratorTools};
    use team_agent::messaging::MessageTarget;
    use team_agent::model::ids::{AgentId, TeamKey};
    let ws = Ws::new(true);
    let other = Ws::new(false);
    let state = json!({
        "active_team_key": TEAM,
        "teams": {
            TEAM: {"team_id": TEAM, "status": "stopped", "agents": {"w1": {"status": "stopped"}, "w2": {"status": "stopped"}}},
            "beta": {"team_id": "beta", "status": "stopped", "agents": {"b1": {"status": "stopped"}}}
        }
    });
    for root in [&ws.root, &other.root] {
        std::fs::write(
            root.join(".team/runtime/state.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
    }
    let seat = TeamOrchestratorTools::with_identity(
        &ws.root,
        Some(AgentId::new("w1")),
        Some(TeamKey::new(TEAM)),
    );
    let mailbox = Value::Bool(true);
    let send = |to: &str, scope: Option<Scope>| {
        seat.send_message_with_presentation(
            &MessageTarget::Single(to.into()),
            "scope probe",
            None,
            None,
            scope,
            Some(&mailbox),
            None,
        )
    };
    assert!(
        send("w2", None).is_ok(),
        "positive control: in-team seat send must be admitted"
    );
    let before = (ws.records(), other.records());
    for target in [
        "beta/b1".to_string(),
        format!("{}::beta/b1", ws.root.display()),
        format!("{}::{TEAM}/w2", other.root.display()),
    ] {
        let refused = send(&target, None);
        assert!(
            refused.is_err(),
            "seat widened scope to {target}: {refused:?}"
        );
    }
    assert!(
        send("w2", Some(Scope::Workspace)).is_err(),
        "workspace scope override must be refused"
    );
    assert_eq!(
        (ws.records(), other.records()),
        before,
        "a refused widening wrote records"
    );
    assert!(
        !ws.root.join(".team/inbox/beta").exists(),
        "no foreign-team inbox may be created"
    );
    ws.assert_no_legacy_artifacts();
}

/// Durable non-injected routes (CLI/MCP mailbox, non-critical stage_result reports with a
/// casefile or silent sink) are stored and pullable but never pasted into a live pane; a
/// plain send to the same pane is (positive control).
#[test]
fn casefile_silent_and_stage_result_are_stored_but_never_pasted() {
    use team_agent::mcp_server::TeamOrchestratorTools;
    use team_agent::messaging::MessageTarget;
    use team_agent::model::ids::{AgentId, TeamKey};
    let mut ws = Ws::new(true);
    let receiver = Receiver::start(&mut ws);
    let tag = std::process::id();
    let casefile = format!("stage1 casefile {tag}");
    let mcp_mailbox = format!("stage1 mcp mailbox {tag}");
    let stage_casefile = format!("stage1 stage-result casefile {tag}");
    let stage_silent = format!("stage1 stage-result silent {tag}");
    let control = format!("stage1 live control {tag}");
    let (output, value) = ws.send("w1", &casefile, &["--mailbox"]);
    assert!(
        output.status.success() && value["live"] == false,
        "{}",
        text(&output)
    );
    // send_message's durable non-injected route is the mailbox flag (casefile sink).
    let leader = TeamOrchestratorTools::with_identity(
        &ws.root,
        Some(AgentId::new("leader")),
        Some(TeamKey::new(TEAM)),
    );
    let mailboxed = leader.send_message_with_presentation(
        &MessageTarget::Single("w1".into()),
        &mcp_mailbox,
        None,
        None,
        None,
        Some(&Value::Bool(true)),
        None,
    );
    assert!(mailboxed.is_ok(), "{mailboxed:?}");
    // Non-critical stage_result reports honor the requested casefile/silent sink.
    let worker = TeamOrchestratorTools::with_identity(
        &ws.root,
        Some(AgentId::new("w1")),
        Some(TeamKey::new(TEAM)),
    );
    for (summary, sink) in [(&stage_casefile, "casefile"), (&stage_silent, "silent")] {
        let reported = worker.report_result_with_presentation(
            None,
            Some(summary),
            Some("success"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&json!({"sink": sink, "class": "stage_result", "case_id": "stage1-case"})),
        );
        assert!(reported.is_ok(), "{sink}: {reported:?}");
    }
    let (output, value) = ws.send("w1", &control, &[]);
    assert!(
        output.status.success() && value["delivery"] == "physical",
        "{}",
        text(&output)
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while receiver.pasted(&control) == 0 {
        assert!(
            Instant::now() < deadline,
            "positive control never reached the pane"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(300));
    for quiet in [&casefile, &mcp_mailbox, &stage_casefile, &stage_silent] {
        assert_eq!(
            receiver.pasted(quiet),
            0,
            "non-leader sink was pasted: {quiet}"
        );
    }
    let w1 = contents(&ws.inbox("w1"));
    assert!(
        w1.contains(&casefile) && w1.contains(&mcp_mailbox) && w1.contains(&control),
        "{w1:?}"
    );
    let leader_inbox = contents(&ws.inbox("leader"));
    assert!(
        leader_inbox.contains(&stage_casefile) && leader_inbox.contains(&stage_silent),
        "stage results must stay pullable: {leader_inbox:?}"
    );
    ws.assert_no_legacy_artifacts();
}
