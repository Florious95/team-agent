//! S1a second-pass verifier guards (frozen by verifier).
//!
//! Anchors two new car-A surfaces verified at candidate 488fd553:
//! 1. the single non-migrating optional read ingress
//!    `StateRepository::load_workspace_if_exists_without_migrations`
//!    (state/repository.rs), and
//! 2. typed reapply route parity: `McpUpdateStateNote { team_key: Some(_) }`
//!    must resolve to `ReapplyScope::Team`, while `McpAssignTask` must stay
//!    out of the Team match set and therefore resolve to `ReapplyScope::Root`.

#![allow(clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::PathBuf;

fn repository_source() -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join("src/state/repository.rs"))
        .expect("state/repository.rs must exist for the S1a read/route guards")
}

#[test]
fn raw_read_facade_is_the_single_non_migrating_read_ingress() {
    let repository = repository_source();
    assert!(
        repository.contains("pub fn load_workspace_if_exists_without_migrations"),
        "S1a-2A: the non-migrating optional read ingress must live on the \
         repository authority (state/repository.rs)"
    );
}

#[test]
fn reapply_route_parity_pins_update_state_note_team_and_assign_task_root() {
    let repository = repository_source();
    let start = repository
        .find("fn reapply_scope(")
        .expect("S1a-2B: typed reapply_scope router must exist");
    let section = &repository[start..];
    let end = section
        .find("fn route_reapply")
        .expect("S1a-2B: route_reapply consumer must follow reapply_scope");
    let section = &section[..end];
    let team_arm = &section[..section
        .find("ReapplyScope::Team")
        .expect("S1a-2B: reapply_scope must contain a Team resolution arm")];
    assert!(
        team_arm.contains("McpUpdateStateNote") && team_arm.contains("team_key: Some(_)"),
        "S1a-2B: McpUpdateStateNote with a team key must reapply at Team scope; \
         section={section}"
    );
    assert!(
        !team_arm.contains("McpAssignTask"),
        "S1a-2B: McpAssignTask must NOT be routed to Team scope; it must fall \
         through to the Root reapply arm; section={section}"
    );
    assert!(
        section.contains("ReapplyScope::Root"),
        "S1a-2B: the fall-through Root arm must exist; section={section}"
    );
}
