use std::path::{Path, PathBuf};

use crate::cli::diagnose::{pi_doctor_facts, PiBackingStatus, PiDoctorInput, PiMcpStatus};
use crate::lifecycle::launch::pi_mcp::{pi_cleanup_plan, pi_seat_paths, PiCleanupAction};
use crate::provider::{AuthHintStatus, ClassifySource, TurnState};

fn doctor_input() -> PiDoctorInput {
    PiDoctorInput {
        executable_chain_verified: true,
        pi_version: Some("0.84.3".to_string()),
        adapter_version: Some("2.30.0".to_string()),
        catalog_sha256: Some(
            "726cedb6c3f6fe80a0d7b98918d8ed5063695e01f510a48f46c4bad5daab49fe".to_string(),
        ),
        selected_model: Some("team-agent/qwen3.8-27b".to_string()),
        candidate_executable: Some(PathBuf::from("/candidate/team-agent")),
        wrapper: Some(PathBuf::from(
            "/workspace/.team/runtime/pi/team-a/worker-a/team-mcp.ts",
        )),
        mcp_roundtrip_at: None,
        backing: PiBackingStatus::Pending,
        activity: TurnState::Unknown,
        activity_source: ClassifySource::SessionFile,
        env_key_names: vec![
            "TEAM_AGENT_ID".to_string(),
            "TEAM_AGENT_OWNER_TEAM_ID".to_string(),
        ],
    }
}

#[test]
fn pi_stop_retains_backing_and_never_scans_by_process_name() {
    let paths = pi_seat_paths(Path::new("/workspace"), "team-a", "worker-a");
    let stop = pi_cleanup_plan(PiCleanupAction::Stop, &paths);
    assert!(
        stop.delete_paths.is_empty(),
        "stop must retain both the Team-owned wrapper and session backing"
    );
    assert!(stop.retain_paths.contains(&paths.wrapper));
    assert!(stop.retain_paths.contains(&paths.sessions));
    assert_eq!(
        stop.process_name_scan, None,
        "Pi cleanup may use exact recorded pane/child receipts, never a process-name scan"
    );

    let remove = pi_cleanup_plan(PiCleanupAction::Remove, &paths);
    assert_eq!(remove.delete_paths, [paths.wrapper.clone()]);
    assert!(remove.retain_paths.contains(&paths.sessions));
    assert_eq!(remove.process_name_scan, None);
    assert!(
        remove
            .delete_paths
            .iter()
            .all(|path| path.starts_with(&paths.runtime_root)),
        "remove may delete only exact Team-owned seat paths"
    );
}

#[test]
fn pi_doctor_separates_configured_roundtrip_backing_and_activity_unknown() {
    let facts = pi_doctor_facts(doctor_input());

    assert_eq!(facts.mcp, PiMcpStatus::ConfiguredLazy);
    assert!(
        !facts.mcp_connected,
        "wrapper existence is not an MCP roundtrip"
    );
    assert_eq!(facts.backing, PiBackingStatus::Pending);
    assert_eq!(facts.activity, TurnState::Unknown);
    assert_eq!(facts.activity_source, ClassifySource::SessionFile);
    assert_eq!(facts.auth_hint, AuthHintStatus::Unknown);

    let serialized = serde_json::to_string(&facts).expect("serialize doctor facts");
    assert!(serialized.contains("configured_lazy"));
    assert!(serialized.contains("unknown"));
    for secret_surface in [
        "api_key",
        "access_token",
        "refresh_token",
        "cookie",
        "authorization",
    ] {
        assert!(
            !serialized.to_ascii_lowercase().contains(secret_surface),
            "doctor must expose identity/digest/key names, never credential values"
        );
    }

    let connected = pi_doctor_facts(PiDoctorInput {
        mcp_roundtrip_at: Some("2026-08-29T10:02:00+00:00".to_string()),
        backing: PiBackingStatus::Captured,
        ..doctor_input()
    });
    assert_eq!(connected.mcp, PiMcpStatus::RoundtripVerified);
    assert!(connected.mcp_connected);
    assert_eq!(connected.backing, PiBackingStatus::Captured);
    assert_eq!(connected.activity, TurnState::Unknown);
}
