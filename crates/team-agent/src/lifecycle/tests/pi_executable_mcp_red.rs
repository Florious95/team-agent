use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::lifecycle::launch::pi_mcp::{
    parse_pi_list_models_table, pi_seat_paths, resolve_pi_executable_chain,
    validate_pi_adapter_identity, validate_pi_executable_chain, validate_pi_wrapper_source,
    write_pi_wrapper, write_pi_wrapper_with_publish, PiAdapterIdentity, PiExecutableChain,
    PiExecutableFileType, PiWrapperRequest,
};
use crate::provider::McpConfig;

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

const PACKAGE_DIGEST: &str = "ce8b8b6154e83e9732c58bd993e7ed69390617616f4f0e7330274d5ee9e2f620";
const INDEX_DIGEST: &str = "16d260ac25b66346baab6ecef76680324336953bafc7be8cf95b3df5c611b89e";
const CATALOG_DIGEST: &str = "726cedb6c3f6fe80a0d7b98918d8ed5063695e01f510a48f46c4bad5daab49fe";
static NEXT_ROOT: AtomicU32 = AtomicU32::new(0);
const PI_WRAPPER_CHILD: &str = "TEAM_AGENT_TEST_PI_WRAPPER_CHILD";
const PI_WRAPPER_AMBIENT_CHILD: &str = "TEAM_AGENT_TEST_PI_WRAPPER_AMBIENT_CHILD";
const PI_SYMLINK_CHILD: &str = "TEAM_AGENT_TEST_PI_SYMLINK_CHILD";
const PI_WRAPPER_PARENT_PID: &str = "TEAM_AGENT_TEST_PI_WRAPPER_PARENT_PID";
const PI_WRAPPER_TEST: &str = concat!(
    "lifecycle::tests::pi_executable_mcp_red::",
    "pi_wrapper_is_atomic_per_seat_and_embeds_exact_candidate"
);
const PI_WRAPPER_AMBIENT_TEST: &str = concat!(
    "lifecycle::tests::pi_executable_mcp_red::",
    "pi_wrapper_runtime_registration_does_not_copy_ambient_config"
);
const PI_SYMLINK_TEST: &str = concat!(
    "lifecycle::tests::pi_executable_mcp_red::",
    "pi_standard_npm_symlink_is_a_verified_launch_entry"
);

fn run_process_isolated(marker: &str, test_name: &str, body: impl FnOnce()) {
    if std::env::var_os(marker).is_some() {
        body();
        return;
    }

    let output =
        std::process::Command::new(std::env::current_exe().expect("current lib-test executable"))
            .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
            .env(marker, "1")
            .env(PI_WRAPPER_PARENT_PID, std::process::id().to_string())
            .output()
            .expect("run Pi wrapper send_message fixture in isolated child test process");
    assert!(
        output.status.success(),
        "isolated Pi wrapper child failed: test={test_name} status={:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn assert_isolated_child() {
    let parent_pid = std::env::var(PI_WRAPPER_PARENT_PID)
        .expect("isolated child receives the parent test process id");
    assert_ne!(
        std::process::id().to_string(),
        parent_pid,
        "Pi wrapper send_message fixture must not run in the parent lib-test process"
    );
}

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
fn pi_standard_npm_symlink_is_a_verified_launch_entry() {
    run_process_isolated(PI_SYMLINK_CHILD, PI_SYMLINK_TEST, || {
        assert_isolated_child();
        let root = temp_root("npm-symlink");
        let bin = root.join("bin");
        let later_bin = root.join("later-bin");
        let package_root = root.join("pi-mcp-adapter");
        std::fs::create_dir_all(&bin).expect("create bin");
        std::fs::create_dir_all(&later_bin).expect("create later bin");
        std::fs::create_dir_all(&package_root).expect("create adapter root");
        let real = root.join("cli.js");
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  --version) printf '0.84.4\\n' ;;\n  --list-models) printf 'provider model\\nteam-agent qwen3.8-27b\\n' ;;\n  list) printf 'npm:pi-mcp-adapter\\n{}\\n' ;;\n  *) exit 64 ;;\nesac\n",
            package_root.display()
        );
        std::fs::write(&real, &script).expect("write protocol-capable Pi entry");
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755))
            .expect("make Pi entry executable");
        std::os::unix::fs::symlink(&real, bin.join("pi")).expect("create npm-style symlink");
        std::fs::write(later_bin.join("pi"), script).expect("write later Pi wrapper");
        std::fs::set_permissions(later_bin.join("pi"), std::fs::Permissions::from_mode(0o755))
            .expect("make later Pi wrapper executable");
        std::fs::write(
            package_root.join("package.json"),
            br#"{"name":"pi-mcp-adapter","version":"2.30.0","pi":{"extensions":["./index.ts"]}}"#,
        )
        .expect("write adapter package");
        std::fs::write(
            package_root.join("index.ts"),
            b"export const createMcpAdapter = () => {};\n",
        )
        .expect("write adapter entry");
        unsafe {
            std::env::set_var(
                "PATH",
                std::env::join_paths([&bin, &later_bin]).expect("join fixture PATH"),
            );
        }

        let (chain, models) = resolve_pi_executable_chain()
            .expect("the first PATH Pi entry must define direct Pi behavior");
        assert_eq!(chain.path_entry, bin.join("pi"));
        assert_eq!(chain.path_entry_type, PiExecutableFileType::Symlink);
        assert_eq!(chain.launch_executable, bin.join("pi"));
        assert_eq!(
            chain.real_binary,
            std::fs::canonicalize(&real).expect("canonical real Pi entry")
        );
        assert_eq!(models, ["team-agent/qwen3.8-27b"]);
        std::fs::remove_dir_all(root).expect("remove symlink fixture");
    });
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
    run_process_isolated(PI_WRAPPER_CHILD, PI_WRAPPER_TEST, || {
        let hermetic = HermeticTestEnv::enter("pi-wrapper-send");
        assert_isolated_child();
        pi_wrapper_is_atomic_body(&hermetic);
    });
}

fn pi_wrapper_is_atomic_body(hermetic: &HermeticTestEnv) {
    let root = hermetic.workspace("wrapper");
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
    assert!(source.contains(candidate.to_string_lossy().as_ref()));
    assert!(source.contains("TEAM_AGENT_ID") && source.contains("worker-a"));
    assert!(source.contains("TEAM_AGENT_OWNER_TEAM_ID") && source.contains("team-a"));
    assert!(source.contains("pi-mcp-adapter:runtime-register:v1"));
    assert!(source.contains("session_start") && source.contains("session_shutdown"));
    assert!(source.contains("includeTools"));
    assert!(source.contains("send_message") && source.contains("report_result"));
    assert!(!source.contains("createMcpAdapter"));
    assert!(!source.contains("directTools"));
    assert!(!source.contains("toolPrefix"));
    assert!(!source.contains("\"lifecycle\""));
    assert!(!source.contains("command: \"team-agent\""));
    assert!(!source.contains("--mcp-config"));

    let other_destination = workspace.join(".team/runtime/pi/team-a/worker-b/team-mcp.ts");
    write_pi_wrapper(PiWrapperRequest {
        destination: &other_destination,
        adapter: &adapter,
        candidate_executable: &candidate,
        mcp_config: &mcp_config(&candidate, &workspace, "worker-b"),
        team_id: "team-a",
        agent_id: "worker-b",
        workspace: &workspace,
        include_tools: &["send_message", "report_result"],
    })
    .expect("write second seat wrapper");
    let other_source = std::fs::read_to_string(&other_destination).expect("read second wrapper");
    assert!(source.contains("worker-a") && !source.contains("worker-b"));
    assert!(other_source.contains("worker-b") && !other_source.contains("worker-a"));
    assert_ne!(written, other_destination);

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
fn pi_leader_wrapper_materializes_from_empty_unicode_wsl_workspace() {
    let root = temp_root("empty-unicode-leader");
    let workspace = root.join("mnt/f/测试基础设施");
    let candidate = root.join("candidate/team-agent");
    std::fs::create_dir_all(&workspace).expect("create empty Unicode workspace");
    std::fs::create_dir_all(candidate.parent().expect("candidate parent"))
        .expect("create candidate parent");
    std::fs::write(&candidate, b"candidate").expect("write candidate fixture");
    assert!(
        !workspace.join(".team").exists(),
        "fixture must begin without a .team directory"
    );

    let paths = pi_seat_paths(&workspace, "current", "leader");
    let adapter = adapter_identity(&root);
    let written = write_pi_wrapper(PiWrapperRequest {
        destination: &paths.wrapper,
        adapter: &adapter,
        candidate_executable: &candidate,
        mcp_config: &mcp_config(&candidate, &workspace, "leader"),
        team_id: "current",
        agent_id: "leader",
        workspace: &workspace,
        include_tools: &["assign_task", "send_message"],
    })
    .expect("materialize Pi leader wrapper from an empty Unicode workspace");

    assert_eq!(written, paths.wrapper);
    assert!(written.is_file(), "published leader wrapper must exist");
    assert!(
        paths.runtime_root.is_dir(),
        "leader runtime root must be created recursively"
    );
    assert_eq!(
        std::fs::metadata(&written)
            .expect("leader wrapper metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    std::fs::remove_dir_all(root).expect("remove Unicode leader fixture");
}

#[test]
fn pi_wrapper_permissions_are_finalized_before_atomic_publish() {
    let source = include_str!("../launch/pi_mcp.rs");
    let write_start = source
        .find("pub(crate) fn write_pi_wrapper")
        .expect("write_pi_wrapper source");
    let write_source = &source[write_start..];
    let permissions = write_source
        .find("file.set_permissions")
        .expect("wrapper permissions must use the open temp-file handle");
    let publish = write_source
        .find("publish(&temp, request.destination)")
        .expect("atomic wrapper publish");

    assert!(
        permissions < publish,
        "DrvFS may not resolve the destination immediately after rename; finalize mode through the open temp-file handle before publishing"
    );
    assert!(
        !write_source.contains("std::fs::set_permissions(request.destination"),
        "atomic publish must not require a second destination-path lookup"
    );
}

#[test]
fn pi_wrapper_publish_needs_no_final_path_metadata_lookup() {
    let root = temp_root("publish-path-enoent");
    let workspace = root.join("mnt/f/测试基础设施");
    let candidate = root.join("candidate/team-agent");
    std::fs::create_dir_all(&workspace).expect("create Unicode workspace");
    std::fs::create_dir_all(candidate.parent().expect("candidate parent"))
        .expect("create candidate parent");
    std::fs::write(&candidate, b"candidate").expect("write candidate fixture");

    let paths = pi_seat_paths(&workspace, "current", "leader");
    let published_parent = paths.runtime_root.clone();
    let hidden_parent = published_parent.with_file_name("leader-after-rename");
    let adapter = adapter_identity(&root);
    write_pi_wrapper_with_publish(
        PiWrapperRequest {
            destination: &paths.wrapper,
            adapter: &adapter,
            candidate_executable: &candidate,
            mcp_config: &mcp_config(&candidate, &workspace, "leader"),
            team_id: "current",
            agent_id: "leader",
            workspace: &workspace,
            include_tools: &["assign_task", "send_message"],
        },
        |temp, destination| {
            std::fs::rename(temp, destination)?;
            std::fs::rename(&published_parent, &hidden_parent)?;
            assert_eq!(
                std::fs::metadata(destination)
                    .expect_err("simulate DrvFS final-path metadata ENOENT after rename")
                    .kind(),
                std::io::ErrorKind::NotFound
            );
            Ok(())
        },
    )
    .expect("successful publish must not perform final-path metadata operations");

    let published = hidden_parent.join("team-mcp.ts");
    assert!(published.is_file(), "rename published a complete wrapper");
    assert_eq!(
        std::fs::metadata(&published)
            .expect("published wrapper metadata through converged path")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    std::fs::remove_dir_all(root).expect("remove publish-path fixture");
}

#[test]
fn pi_wrapper_runtime_registration_does_not_copy_ambient_config() {
    run_process_isolated(PI_WRAPPER_AMBIENT_CHILD, PI_WRAPPER_AMBIENT_TEST, || {
        let hermetic = HermeticTestEnv::enter("pi-wrapper-negative-send");
        assert_isolated_child();
        pi_wrapper_import_failure_body(&hermetic);
    });
}

fn pi_wrapper_import_failure_body(hermetic: &HermeticTestEnv) {
    let root = hermetic.workspace("wrapper-negative");
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
    .expect("runtime wrapper leaves ambient config to direct Pi");
    let source = std::fs::read_to_string(&destination).expect("wrapper source");
    assert!(!source.contains("ambient-evil"));
    validate_pi_wrapper_source(&source, &adapter, &candidate)
        .expect("runtime registration and exact candidate");
    assert!(!source.contains("createMcpAdapter"));
    assert!(!source.contains("--mcp-config"));

    let missing_proxy_tools = source.replace("\"includeTools\"", "\"missingTools\"");
    validate_pi_wrapper_source(&missing_proxy_tools, &adapter, &candidate)
        .expect_err("wrapper without the MCP proxy tool allowlist must refuse");

    std::fs::remove_dir_all(root).expect("remove wrapper fixture");
}
