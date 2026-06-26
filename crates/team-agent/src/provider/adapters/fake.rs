//! Fake provider — scripted worker built into the team-agent CLI itself.
//! Extracted from `provider/adapter.rs` (0.4.x decoupling step 2).

pub(crate) fn fake_worker_command() -> Vec<String> {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.into_os_string().into_string().ok())
        .unwrap_or_else(|| "team-agent".to_string());
    vec![
        exe,
        "fake-worker".to_string(),
        "--workspace".to_string(),
        "{workspace}".to_string(),
        "--agent-id".to_string(),
        "{agent_id}".to_string(),
    ]
}
