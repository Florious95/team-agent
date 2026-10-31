//! Metadata/control-fence regressions. Controlled fixtures, not native-provider acceptance.
#![cfg(unix)]
mod physical_support;
use std::time::Duration;
use team_agent_contract::{
    contract::{fork::NativeControl, types::*},
    host::{clock::RealClock, command::OutputLimits, tmux::HostLimits},
    orchestration::{lifecycle::LifecycleHost, physical::{PhysicalRuntime, PhysicalSettings, Registration}, store::{SeatRecord, SeatStatus}, Error},
};

fn seat() -> SeatRecord {
    let target = physical_support::target();
    SeatRecord {
        identity: target.identity().clone(), provider: target.provider.clone(), native: target.native.clone(),
        cwd: target.cwd.clone(), endpoint: target.endpoint.to_string_lossy().into_owned(), pane: target.pane.clone(),
        binding_key: target.binding.clone(), profile: "physical-fixture".into(), server_key: "fixture-server".into(),
        evidence_kind: target.evidence_kind, status: SeatStatus::Ready, process: Some(target.process.identity.clone()),
        physical: Some(target), session: None, resources: vec![], workspace_resources: vec![], bootstrap_used: false,
    }
}

#[test]
fn physical_receipt_must_match_the_address_and_complete_seat_authority() {
    let current = seat();
    assert!(current.physical_target().is_ok());
    let mutations: &[fn(&mut SeatRecord)] = &[
        |s| s.endpoint.push_str("-replacement"), |s| s.pane = "%9".into(),
        |s| s.binding_key.push_str("-replacement"), |s| s.process = None,
        |s| s.process.as_mut().unwrap().birth_identity.push_str("-replacement"),
        |s| s.provider = ProviderId::new("other-provider").unwrap(),
        |s| s.native.ui.push_str("-replacement"), |s| s.cwd.identity = Digest([99; 32]),
        |s| s.identity.generation = Generation(99), |s| s.evidence_kind = EvidenceKind::Native,
    ];
    for mutate in mutations {
        let mut changed = current.clone();
        mutate(&mut changed);
        assert!(changed.physical_target().unwrap_err().is_fence());
    }
}

#[test]
fn foreign_controls_refuse_before_lookup_or_host_access() {
    let seat = seat();
    let descriptor = physical_support::descriptor("single", &seat.native);
    let hooks = physical_support::hooks();
    let clock = RealClock::new();
    let mut host = PhysicalRuntime::new(Registration { descriptor: &descriptor, hooks: &hooks,
        controls: &[], startup: None, session_panel: None }, PhysicalSettings {
        tmux: "/nonexistent-m4-control-tmux".into(),
        limits: HostLimits { command_output: OutputLimits { stdout: 4096, stderr: 4096, stdin: 4096 },
            max_script_bytes: 4096, max_capture_bytes: 4096, max_payload_bytes: 4096, max_executable_bytes: 4096,
            poll_interval: Duration::from_millis(10), columns: 80, rows: 24 },
        action_budget: Duration::from_secs(1), freshness: Duration::from_secs(1), journal_bytes: 4096,
    }, OperationId::new("foreign-control").unwrap(), &clock).unwrap();
    host.executing_candidate = Some(Digest([99; 32]));
    assert!(matches!(host.control(&seat, &NativeControl::Exit), Err(Error::Invalid("seat_requires_restart"))));
    assert!(host.evidence.is_empty(), "do not manufacture old-candidate evidence");
}
