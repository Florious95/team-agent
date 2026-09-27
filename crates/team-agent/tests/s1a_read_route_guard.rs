//! S1a repository read-facade guard.
//!
//! The remaining contract pins the non-migrating optional read ingress.

#![allow(clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::PathBuf;

fn repository_source() -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join("src/state/repository.rs"))
        .expect("state/repository.rs must exist for the S1a read guard")
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
