//! Issue #283 independent public CLI contracts. Frozen baseline: 86a4d431.
//! No new product API is assumed; Initial RED must be behavioral, not compilation failure.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const OVERRIDE: &str = "TEAM_AGENT_CLI_ARGV_ROUTING";
const KEYS: &[&str] = &["claude", "codex", "copilot", "gemini_cli", "grok", "cursor_agent", "pi"];
static SEQ: AtomicU64 = AtomicU64::new(0);
struct Fixture { root: PathBuf, home: PathBuf, cwd: PathBuf }
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ta-argv-cli-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
        let home = root.join("home");
        let cwd = root.join("empty-cwd");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        Self { root, home, cwd }
    }
    fn config(&self) -> PathBuf { self.home.join(".team-agent/argv-routing.json") }
    fn seed(&self, value: &str) {
        fs::create_dir_all(self.config().parent().unwrap()).unwrap();
        fs::write(self.config(), value).unwrap();
    }
    fn command(&self, args: &[&str], env: Option<&str>) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        cmd.current_dir(&self.cwd).args(args).env("HOME", &self.home).env_remove(OVERRIDE).env_remove("USERPROFILE");
        for key in hermetic::CALLER_IDENTITY_ENVS { cmd.env_remove(key); }
        if let Some(value) = env { cmd.env(OVERRIDE, value); }
        cmd
    }
    fn run(&self, args: &[&str], env: Option<&str>) -> Output {
        let out = self.command(args, env).output().unwrap();
        assert!(!self.cwd.join(".team").exists(), "route must never create cwd .team; args={args:?}, {}", describe(&out));
        out
    }
    fn ok(&self, args: &[&str], env: Option<&str>) -> Value {
        let out = self.run(args, env);
        assert!(out.status.success(), "expected route success, args={args:?}, {}", describe(&out));
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("JSON result: {e}: {}", describe(&out)));
        assert_eq!(v["ok"], true, "{v}");
        v
    }
    fn show(&self, provider: &str) -> Value { self.ok(&["route", "show", provider, "--json"], None) }
    fn disk(&self) -> Value { serde_json::from_slice(&fs::read(self.config()).unwrap()).unwrap() }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); } }
fn describe(out: &Output) -> String {
    format!("rc={:?} stdout={:?} stderr={:?}", out.status.code(), String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}
fn success_shape(v: &Value, f: &Fixture, operation: &str) {
    assert_eq!(v["operation"], operation);
    assert_eq!(v["config_path"], f.config().to_string_lossy().as_ref());
    for field in ["persisted_enabled", "effective_enabled"] { assert!(v[field].is_boolean(), "missing {field}: {v}"); }
    assert!(["env", "config", "default"].contains(&v["enabled_source"].as_str().unwrap()));
    assert!(["unset", "on", "off", "invalid"].contains(&v["override_status"].as_str().unwrap()));
}

#[test]
fn a01_queries_and_clear_missing_are_read_only_in_empty_cwd_without_provider() {
    let f = Fixture::new();
    for args in [vec!["route", "--json"], vec!["route", "status", "--json"], vec!["route", "show", "--json"], vec!["route", "show", "pi", "--json"], vec!["route", "clear", "pi", "--json"]] {
        let mut cmd = f.command(&args, None);
        cmd.env("PATH", f.root.join("no-installed-tools"));
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "route has no installed-provider/tmux requirement: {}", describe(&out));
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["ok"], true);
        assert!(!f.home.join(".team-agent").exists(), "read/no-op must not initialize route home");
        assert!(!f.cwd.join(".team").exists());
    }
}

#[test]
fn a03_env_precedence_mutation_observability_and_cross_cwd_persistence() {
    let f = Fixture::new();
    let enabled = f.ok(&["route", "enable", "--json"], Some(" OFF "));
    success_shape(&enabled, &f, "enable");
    assert_eq!(enabled["persisted_enabled"], true);
    assert_eq!(enabled["effective_enabled"], false);
    assert_eq!(enabled["enabled_source"], "env");
    assert_eq!(enabled["override_status"], "off");
    let disabled = f.ok(&["route", "disable", "--json"], Some(" TrUe "));
    assert_eq!(disabled["persisted_enabled"], false);
    assert_eq!(disabled["effective_enabled"], true);
    for env in ["1", "true", "ON", " on "] { assert_eq!(f.ok(&["route", "status", "--json"], Some(env))["effective_enabled"], true); }
    for env in ["0", "false", "OFF", " off "] { assert_eq!(f.ok(&["route", "status", "--json"], Some(env))["effective_enabled"], false); }
    for env in ["", "maybe", "2"] {
        let v = f.ok(&["route", "status", "--json"], Some(env));
        assert_eq!(v["effective_enabled"], false);
        assert_eq!(v["override_status"], "invalid");
    }
    f.ok(&["route", "enable", "--json"], None);
    let other = f.root.join("other-new-project"); fs::create_dir(&other).unwrap();
    let out = f.command(&["route", "status", "--json"], None).current_dir(&other).output().unwrap();
    assert!(out.status.success());
    assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap()["effective_enabled"], true);
    assert!(!other.join(".team").exists());
    assert_eq!(f.disk()["enabled"], true);
}

#[test]
fn a04_crud_preserves_order_duplicates_other_mappings_and_enabled_bit() {
    let f = Fixture::new();
    f.ok(&["route", "enable", "--json"], None);
    f.ok(&["route", "set", "pi", "--json", "--", "--mode", "rpc"], None);
    f.ok(&["route", "set", "grok", "--json", "--", "x y", ""], None);
    f.ok(&["route", "add", "pi", "--json", "--", "rpc", "--mode"], None);
    assert_eq!(f.show("pi")["argv"], json!(["--mode", "rpc", "rpc", "--mode"]));
    f.ok(&["route", "set", "pi", "--json", "--", "replacement"], None);
    assert_eq!(f.show("pi")["argv"], json!(["replacement"]));
    assert_eq!(f.show("grok")["argv"], json!(["x y", ""]));
    f.ok(&["route", "add", "codex", "--json", "--", "new", "new"], None);
    assert_eq!(f.show("codex")["argv"], json!(["new", "new"]));
    for _ in 0..2 { f.ok(&["route", "clear", "pi", "--json"], None); }
    assert_eq!(f.show("pi")["configured"], false);
    assert_eq!(f.show("pi")["argv"], json!([]));
    assert_eq!(f.disk()["enabled"], true);
    f.ok(&["route", "disable", "--json"], None);
    assert_eq!(f.show("grok")["argv"], json!(["x y", ""]));
}

#[test]
fn a04_bad_usage_is_nonzero_without_workspace_or_config_mutation() {
    let f = Fixture::new();
    f.seed(r#"{"schema_version":1,"enabled":false,"providers":{"pi":["keep"]}}"#);
    let before = fs::read(f.config()).unwrap();
    for args in [vec!["route", "typo", "--json"], vec!["route", "set", "pi", "--json"], vec!["route", "set", "pi", "--json", "--"], vec!["route", "set", "pi", "--json", "--args", "x"], vec!["route", "set", "fake", "--json", "--", "x"], vec!["route", "set", "/bin/pi", "--json", "--", "x"], vec!["route", "show", "typo", "--json"], vec!["route", "status", "--json", "--json"], vec!["route", "clear", "pi", "extra", "--json"]] {
        let out = f.run(&args, None);
        assert!(!out.status.success(), "invalid route accepted: {args:?}");
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["ok"], false);
        for field in ["error", "reason", "action"] { assert!(v[field].is_string(), "{args:?}: {v}"); }
        assert!(v.get("log").is_none_or(Value::is_null), "no fabricated log: {v}");
        assert_eq!(fs::read(f.config()).unwrap(), before);
    }
}

#[test]
fn a05_literal_payload_and_delimiter_survive_cli_and_json() {
    let f = Fixture::new();
    let dangerous = format!("$(touch {})", f.root.join("MUST-NOT-EXIST").display());
    let payload = ["", " a b ", "路由🚀", "'quoted'", "\"double\"", "line\nbreak", "; echo bad", "`echo bad`", "{workspace}", "~/*", "--help", "-h", "--json", "--no-display", "--", "--workspace", "other", &dangerous];
    let mut args = vec!["route", "set", "pi", "--json", "--"]; args.extend(payload);
    f.ok(&args, None);
    assert_eq!(f.disk()["providers"]["pi"], json!(payload));
    assert_eq!(f.show("pi")["argv"], json!(payload));
    assert!(!f.root.join("MUST-NOT-EXIST").exists());
    // No control-area --json: payload must neither activate JSON nor trigger help.
    let out = f.run(&["route", "set", "pi", "--", "--help", "--json", "--no-display"], None);
    assert!(out.status.success(), "{}", describe(&out));
    assert!(serde_json::from_slice::<Value>(&out.stdout).is_err(), "payload --json is data, not output option");
    assert_eq!(f.disk()["providers"]["pi"], json!(["--help", "--json", "--no-display"]));
}

#[test]
fn a04_control_help_never_reads_or_writes_invalid_config() {
    let f = Fixture::new(); f.seed("bad JSON");
    let before = fs::read(f.config()).unwrap();
    for args in [vec!["route", "--help"], vec!["route", "set", "--help"]] {
        assert!(f.run(&args, None).status.success());
        assert_eq!(fs::read(f.config()).unwrap(), before);
    }
}

#[test]
fn a06_aliases_canonicalize_without_changing_provider_wire_contract() {
    let f = Fixture::new();
    for (alias, canonical) in [("claude_code", "claude"), ("claude-code", "claude"), ("agent", "cursor_agent"), ("cursor", "cursor_agent")] {
        let v = f.ok(&["route", "set", alias, "--json", "--", alias], None);
        assert_eq!(v["provider"], canonical);
        assert_eq!(f.show(alias)["argv"], json!([alias]));
        assert_eq!(f.disk()["providers"][canonical], json!([alias]));
    }
    for key in f.disk()["providers"].as_object().unwrap().keys() { assert!(KEYS.contains(&key.as_str())); }
}

#[test]
fn a11_strict_management_rejects_malformed_schema_nul_and_duplicate_keys() {
    let f = Fixture::new();
    for raw in ["[]", "{}", r#"{"schema_version":2,"enabled":true,"providers":{}}"#, r#"{"schema_version":1,"enabled":true,"providers":{},"extra":0}"#, r#"{"schema_version":1,"enabled":true,"providers":{"fake":["x"]}}"#, r#"{"schema_version":1,"enabled":true,"providers":{"claude_code":["x"]}}"#, r#"{"schema_version":1,"enabled":true,"providers":{"pi":[7]}}"#, r#"{"schema_version":1,"enabled":true,"providers":{"pi":["\u0000"]}}"#, r#"{"schema_version":1,"enabled":true,"enabled":false,"providers":{}}"#, r#"{"schema_version":1,"enabled":true,"providers":{"pi":["a"],"pi":["b"]}}"#] {
        f.seed(raw);
        for verb in ["status", "disable"] {
            let out = f.run(&["route", verb, "--json"], None);
            assert!(!out.status.success(), "strict management accepted {raw}: {}", describe(&out));
            assert_eq!(fs::read_to_string(f.config()).unwrap(), raw, "never repair by overwriting invalid user config");
        }
    }
}

#[test]
fn a11_unavailable_home_does_not_fall_back_to_workspace() {
    let f = Fixture::new();
    for home in ["", "relative-home"] {
        let out = f.command(&["route", "enable", "--json"], None).env("HOME", home).output().unwrap();
        assert!(!out.status.success(), "unavailable home must reject mutation");
        assert!(!f.cwd.join(".team").exists());
        assert!(!f.cwd.join(".team-agent").exists());
        assert!(!f.cwd.join("relative-home").exists());
    }
}

#[test]
fn a12_parallel_mutations_are_lossless_and_readers_only_see_complete_json() {
    let f = Fixture::new(); f.ok(&["route", "enable", "--json"], None);
    let mut children = Vec::new();
    for key in KEYS { for token in ["a", "b", "c"] { children.push(f.command(&["route", "add", key, "--json", "--", token], None).spawn().unwrap()); } }
    while children.iter_mut().any(|child| child.try_wait().unwrap().is_none()) {
        let raw = fs::read(f.config()).unwrap();
        let v: Value = serde_json::from_slice(&raw).expect("reader must never observe truncated/partial config");
        assert_eq!(v["schema_version"], 1);
        std::thread::yield_now();
    }
    for mut child in children { assert!(child.wait().unwrap().success()); }
    let v = f.disk();
    for key in KEYS {
        let mut tokens = v["providers"][key].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect::<Vec<_>>();
        tokens.sort(); assert_eq!(tokens, ["a", "b", "c"], "no lost update for {key}");
    }
    assert_eq!(v["enabled"], true);
}

#[test]
fn a12_lock_timeout_preserves_config_stable_lock_and_unowned_temp() {
    let f = Fixture::new(); f.seed(r#"{"schema_version":1,"enabled":true,"providers":{"pi":["keep"]}}"#);
    let lock_path = f.config().with_file_name("argv-routing.lock");
    let lock = fs::OpenOptions::new().create(true).truncate(false).write(true).open(&lock_path).unwrap();
    assert!(team_agent::platform::file_lock::try_lock_once_nonblocking(&lock).unwrap());
    let foreign = f.config().with_file_name("argv-routing.foreign.tmp"); fs::write(&foreign, "foreign").unwrap();
    let before = fs::read(f.config()).unwrap();
    let out = f.run(&["route", "set", "pi", "--json", "--", "new"], None);
    assert!(!out.status.success(), "mutation must fail on held lock");
    assert_eq!(fs::read(f.config()).unwrap(), before);
    assert_eq!(fs::read_to_string(&foreign).unwrap(), "foreign");
    assert!(lock_path.exists(), "stable lock must not be removed");
    team_agent::platform::file_lock::unlock(&lock).unwrap();
}

#[test]
fn a12_unwritable_parent_and_unreadable_config_fail_honestly() {
    let f = Fixture::new();
    // A directory at config path deterministically fails even for root; no chmod/root ambiguity.
    fs::create_dir_all(f.config()).unwrap();
    let out = f.run(&["route", "disable", "--json"], None);
    assert!(!out.status.success()); assert!(f.config().is_dir());
    fs::remove_dir(f.config()).unwrap();
    fs::remove_dir(f.home.join(".team-agent")).unwrap();
    fs::write(f.home.join(".team-agent"), "owned sentinel").unwrap();
    assert!(!f.run(&["route", "enable", "--json"], None).status.success());
    assert_eq!(fs::read_to_string(f.home.join(".team-agent")).unwrap(), "owned sentinel");
}

#[cfg(unix)]
#[test]
fn a03_nonunicode_override_is_invalid_not_persisted_or_expanded() {
    use std::os::unix::ffi::OsStringExt;
    let f = Fixture::new(); f.seed(r#"{"schema_version":1,"enabled":true,"providers":{}}"#);
    let out = f.command(&["route", "status", "--json"], None).env(OVERRIDE, std::ffi::OsString::from_vec(vec![0xff])).output().unwrap();
    assert!(out.status.success(), "{}", describe(&out));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["override_status"], "invalid"); assert_eq!(v["effective_enabled"], false);
    assert_eq!(f.disk()["enabled"], true);
}

#[allow(dead_code)]
fn _path_marker(_: &Path) {}
