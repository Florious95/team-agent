mod physical_support;
use physical_support::*;
use std::time::Duration;
use team_agent_contract::contract::probe::*;
use team_agent_contract::contract::types::*;
use team_agent_contract::runtime::probes::*;

fn server() -> InstanceId {
    InstanceId::new("server-one").unwrap()
}
fn event(sequence: u64, fact: ProtocolFact) -> ProtocolEvent {
    ProtocolEvent {
        scope: target().scope(
            sequence,
            Duration::from_millis(sequence),
            Duration::from_millis(100),
            "controlled-protocol-event",
        ),
        fact,
    }
}
fn bound() -> ClientBindingEvidence {
    ClientBindingEvidence {
        server_key: "fixture-server".into(),
        tools: TEAM_TOOLS.to_vec(),
        source: ClientBindingSource::NativeClientDiscovery,
    }
}
fn snapshot(events: &[ProtocolEvent], now: u64) -> ProtocolSnapshot {
    collect_protocol(
        &target(),
        &server(),
        "fixture-server",
        events,
        Duration::from_millis(now),
        Duration::from_millis(100),
    )
}
fn call() -> CallIdentity {
    CallIdentity {
        call_id: "call-one".into(),
        tool: LogicalTool::SendMessage,
        message: Some(MessageId::new("m-one").unwrap()),
        attempt: Some(AttemptId::new("attempt-one").unwrap()),
    }
}

#[test]
fn configuration_and_server_handshake_do_not_fabricate_client_binding() {
    let configured = snapshot(&[event(1, ProtocolFact::Configured)], 2);
    assert!(matches!(
        configured.server.outcome,
        ProbeOutcome::Unknown(_)
    ));
    assert!(matches!(
        configured.binding.outcome,
        ProbeOutcome::Unknown(_)
    ));
    let events = [
        event(
            1,
            ProtocolFact::InitializeResponseWritten {
                server_instance: server(),
            },
        ),
        event(
            2,
            ProtocolFact::ToolsListResponseWritten {
                server_instance: server(),
            },
        ),
    ];
    let value = snapshot(&events, 3);
    assert!(matches!(
        value.server.outcome,
        ProbeOutcome::Observed(ServerHandshakeEvidence {
            initialize_response_written: true,
            tools_list_response_written: true,
            ..
        })
    ));
    assert!(matches!(value.binding.outcome, ProbeOutcome::Unknown(_)));
    assert!(value.calls.is_empty());
}

#[test]
fn different_server_instances_never_merge_handshake_or_binding_evidence() {
    let other = InstanceId::new("server-other").unwrap();
    let events = [
        event(
            1,
            ProtocolFact::InitializeResponseWritten {
                server_instance: server(),
            },
        ),
        event(
            2,
            ProtocolFact::ToolsListResponseWritten {
                server_instance: other.clone(),
            },
        ),
        event(
            3,
            ProtocolFact::ClientBound {
                server_instance: other,
                evidence: bound(),
            },
        ),
    ];
    let value = snapshot(&events, 4);
    assert!(matches!(
        value.server.outcome,
        ProbeOutcome::Observed(ServerHandshakeEvidence {
            initialize_response_written: true,
            tools_list_response_written: false,
            ..
        })
    ));
    assert!(matches!(value.binding.outcome, ProbeOutcome::Unknown(_)));
}

#[test]
fn current_exit_and_unbind_counters_are_not_erased_by_ttl_expiry() {
    let events = [
        event(
            1,
            ProtocolFact::ClientBound {
                server_instance: server(),
                evidence: bound(),
            },
        ),
        event(
            2,
            ProtocolFact::ClientUnbound {
                server_instance: server(),
                server_key: "fixture-server".into(),
            },
        ),
        event(
            3,
            ProtocolFact::ServerExited {
                server_instance: server(),
                pid: 42,
                exit_code: Some(1),
            },
        ),
    ];
    let value = snapshot(&events, 1000);
    assert!(matches!(value.server.outcome, ProbeOutcome::Negative(_)));
    assert!(matches!(value.binding.outcome, ProbeOutcome::Negative(_)));
}

#[test]
fn equal_sequence_unbind_wins_over_bound_regardless_of_event_order() {
    let positive = event(
        5,
        ProtocolFact::ClientBound {
            server_instance: server(),
            evidence: bound(),
        },
    );
    let negative = event(
        5,
        ProtocolFact::ClientUnbound {
            server_instance: server(),
            server_key: "fixture-server".into(),
        },
    );
    for events in [
        vec![positive.clone(), negative.clone()],
        vec![negative.clone(), positive.clone()],
    ] {
        assert!(matches!(
            snapshot(&events, 10).binding.outcome,
            ProbeOutcome::Negative(_)
        ));
    }
}

#[test]
fn generation_endpoint_binding_and_session_fences_ignore_foreign_events() {
    for field in 0..4 {
        let mut wrong = event(
            1,
            ProtocolFact::ClientBound {
                server_instance: server(),
                evidence: bound(),
            },
        );
        match field {
            0 => wrong.scope.identity.generation = Generation(2),
            1 => wrong.scope.endpoint = "/foreign.sock".into(),
            2 => wrong.scope.binding = "foreign-binding".into(),
            _ => {
                wrong.scope.session = Some(
                    team_agent_contract::contract::session::NativeSessionId::new("foreign-session")
                        .unwrap(),
                )
            }
        }
        assert!(matches!(
            snapshot(&[wrong], 2).binding.outcome,
            ProbeOutcome::Unknown(_)
        ));
    }
}

#[test]
fn invocation_and_response_written_are_not_client_consumption_or_presentation() {
    let events = [
        event(
            1,
            ProtocolFact::Call {
                server_instance: server(),
                identity: call(),
                fact: RoundTripFact::InvocationReceived,
            },
        ),
        event(
            2,
            ProtocolFact::Call {
                server_instance: server(),
                identity: call(),
                fact: RoundTripFact::ResponseWritten,
            },
        ),
    ];
    let value = snapshot(&events, 3);
    assert_eq!(value.calls.len(), 1);
    let ProbeOutcome::Observed(evidence) = &value.calls[0].outcome else {
        panic!("missing call evidence");
    };
    assert_eq!(
        evidence.facts,
        vec![
            RoundTripFact::InvocationReceived,
            RoundTripFact::ResponseWritten
        ]
    );
    assert!(!evidence
        .facts
        .contains(&RoundTripFact::ClientConsumptionObserved));
    assert!(matches!(value.binding.outcome, ProbeOutcome::Unknown(_)));
}

#[test]
fn call_id_with_conflicting_message_attribution_remains_unknown() {
    let mut other = call();
    other.message = Some(MessageId::new("another-message").unwrap());
    let value = snapshot(
        &[
            event(
                1,
                ProtocolFact::Call {
                    server_instance: server(),
                    identity: call(),
                    fact: RoundTripFact::InvocationReceived,
                },
            ),
            event(
                2,
                ProtocolFact::Call {
                    server_instance: server(),
                    identity: other,
                    fact: RoundTripFact::ResponseWritten,
                },
            ),
        ],
        3,
    );
    assert!(matches!(value.calls[0].outcome, ProbeOutcome::Unknown(_)));
}

#[test]
fn uncorrelated_call_is_not_assigned_to_a_latest_message() {
    let mut id = call();
    id.message = None;
    id.attempt = None;
    let value = snapshot(
        &[event(
            1,
            ProtocolFact::Call {
                server_instance: server(),
                identity: id,
                fact: RoundTripFact::InvocationReceived,
            },
        )],
        2,
    );
    let ProbeOutcome::Observed(evidence) = &value.calls[0].outcome else {
        panic!("missing invocation");
    };
    assert!(evidence.message.is_none());
    assert!(evidence.attempt.is_none());
}

#[test]
fn stale_positive_events_do_not_keep_a_binding_ready() {
    let value = snapshot(
        &[event(
            1,
            ProtocolFact::ClientBound {
                server_instance: server(),
                evidence: bound(),
            },
        )],
        1000,
    );
    assert!(matches!(value.binding.outcome, ProbeOutcome::Unknown(_)));
}
