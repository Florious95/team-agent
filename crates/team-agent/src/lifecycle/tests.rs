#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

static SEQ: AtomicU32 = AtomicU32::new(0);

/// 隔离的空 workspace。仿 sibling(event_log.rs)风格,不依赖 tempfile dev-dep。
fn temp_ws() -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let ws = std::env::temp_dir().join(format!("ta_rs_lc_{}_{}", std::process::id(), n));
    std::fs::create_dir_all(&ws).unwrap();
    ws
}

fn aid(s: &str) -> AgentId {
    AgentId::new(s)
}
fn sess(s: &str) -> SessionName {
    SessionName(s.to_string())
}

pub(crate) fn test_binary_path() -> &'static str {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        let path = if let Ok(path) = std::env::var("CARGO_BIN_EXE_team-agent") {
            path
        } else {
            let current = std::env::current_exe().expect("test executable path");
            current
            .parent()
            .and_then(|deps| deps.parent())
            .map(|target| target.join("team-agent"))
            .expect("team-agent test binary path")
            .to_string_lossy()
            .into_owned()
        };
        assert!(
            std::path::Path::new(&path).is_file(),
            "team-agent test binary does not exist: {path}; run `cargo build -p team-agent --bin team-agent` first"
        );
        path
    })
    .as_str()
}

mod agent_ops;
mod core;
mod lane_ops;
mod launch_spawn;
mod lifecycle_lock;
mod main_preserved;
mod phase_b_contracts;
mod phase_golden;
mod restart;
mod b0_legacy_snapshot_nonauthority_contract;
mod b0_reader_hideone_audit_contract;
mod behavioral_diff_264_red;
mod claude_profile_launch_red;
mod codex_weak_window_attribution_red;
mod copilot_provider_red;
mod display_adaptive_red;
mod core_034_real_red;
mod f032_startup_prompt_best_effort_red;
mod harvest2_a_batch_red;
mod host_cotenant_death_p0_contract;
mod lifecycle_rollback_red;
mod quick_start_worker_readiness_red;
mod realmachine_clusters_1_6_red;
mod realmachine_residual_g1_g4_red;
mod restart_build_before_destroy_0540_contract;
mod restart_liveness_red;
mod restart_rebind_hotfix_252_red;
mod restart_session_capture_red;
mod resume_recover_red;
mod stale_team_saveconflict_contract;
mod startup_latency_contract;
mod status_credential_redaction_contract;
mod subprep_codex_trust_roundtrip_red;
mod swallow_batch4_semantics_red;
mod team_in_team_identity_scope_red;
mod team_in_team_sibling_quick_start_red;
mod team_in_team_state_scope_red;
mod team_key_retirement_roster_red;
mod team_key_retirement_txn_red;
mod test_isolation_escape_contract;
mod upgrade_compat_0211_red;
mod verify_rs031_window_consistency_red;
mod worker_spawn_env_red;
mod acceptance_b_batch_red;
mod claude_compatible_config_red;
mod grok_mcp_overlay_red;
mod grok_require_explicit_model_red;
mod communication_mode_runtime_contract_red;
mod clone_fork_copilot_perms_red;
