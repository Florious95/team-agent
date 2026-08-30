use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::lifecycle::launch::pi_mcp::{
    parse_pi_list_models_table, validate_pi_adapter_identity, validate_pi_executable_chain,
    validate_pi_wrapper_source, write_pi_wrapper, PiAdapterIdentity, PiExecutableChain,
    PiExecutableFileType, PiWrapperRequest,
};
use crate::provider::McpConfig;

const PACKAGE_DIGEST: &str = "ce8b8b6154e83e9732c58bd993e7ed69390617616f4f0e7330274d5ee9e2f620";
const INDEX_DIGEST: &str = "16d260ac25b66346baab6ecef76680324336953bafc7be8cf95b3df5c611b89e";
const CATALOG_DIGEST: &str = "726cedb6c3f6fe80a0d7b98918d8ed5063695e01f510a48f46c4bad5daab49fe";
static NEXT_ROOT: AtomicU32 = AtomicU32::new(0);

fn temp_root(label: &str) -> PathBuf {
    let seq = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "team-agent-pi-{label}-{}-{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create fixture root");
    root
}

fn adapter_identity(root: &Path) -> PiAdapterIdentity {
    let package_root = root.join("pi-mcp-adapter");
    std::fs::create_dir_all(&package_root).expect("create adapter package root");
    let package_json = package_root.join("package.json");
    let index_ts = package_root.join("index.ts");
    std::fs::write(&package_json, b"{}").expect("write adapter package receipt");
    std::fs::write(&index_ts, b"export const createMcpAdapter = () => {};\n")
        .expect("write loadable adapter entry");
    PiAdapterIdentity {
        package_name: "pi-mcp-adapter".to_string(),
        version: "2.30.0".to_string(),
        extension_entry: "./index.ts".to_string(),
        package_json,
        index_ts,
        package_json_sha256: PACKAGE_DIGEST.to_string(),
        index_ts_sha256: INDEX_DIGEST.to_string(),
    }
}

fn mcp_config(candidate: &Path, workspace: &Path, agent_id: &str) -> McpConfig {
    McpConfig {
        raw: serde_json::json!({
            "team_orchestrator": {
                "command": candidate,
                "args": ["mcp-server"],
                "env": {
                    "TEAM_AGENT_WORKSPACE": workspace,
                    "TEAM_AGENT_ID": agent_id,
                    "TEAM_AGENT_AGENT_ID": agent_id,
                    "TEAM_AGENT_OWNER_TEAM_ID": "team-a",
                    "TEAM_AGENT_AUTH_MODE": "subscription"
                }
            }
        }),
    }
}

#[test]
fn pi_executable_chain_freezes_wrapper_real_binary_catalog_and_plugin_identity() {
    let root = temp_root("executable-chain");
    let chain = PiExecutableChain {
        path_entry: PathBuf::from("/Users/fixture/.local/bin/pi"),
        path_entry_type: PiExecutableFileType::Wrapper,
        launch_executable: PathBuf::from("/Users/fixture/.local/bin/pi"),
        real_binary: PathBuf::from(
            "/opt/homebrew/lib/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js",
        ),
        pi_version: "0.84.3".to_string(),
        catalog_sha256: CATALOG_DIGEST.to_string(),
        adapter: adapter_identity(&root),
    };
    validate_pi_executable_chain(&chain).expect("the protocol-capable wrapper chain is valid");

    let mut homebrew_symlink_entry = chain.clone();
    homebrew_symlink_entry.real_binary = PathBuf::from("/opt/homebrew/bin/pi");
    validate_pi_executable_chain(&homebrew_symlink_entry)
        .expect("the distinct Homebrew symlink entry may resolve to the shebang cli.js target");

    let mut wrong_version = chain.clone();
    wrong_version.pi_version = "0.84.4".to_string();
    validate_pi_executable_chain(&wrong_version)
        .expect("an arbitrary newer Pi version with the same public protocol is valid");

    let mut missing_real = chain.clone();
    missing_real.real_binary = PathBuf::new();
    assert!(validate_pi_executable_chain(&missing_real).is_err());

    let mut wrong_catalog = chain.clone();
    wrong_catalog.catalog_sha256 = "00".repeat(32);
    assert!(validate_pi_executable_chain(&wrong_catalog).is_err());

    let mut direct_binary = chain.clone();
    direct_binary.path_entry = PathBuf::from("/opt/homebrew/bin/pi");
    direct_binary.path_entry_type = PiExecutableFileType::Binary;
    direct_binary.launch_executable = direct_binary.path_entry.clone();
    direct_binary.real_binary = direct_binary.path_entry.clone();
    assert!(
        validate_pi_executable_chain(&direct_binary).is_err(),
        "a direct Homebrew binary must not replace the verified PATH wrapper launch entry"
    );

    let mut wrong_plugin = chain;
    wrong_plugin.adapter.version = "2.31.0".to_string();
    wrong_plugin.adapter.package_json_sha256 = "11".repeat(32);
    wrong_plugin.adapter.index_ts_sha256 = "22".repeat(32);
    validate_pi_executable_chain(&wrong_plugin)
        .expect("adapter version and content digests are diagnostic observations only");

    assert!(parse_pi_list_models_table(b"provider model\nopenai-codex gpt-5.6-luna\n").is_ok());
    assert!(parse_pi_list_models_table(b"provider model\n").is_err());
    assert!(parse_pi_list_models_table(b"provider model\nmalformed\n").is_err());
    std::fs::remove_dir_all(root).expect("remove executable chain fixture");
}

#[test]
fn pi_adapter_detector_requires_exact_2_30_0_entry_and_digest() {
    let root = temp_root("adapter-identity");
    let exact = adapter_identity(&root);
    validate_pi_adapter_identity(&exact).expect("protocol-capable adapter identity");

    let mut wrong_name = exact.clone();
    wrong_name.package_name = "pi-mcp-adapter-fork".to_string();
    assert!(validate_pi_adapter_identity(&wrong_name).is_err());

    let mut missing_entry = exact.clone();
    missing_entry.extension_entry.clear();
    missing_entry.index_ts = PathBuf::new();
    assert!(validate_pi_adapter_identity(&missing_entry).is_err());

    let mut unloadable_entry = exact.clone();
    unloadable_entry.index_ts = root.join("pi-mcp-adapter/missing.ts");
    assert!(validate_pi_adapter_identity(&unloadable_entry).is_err());

    let mut newer_observation = exact;
    newer_observation.version = "9.7.3".to_string();
    newer_observation.package_json_sha256 = "11".repeat(32);
    newer_observation.index_ts_sha256 = "22".repeat(32);
    validate_pi_adapter_identity(&newer_observation)
        .expect("version and digests do not define adapter protocol capability");
    std::fs::remove_dir_all(root).expect("remove adapter identity fixture");
}

#[test]
fn pi_wrapper_is_atomic_per_seat_and_embeds_exact_candidate() {
    let root = temp_root("wrapper");
    let workspace = root.join("workspace");
    let candidate = root.join("candidate/team-agent");
    std::fs::create_dir_all(candidate.parent().expect("candidate parent"))
        .expect("create candidate parent");
    std::fs::write(&candidate, b"candidate").expect("write candidate fixture");
    let destination = workspace.join(".team/runtime/pi/team-a/worker-a/team-mcp.ts");
    let adapter = adapter_identity(&root);

    let written = write_pi_wrapper(PiWrapperRequest {
        destination: &destination,
        adapter: &adapter,
        candidate_executable: &candidate,
        mcp_config: &mcp_config(&candidate, &workspace, "worker-a"),
        team_id: "team-a",
        agent_id: "worker-a",
        workspace: &workspace,
        include_tools: &["send_message", "report_result"],
    })
    .expect("atomic wrapper write");
    assert_eq!(written, destination);
    assert_eq!(
        std::fs::metadata(&written)
            .expect("wrapper metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let source = std::fs::read_to_string(&written).expect("read wrapper");
    assert!(source.contains(adapter.index_ts.to_string_lossy().as_ref()));
    assert!(source.contains(candidate.to_string_lossy().as_ref()));
    assert!(source.contains("TEAM_AGENT_ID") && source.contains("worker-a"));
    assert!(source.contains("TEAM_AGENT_OWNER_TEAM_ID") && source.contains("team-a"));
    assert!(source.contains("lifecycle") && source.contains("lazy"));
    assert!(source.contains("directTools") && source.contains("false"));
    assert!(source.contains("toolPrefix") && source.contains("server"));
    assert!(source.contains("includeTools"));
    assert!(source.contains("send_message") && source.contains("report_result"));
    assert!(!source.contains("command: \"team-agent\""));
    assert!(!source.contains("--mcp-config"));

    let entries = std::fs::read_dir(destination.parent().expect("wrapper parent"))
        .expect("list wrapper parent")
        .map(|entry| entry.expect("entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(
        entries,
        [std::ffi::OsString::from("team-mcp.ts")],
        "atomic write must not leave a temporary or backup sibling"
    );
    std::fs::remove_dir_all(root).expect("remove wrapper fixture");
}

#[test]
fn pi_wrapper_import_failure_has_no_ambient_merge_fallback() {
    let root = temp_root("wrapper-negative");
    let workspace = root.join("workspace");
    let candidate = root.join("candidate/team-agent");
    std::fs::create_dir_all(candidate.parent().expect("candidate parent"))
        .expect("create candidate parent");
    std::fs::write(&candidate, b"candidate").expect("write candidate fixture");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::write(
        workspace.join(".mcp.json"),
        r#"{"mcpServers":{"team_orchestrator":{"command":"/tmp/ambient-evil"}}}"#,
    )
    .expect("write malicious ambient config");

    let destination = workspace.join(".team/runtime/pi/team-a/worker-a/team-mcp.ts");
    let adapter = adapter_identity(&root);
    write_pi_wrapper(PiWrapperRequest {
        destination: &destination,
        adapter: &adapter,
        candidate_executable: &candidate,
        mcp_config: &mcp_config(&candidate, &workspace, "worker-a"),
        team_id: "team-a",
        agent_id: "worker-a",
        workspace: &workspace,
        include_tools: &["send_message", "report_result"],
    })
    .expect("isolated wrapper ignores ambient config");
    let source = std::fs::read_to_string(&destination).expect("wrapper source");
    assert!(!source.contains("ambient-evil"));
    validate_pi_wrapper_source(&source, &adapter, &candidate)
        .expect("exact isolated import and candidate");

    let broken = source.replace(
        adapter.index_ts.to_string_lossy().as_ref(),
        "/missing/pi-mcp-adapter/index.ts",
    );
    let error = validate_pi_wrapper_source(&broken, &adapter, &candidate)
        .expect_err("broken import must refuse");
    let text = error.to_string();
    assert!(
        text.contains("import") || text.contains("adapter"),
        "got {text}"
    );
    assert!(!broken.contains("--mcp-config"));

    let missing_proxy_tools = source.replace("\"includeTools\"", "\"missingTools\"");
    validate_pi_wrapper_source(&missing_proxy_tools, &adapter, &candidate)
        .expect_err("wrapper without the MCP proxy tool allowlist must refuse");

    std::fs::remove_dir_all(root).expect("remove wrapper fixture");
}
