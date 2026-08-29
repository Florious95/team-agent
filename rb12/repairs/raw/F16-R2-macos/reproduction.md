# F16 R2 macOS bounded reproduction

- Date: 2026-08-29 (Asia/Shanghai).
- Worktree: `/Volumes/nvme/tmp/team-agent-f16-wleak-terminal`.
- Exact HEAD before execution: `86b3aec1065a1e48ce6d647d60e2a10f21be9182`.
- Tree: `35f12d48db7a0df3e848393cf33a63363b925fe9`.
- Accepted F14 `312d19a38330cf9389fa69f915709f6ec7477b1e` is an ancestor (exit 0).
- Evidence directory passed to the process: `rb12/repairs/raw/F16-R2-macos`.

## Command and result

```text
TEAM_AGENT_E2E_EVIDENCE_DIR="$PWD/rb12/repairs/raw/F16-R2-macos" cargo test --locked --package team-agent --test e2e cases::wleak_worker_delivery_socket_leak_contract::wleak_cached_pane_owned_by_other_window_never_receives_worker_message -- --exact --nocapture --test-threads=1
```

- Independent selector output observed by the task runner: exit 0; `running 1 test`; `1 passed; 0 failed; 0 ignored; 0 measured; 67 filtered out`; test duration `6.94s`.
- The bounded clean-target capture is authoritative for diagnosis: exit 101; `running 1 test`; `0 passed; 1 failed; 0 ignored; 0 measured; 67 filtered out`; test duration `7.90s`; total command timing `real 35.59`. Its exact status, source manifest, timeout receipt, and compact event record are under `repro/`.

## Evidence status

The bounded clean-target capture reproduced the CI-observed stage: `timed out after 6s waiting for: message reaches terminal status`, message `msg_52b0f5ed0008`. The receipt records row status `target_resolved`, `delivery_attempts=1`, no `delivered_at`, coordinator tick `1`, and a live target tuple whose pane capture is `TEAM_AGENT_FAKE_READY agent=b`; the sole message event injected into pane `%0` for worker `a`. This identifies a startup pane/state race, not leader-registry causality.

The earlier passing selector is retained only as a non-reproduction. Neither result licenses a timeout increase, retry, or flaky classification.
