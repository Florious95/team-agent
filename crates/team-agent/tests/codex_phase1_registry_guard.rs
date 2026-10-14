//! purpose: lock Codex host registration for phase 3
//! contract: descriptor("codex") is the phase-1 leaf, and legacy seats stay off contract dispatch
//! boundary: registration is not launch admission and is not G1–G5

#[test]
fn codex_host_registration_keeps_legacy_seats_off_contract_dispatch() {
    use team_agent::contract_runtime::registry::{contract_dispatch, descriptor, recognizes};

    let codex = descriptor("codex").expect("codex leaf is registered");
    assert_eq!(codex.identity.id, "codex");
    assert_eq!(codex.identity.display_name, "Codex CLI (contract)");
    assert_eq!(codex.identity.binary, "codex");
    assert!(codex.identity.aliases.is_empty());
    assert!(recognizes("codex"));
    assert!(recognizes(" Codex "));
    assert!(descriptor("Codex").is_none());
    assert!(!contract_dispatch("codex", false));
    assert!(contract_dispatch("codex", true));
    assert!(contract_dispatch("cursor", false));
    assert!(!contract_dispatch("grok", false));
    assert!(!contract_dispatch("pi", false));
}
