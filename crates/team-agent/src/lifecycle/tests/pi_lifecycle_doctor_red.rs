use std::path::{Path, PathBuf};

use crate::lifecycle::launch::pi_mcp::{pi_cleanup_plan, pi_seat_paths, PiCleanupAction};


#[test]
fn pi_stop_retains_backing_and_never_scans_by_process_name() {
    let paths = pi_seat_paths(Path::new("/workspace"), "team-a", "worker-a");
    // Stage 3 test-chief ruling: stop retention excludes destructive Pi cleanup.
    assert!(!include_str!("../restart/agent.rs").contains("pi_cleanup_plan"), "pi stop must never route through destructive Pi cleanup; backing is retained by construction");
    assert!(!include_str!("../../contract_runtime/commands.rs").contains("pi_cleanup_plan"), "commands stop must never route through destructive Pi cleanup");

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
