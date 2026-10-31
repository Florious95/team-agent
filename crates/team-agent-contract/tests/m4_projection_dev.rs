//! Developer-owned M4 contract probes; independent of Track B's hidden tests.
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);
use team_agent_contract::{
    contract::types::{ScopeId, SeatId},
    orchestration::store::ContractStore,
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let base = std::env::temp_dir().canonicalize().unwrap();
        Self(
            base.join(format!(
                "m4-projection-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
            )),
        )
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn creating_a_contract_store_does_not_create_sqlite_authority() {
    let root = Root::new();
    let scope = ScopeId::new("m4-projection").unwrap();
    let store = ContractStore::create(&root.0, scope.clone(), "m4-native").unwrap();
    assert!(store.seats().unwrap().is_empty());
    assert!(
        !root.0.join("contract.db").exists(),
        "M4 must not create SQLite authority"
    );
    drop(store);
    let reopened = ContractStore::open_readonly(&root.0, scope, "m4-native").unwrap();
    assert!(reopened.seats().unwrap().is_empty());
}

#[test]
fn identity_proposals_are_read_only_and_scope_fenced() {
    let root = Root::new();
    let scope = ScopeId::new("m4-projection").unwrap();
    let store = ContractStore::create(&root.0, scope.clone(), "m4-native").unwrap();
    let seat = SeatId::new("worker").unwrap();
    let a = store.propose_identity(&seat).unwrap();
    let b = store.propose_identity(&seat).unwrap();
    assert_eq!(a.generation.0, 1);
    assert_eq!(b.generation.0, 1);
    assert_ne!(a.instance, b.instance);
    assert!(
        ContractStore::open_readonly(&root.0, ScopeId::new("foreign").unwrap(), "m4-native")
            .is_err()
    );
    assert!(
        !root.0.join("contract.db").exists(),
        "identity allocation must not use SQLite"
    );
}
