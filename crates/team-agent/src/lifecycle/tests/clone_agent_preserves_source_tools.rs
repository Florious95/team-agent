//! ---
//! purpose: 回归——clone-agent 分身 tools 必须与源席集合相等，禁止静默夹成 leader 三件套
//! contract:
//!   provides:
//!     - name: clone_agent_preserves_source_tools
//!       what: 真跑 clone-agent 后，dynamic-role-files / runtime spec 与源席 tools 集合相等
//!   requires:
//!     - name: source-six-set-vs-leader-three-set
//!       what: 同一 team 里 leader 三件套 + 源席六件套的共享冲突面
//! boundary:
//!   - 不扩权：源席故意三件套时分身仍是那三件
//!   - 不改 add-agent / approval / clone 产品路径
//! maturity: wired
//! ---

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Output;

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

use team_agent::model::yaml::Value;

const TEAM_NAME: &str = "g1ct";
const SOURCE: &str = "src_worker";
const CLONE: &str = "new_worker";
const NARROW: &str = "narrow_src";
const NARROW_CLONE: &str = "narrow_clone";

const SIX: &[&str] = &[
    "execute_bash",
    "fs_list",
    "fs_read",
    "fs_write",
    "mcp_team",
    "provider_builtin",
];
const THREE: &[&str] = &["fs_list", "fs_read", "mcp_team"];

struct Case {
    env: HermeticTestEnv,
    workspace: PathBuf,
}

impl Case {
    fn start() -> Self {
        ensure_team_agent_cli();
        let env = HermeticTestEnv::enter("g1ct");
        let workspace = env.workspace("ws");
        write_team_docs(&workspace);
        Self { env, workspace }
    }

    fn ws(&self) -> &str {
        self.workspace.to_str().expect("ws utf8")
    }

    fn run(&self, args: &[&str]) -> Output {
        self.env.run_cli(&self.workspace, args)
    }

    fn shutdown(&self) {
        let _ = self.run(&[
            "shutdown",
            "--workspace",
            self.ws(),
            "--team",
            TEAM_NAME,
            "--json",
        ]);
    }
}

/// `cargo test --lib` does not build `bin/team-agent`. Hermetic CLI
/// lookup then panics and impl_check.sh sees 1 FAILED. Build the bin
/// into the same CARGO_TARGET_DIR the lib test already uses.
fn ensure_team_agent_cli() {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_team-agent") {
        assert!(
            std::path::Path::new(&path).is_file(),
            "CARGO_BIN_EXE_team-agent missing: {path}"
        );
        return;
    }
    let bin = std::env::current_exe()
        .expect("test exe")
        .parent()
        .and_then(|deps| deps.parent())
        .map(|target| target.join("team-agent"))
        .expect("target/debug/team-agent");
    if bin.is_file() {
        return;
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = std::process::Command::new(&cargo)
        .args([
            "build",
            "-p",
            "team-agent",
            "--bin",
            "team-agent",
            "--manifest-path",
        ])
        .arg(std::path::Path::new(&manifest).join("Cargo.toml"))
        .status()
        .unwrap_or_else(|e| panic!("spawn {cargo} build: {e}"));
    assert!(
        status.success(),
        "{cargo} build -p team-agent --bin team-agent failed: {status}"
    );
    assert!(
        bin.is_file(),
        "team-agent still missing after cargo build: {}",
        bin.display()
    );
}

fn write_team_docs(workspace: &Path) {
    std::fs::create_dir_all(workspace.join("agents")).expect("agents dir");
    std::fs::write(
        workspace.join("TEAM.md"),
        format!(
            "---\nname: {TEAM_NAME}\nobjective: clone-agent tools preservation\nprovider: fake\ndisplay_backend: none\n---\n"
        ),
    )
    .expect("TEAM.md");
    write_role(workspace, SOURCE, SIX);
    write_role(workspace, NARROW, THREE);
}

fn write_role(workspace: &Path, name: &str, tools: &[&str]) {
    let tools_yaml = tools
        .iter()
        .map(|tool| format!("  - {tool}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        workspace.join("agents").join(format!("{name}.md")),
        format!(
            "---\nname: {name}\nrole: {name}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n{tools_yaml}\n---\n\n{name} body.\n"
        ),
    )
    .expect("role doc");
}

fn role_tools(path: &Path) -> BTreeSet<String> {
    let (meta, _) = team_agent::compiler::read_front_matter(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let items = meta
        .get("tools")
        .and_then(Value::as_list)
        .unwrap_or_else(|| panic!("{} missing tools list", path.display()));
    items
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn load_runtime_spec(workspace: &Path) -> Value {
    let runtime = workspace.join(".team").join("runtime");
    let spec = std::fs::read_dir(&runtime)
        .expect("runtime dir")
        .flatten()
        .map(|entry| entry.path().join("team.spec.yaml"))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("runtime spec missing"));
    team_agent::model::yaml::loads(&std::fs::read_to_string(&spec).expect("spec"))
        .expect("parse spec")
}

fn yaml_tool_set(node: &Value) -> BTreeSet<String> {
    node.get("tools")
        .and_then(Value::as_list)
        .unwrap_or_else(|| panic!("tools list missing"))
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn spec_tools(workspace: &Path, agent: &str) -> BTreeSet<String> {
    let parsed = load_runtime_spec(workspace);
    let agents = parsed
        .get("agents")
        .and_then(Value::as_list)
        .expect("spec agents");
    for row in agents {
        if row.get("id").and_then(Value::as_str) == Some(agent) {
            return yaml_tool_set(row);
        }
    }
    panic!("agent {agent} missing from runtime spec");
}

fn leader_spec_tools(workspace: &Path) -> BTreeSet<String> {
    yaml_tool_set(
        load_runtime_spec(workspace)
            .get("leader")
            .expect("spec leader"),
    )
}

fn set_of(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn clone_ok(case: &Case, source: &str, dest: &str) {
    let out = case.run(&[
        "clone-agent",
        source,
        "--as",
        dest,
        "--workspace",
        case.ws(),
        "--team",
        TEAM_NAME,
        "--no-display",
        "--json",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "clone-agent {source} --as {dest} must exit 0; code={:?} stderr={stderr} stdout={stdout}",
        out.status.code()
    );
}

/// clone 后分身 role + spec tools 必须等于源席；三件套源不得被扩成六件。
#[test]
fn clone_agent_preserves_source_tools() {
    let case = Case::start();
    let qs = case.run(&[
        "quick-start",
        case.ws(),
        "--workspace",
        case.ws(),
        "--name",
        TEAM_NAME,
        "--yes",
        "--json",
    ]);
    let spec = case
        .workspace
        .join(".team")
        .join("runtime")
        .read_dir()
        .ok()
        .and_then(|entries| {
            entries
                .flatten()
                .find(|e| e.path().join("team.spec.yaml").is_file())
        });
    assert!(
        spec.is_some(),
        "quick-start must leave a runtime spec (fixture team); stderr={} stdout={}",
        String::from_utf8_lossy(&qs.stderr),
        String::from_utf8_lossy(&qs.stdout)
    );

    let source_spec = spec_tools(&case.workspace, SOURCE);
    assert_eq!(
        source_spec,
        set_of(SIX),
        "fixture source must be the six-set"
    );
    let leader_spec = leader_spec_tools(&case.workspace);
    assert_eq!(
        leader_spec,
        set_of(THREE),
        "leader must stay the three-set ceiling so the conflict surface exists; leader={leader_spec:?}"
    );

    clone_ok(&case, SOURCE, CLONE);

    let clone_role = role_tools(
        &case
            .workspace
            .join(".team")
            .join("dynamic-role-files")
            .join(format!("{CLONE}.md")),
    );
    let clone_spec = spec_tools(&case.workspace, CLONE);
    assert_eq!(
        clone_role, source_spec,
        "clone role tools must equal source; source={source_spec:?} clone={clone_role:?}"
    );
    assert_eq!(
        clone_spec, source_spec,
        "clone spec tools must equal source; source={source_spec:?} spec={clone_spec:?}"
    );

    let narrow_src = spec_tools(&case.workspace, NARROW);
    assert_eq!(narrow_src, set_of(THREE), "narrow source fixture");
    clone_ok(&case, NARROW, NARROW_CLONE);
    let narrow_clone = spec_tools(&case.workspace, NARROW_CLONE);
    assert_eq!(
        narrow_clone, narrow_src,
        "narrow clone must keep the three-set (preserve, do not widen); got={narrow_clone:?}"
    );

    case.shutdown();
}
