use std::path::Path;

use crate::lifecycle::launch::pi_mcp::{pi_cleanup_plan, pi_seat_paths, PiCleanupAction};

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
