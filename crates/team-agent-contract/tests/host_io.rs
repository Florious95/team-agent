#![cfg(unix)]
mod physical_support;
use physical_support::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use team_agent_contract::contract::delivery::{DeliveryEffect, StepKind, StepOutcome};
use team_agent_contract::contract::plan::{EnvironmentDelta, ResourceWriteEffect};
use team_agent_contract::contract::types::*;
use team_agent_contract::host::command::*;
use team_agent_contract::host::files::*;
use team_agent_contract::host::process::*;
use team_agent_contract::host::shell::quote;
use team_agent_contract::host::tmux::{parse_pane, PaneMetadataStage};
use team_agent_contract::host::transport::PaneMode;
use team_agent_contract::runtime::journal::*;

struct TestScope(ScopedDirectory);
impl std::ops::Deref for TestScope {
    type Target = ScopedDirectory;
    fn deref(&self) -> &ScopedDirectory {
        &self.0
    }
}
impl Drop for TestScope {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "preserved owned host-IO fixture: {}",
                self.0.path().display()
            );
        } else if self.0.check_live().is_ok() {
            std::fs::remove_dir_all(self.0.path()).unwrap();
        }
    }
}
fn scope() -> TestScope {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = format!(
        "tac-k2-io-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    );
    TestScope(ScopedDirectory::create(&PathBuf::from("/tmp").join(name), owner()).unwrap())
}
fn command(script: &str, budget: Duration) -> CommandRequest {
    CommandRequest {
        executable: "/bin/sh".into(),
        arguments: vec!["-c".into(), script.into()],
        cwd: None,
        environment: EnvironmentDelta {
            remove: BTreeSet::new(),
            set: BTreeMap::new(),
        },
        stdin: None,
        reject_stdout: None,
        budget,
        limits: OutputLimits {
            stdout: 65536,
            stderr: 65536,
            stdin: 2 * 1024 * 1024,
        },
    }
}
fn reap(runner: &mut RealCommandRunner) {
    let until = Instant::now() + Duration::from_secs(1);
    while !runner.pending_children().is_empty() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
        runner.reap_owned();
    }
    assert!(runner.pending_children().is_empty());
}

#[test]
fn command_exit_stdout_stderr_and_environment_are_separate() {
    let mut runner = RealCommandRunner::default();
    let mut r = command(
        "printf '%s' \"$CONTRACT_TEST_VALUE\"; printf 'error-output' >&2; exit 7",
        Duration::from_secs(2),
    );
    r.environment
        .set
        .insert("CONTRACT_TEST_VALUE".into(), "quotes ' 中文\nline".into());
    let receipt = runner.run(&r);
    assert_eq!(receipt.end, CommandEnd::Exited);
    assert_eq!(receipt.exit_code, Some(7));
    assert!(!receipt.success());
    assert_eq!(receipt.stdout, "quotes ' 中文\nline".as_bytes());
    assert_eq!(receipt.stderr, b"error-output");
    assert!(receipt.child_reaped);
}

#[test]
fn blocked_stdin_is_bounded_and_cannot_hang_before_the_timeout_loop() {
    let mut runner = RealCommandRunner::default();
    let mut r = command("exec /bin/sleep 5", Duration::from_millis(40));
    r.stdin = Some(vec![b'x'; 1024 * 1024]);
    let now = Instant::now();
    let receipt = runner.run(&r);
    assert_eq!(receipt.end, CommandEnd::TimedOut);
    assert!(receipt.may_have_executed());
    assert!(now.elapsed() < Duration::from_secs(1));
    reap(&mut runner);
}

#[test]
fn an_inherited_output_pipe_does_not_create_an_unbounded_reader_join() {
    let mut runner = RealCommandRunner::default();
    let r = command("(sleep 0.2) & exit 0", Duration::from_millis(30));
    let now = Instant::now();
    let receipt = runner.run(&r);
    assert_eq!(receipt.end, CommandEnd::TimedOut);
    assert_eq!(receipt.exit_code, Some(0));
    assert!(!receipt.success());
    assert!(receipt.child_reaped);
    assert!(now.elapsed() < Duration::from_millis(180));
    // The bounded fixture descendant terminates naturally; the runner never kills unknown PIDs.
    std::thread::sleep(Duration::from_millis(250));
    reap(&mut runner);
}

#[test]
fn output_flood_is_capped_and_spawning_failure_does_not_claim_execution() {
    let mut runner = RealCommandRunner::default();
    let mut r = command(
        "while :; do printf '12345678901234567890'; done",
        Duration::from_secs(2),
    );
    r.limits.stdout = 100;
    let receipt = runner.run(&r);
    assert_eq!(receipt.end, CommandEnd::OutputLimit);
    assert_eq!(receipt.stdout.len(), 100);
    assert!(receipt.may_have_executed());
    reap(&mut runner);
    r.executable = "/definitely-not-a-real-contract-command".into();
    let receipt = runner.run(&r);
    assert_eq!(receipt.end, CommandEnd::NotStarted);
    assert!(!receipt.may_have_executed());
}

#[test]
fn private_directory_lane_is_exclusive_and_can_be_reopened_from_its_receipt() {
    let directory = scope();
    let other = ScopedDirectory::reopen(directory.receipt().clone()).unwrap();
    let first = directory.try_lane().unwrap();
    assert!(other.try_lane().is_err());
    drop(first);
    assert!(other.try_lane().is_ok());
}

#[test]
fn exclusive_files_reject_overwrite_symlink_escape_and_dirty_cleanup() {
    use std::os::unix::fs::symlink;
    let directory = scope();
    let file = directory.create_file("owned.txt", b"owned").unwrap();
    assert!(directory.create_file("owned.txt", b"changed").is_err());
    assert!(directory.create_file("../escape", b"bad").is_err());
    symlink("owned.txt", directory.path().join("alias")).unwrap();
    assert!(directory.read_file("alias", 100).is_err());
    let lane = directory.try_lane().unwrap();
    std::fs::write(directory.path().join("owned.txt"), b"user-edited").unwrap();
    assert!(directory.remove_file(&file, &lane).is_err());
    assert_eq!(
        directory.read_file("owned.txt", 100).unwrap(),
        b"user-edited"
    );
}

#[test]
fn uncertain_file_receipts_never_authorize_removal() {
    let directory = scope();
    let mut file = directory.create_file("maybe", b"bytes").unwrap();
    file.effect = ResourceWriteEffect::MayHaveWritten;
    let lane = directory.try_lane().unwrap();
    assert!(directory.remove_file(&file, &lane).is_err());
    assert!(directory.path().join("maybe").exists());
}

#[test]
fn directory_replacement_invalidates_the_old_receipt() {
    let directory = scope();
    let receipt = directory.receipt().clone();
    let moved = directory.path().with_extension("preserved");
    std::fs::rename(directory.path(), &moved).unwrap();
    std::fs::create_dir(directory.path()).unwrap();
    assert!(directory.check_live().is_err());
    assert!(ScopedDirectory::reopen(receipt).is_err());
    // Only the fixture harness knows it created both exact directories. The product refused them.
    std::fs::remove_dir_all(&moved).unwrap();
    std::fs::remove_dir_all(directory.path()).unwrap();
}

fn metadata(directory: &ScopedDirectory) -> AttemptMetadata {
    AttemptMetadata {
        owner: directory.receipt().owner.clone(),
        attempt: AttemptId::new("journal-attempt").unwrap(),
        correlation: Correlation::Business(MessageId::new("message").unwrap()),
        operation: Operation::OrdinarySend,
        policy_sha256: POLICY,
        payload_sha256: NATIVE,
        payload_bytes: 40,
    }
}
fn record(
    kind: JournalKind,
    ordinal: u64,
    effect: DeliveryEffect,
    step: Option<StepKind>,
    outcome: Option<StepOutcome>,
) -> JournalRecord {
    JournalRecord {
        kind,
        ordinal,
        at: Duration::from_millis(ordinal),
        effect,
        step,
        outcome,
        sequence: Some(ordinal),
        surface: None,
        code: None,
    }
}

#[test]
fn durable_journal_blocks_replay_and_recovers_an_unfinished_input_intent() {
    let directory = scope();
    let metadata = metadata(&directory);
    let mut journal = FileJournal::new(
        ScopedDirectory::reopen(directory.receipt().clone()).unwrap(),
        65536,
    )
    .unwrap();
    assert_eq!(journal.recover(&metadata), RecoveryState::Absent);
    journal.begin(&metadata).unwrap();
    assert_eq!(journal.recover(&metadata), RecoveryState::NoInputIntent);
    journal
        .append(&record(
            JournalKind::ActionIntent,
            1,
            DeliveryEffect::NoEffect,
            Some(StepKind::Paste),
            None,
        ))
        .unwrap();
    assert_eq!(
        journal.recover(&metadata),
        RecoveryState::MayHaveEffect(DeliveryEffect::MayHavePasted)
    );
    let mut reopened = FileJournal::new(
        ScopedDirectory::reopen(directory.receipt().clone()).unwrap(),
        65536,
    )
    .unwrap();
    assert!(reopened.begin(&metadata).is_err());
}

#[test]
fn a_proven_no_effect_result_and_complete_receipt_are_not_an_unresolved_intent() {
    let directory = scope();
    let metadata = metadata(&directory);
    let mut journal = FileJournal::new(
        ScopedDirectory::reopen(directory.receipt().clone()).unwrap(),
        65536,
    )
    .unwrap();
    journal.begin(&metadata).unwrap();
    journal
        .append(&record(
            JournalKind::ActionIntent,
            1,
            DeliveryEffect::NoEffect,
            Some(StepKind::Paste),
            None,
        ))
        .unwrap();
    journal
        .append(&record(
            JournalKind::ActionResult,
            2,
            DeliveryEffect::NoEffect,
            Some(StepKind::Paste),
            Some(StepOutcome::NoEffect),
        ))
        .unwrap();
    journal
        .append(&record(
            JournalKind::Complete,
            3,
            DeliveryEffect::NoEffect,
            None,
            None,
        ))
        .unwrap();
    assert_eq!(
        journal.recover(&metadata),
        RecoveryState::Complete(DeliveryEffect::NoEffect)
    );
}

#[test]
fn torn_journal_keeps_a_prior_submitted_floor_and_never_becomes_replayable() {
    let directory = scope();
    let metadata = metadata(&directory);
    let mut journal = FileJournal::new(
        ScopedDirectory::reopen(directory.receipt().clone()).unwrap(),
        65536,
    )
    .unwrap();
    journal.begin(&metadata).unwrap();
    journal
        .append(&record(
            JournalKind::Accepted,
            1,
            DeliveryEffect::Submitted,
            None,
            None,
        ))
        .unwrap();
    let mut bytes = directory
        .read_file(&journal_name(&metadata), 65536)
        .unwrap();
    bytes.extend_from_slice(b"{\"partial");
    assert_eq!(
        recover_bytes(&metadata, &bytes),
        RecoveryState::UnknownMayHaveEffect {
            floor: DeliveryEffect::Submitted
        }
    );
    assert_eq!(
        recover_bytes(&metadata, b"not-json"),
        RecoveryState::UnknownMayHaveEffect {
            floor: DeliveryEffect::MayHaveSubmitted
        }
    );
}

#[test]
fn posix_quote_preserves_spaces_quotes_unicode_and_newlines() {
    let raw = std::ffi::OsStr::new("a ' b 中文\nnext");
    let quoted = quote(raw).unwrap();
    let mut r = command("", Duration::from_secs(2));
    r.arguments[1] = std::ffi::OsString::from(
        String::from_utf8([b"printf '%s' ".as_slice(), &quoted].concat()).unwrap(),
    );
    let receipt = RealCommandRunner::default().run(&r);
    assert!(receipt.success());
    assert_eq!(receipt.stdout, raw.as_encoded_bytes());
}

#[test]
fn pane_metadata_requires_exact_native_ids_state_and_geometry() {
    let stage = PaneMetadataStage::NewSession;
    let pane = parse_pane(
        b"$1\t@2\t%3\t123\tsession\tworker\t0\t\t0\t\tbinding\t120\t40\n",
        stage,
    )
    .unwrap();
    assert_eq!(pane.address.pid, 123);
    assert!(!pane.dead);
    assert_eq!(pane.columns, 120);
    assert!(parse_pane(b"looks like a pane", stage).is_err());
    assert!(parse_pane(
        b"$1\t@2\t%3\t0\tsession\tworker\t0\t\t0\t\tbinding\t120\t40\n",
        stage,
    )
    .is_err());
}

#[test]
fn pane_metadata_accepts_literal_backslash_t_without_unescaping_native_field_contents() {
    let stage = PaneMetadataStage::NewSession;
    let tabs = "$1\t@2\t%3\t123\tsession\tworker\t0\t\t0\t\tbinding\t120\t40\n";
    let escaped = tabs.replace('\t', r"\t");
    assert_eq!(
        parse_pane(tabs.as_bytes(), stage).unwrap(),
        parse_pane(escaped.as_bytes(), stage).unwrap()
    );
    let named = tabs.replace("session", r"session\ttitle");
    assert_eq!(
        parse_pane(named.as_bytes(), stage)
            .unwrap()
            .address
            .session_name,
        r"session\ttitle"
    );
    // Dual-format support does not accept a mixed frame, extra/missing fields,
    // multiple rows or an invalid native identity/state/geometry.
    for invalid in [
        escaped.replacen(r"\t", "\t", 1),
        escaped.replace(r"\t123\t", r"\t0\t"),
        escaped.replace(r"\t%3\t", r"\t3\t"),
        escaped.replace(r"\tworker\t0\t", r"\tworker\tunknown\t"),
        escaped.replace(r"\t120\t40", r"\t0\t40"),
        escaped.replace(r"\t120\t40", r"\t120"),
        escaped.replace(r"\t120\t40", r"\textra\t120\t40"),
        format!("{escaped}{escaped}"),
    ] {
        assert!(parse_pane(invalid.as_bytes(), stage).is_err());
    }
}

#[test]
fn pane_metadata_native_receipts_preserve_empty_fields() {
    // R3 exact-format and isolated sleep receipts: parser regression only.
    for bytes in [
        b"$0\t@0\t%0\t77438\ttac-a294d53615c4b59461190e6c\tworker\t0\t\t0\t\t\t120\t40\n"
            .as_slice(),
        b"$0\t@0\t%0\t72797\tsleep_test\tworker\t0\t\t0\t\t\t120\t40\n".as_slice(),
    ] {
        for stage in [PaneMetadataStage::NewSession, PaneMetadataStage::Query] {
            let pane = parse_pane(bytes, stage).unwrap();
            assert_eq!(pane.address.session, "$0");
            assert_eq!(pane.address.window, "@0");
            assert_eq!(pane.address.pane, "%0");
            assert_eq!(pane.address.window_name, "worker");
            assert!(!pane.dead);
            assert_eq!(pane.exit_code, None);
            assert_eq!(pane.mode, PaneMode::Normal);
            assert!(pane.binding.is_empty());
            assert_eq!((pane.columns, pane.rows), (120, 40));
        }
    }
}

#[test]
fn pane_metadata_does_not_guess_underscores_after_tmux_sanitization() {
    let sanitized = b"$0_@0_%0_59311_tac-5d82f16579c34d0adc357b03_worker_0__0___120_40\n";
    for stage in [PaneMetadataStage::NewSession, PaneMetadataStage::Query] {
        let error = parse_pane(sanitized, stage).unwrap_err();
        assert_eq!(error.operation, "tmux metadata field count (expected 13)");
        let diagnostic = error.metadata.unwrap();
        assert_eq!(diagnostic.actual_fields, 1);
        assert_eq!(diagnostic.output_bytes, 65);
        let pane = parse_pane(
            b"$0\t@0\t%0\t123\tsession_with_underscores\tworker\t0\t\t0\t\t\t120\t40\n",
            stage,
        )
        .unwrap();
        assert_eq!(pane.address.session_name, "session_with_underscores");
    }
}

#[test]
fn pane_metadata_failures_identify_stage_reason_and_actual_field_count() {
    let valid = "$0\t@0\t%0\t123\tsession\tworker\t0\t\t0\t\t\t120\t40\n";
    for (stage, label) in [
        (PaneMetadataStage::NewSession, "new-session"),
        (PaneMetadataStage::Query, "query"),
    ] {
        for (input, reason, count) in [
            (String::new(), "tmux metadata empty output", 0),
            ("\n".into(), "tmux metadata empty output", 0),
            (
                "$0\t@0\n".into(),
                "tmux metadata field count (expected 13)",
                2,
            ),
            (
                "$0\\t@0\n".into(),
                "tmux metadata field count (expected 13)",
                2,
            ),
            (
                valid.replace("120\t40", "extra\t120\t40"),
                "tmux metadata field count (expected 13)",
                14,
            ),
            (
                valid.replace("$0", "/bin/bash"),
                "tmux metadata invalid session ID",
                13,
            ),
            (
                valid.replace("@0", "0"),
                "tmux metadata invalid window ID",
                13,
            ),
            (
                valid.replace("%0", "%"),
                "tmux metadata invalid pane ID",
                13,
            ),
        ] {
            let error = parse_pane(input.as_bytes(), stage).unwrap_err();
            assert_eq!(error.operation, reason);
            let metadata = error.metadata.as_ref().unwrap();
            assert_eq!(metadata.stage, stage);
            assert_eq!(metadata.actual_fields, count);
            assert_eq!(metadata.output_bytes, input.len());
            assert!(!metadata.truncated);
            let shown = error.to_string();
            assert!(shown.contains(reason));
            assert!(shown.contains(&format!("stage={label}; fields={count};")));
            assert!(shown.contains("sample=\""));
            assert!(shown.bytes().all(|byte| !byte.is_ascii_control()));
        }
    }
}

#[test]
fn pane_metadata_sample_escapes_raw_bytes_and_stops_at_256_bytes() {
    let mut bytes = b"\x1b[31m\t\n\r\"\\\0\xff".to_vec();
    bytes.resize(256, 0xff);
    bytes.extend_from_slice(b"outside-sample-limit");
    let error = parse_pane(&bytes, PaneMetadataStage::NewSession).unwrap_err();
    assert_eq!(error.operation, "tmux metadata encoding");
    let metadata = error.metadata.as_ref().unwrap();
    assert!(metadata.truncated);
    assert_eq!(metadata.output_bytes, bytes.len());
    assert!(metadata.sample.len() <= 1024);
    for escaped in [
        r"\x1b", r"\t", r"\n", r"\r", r#"\""#, r"\\", r"\x00", r"\xff",
    ] {
        assert!(metadata.sample.contains(escaped));
    }
    assert!(!error.to_string().contains("outside-sample-limit"));
    assert!(error
        .to_string()
        .bytes()
        .all(|byte| (b' '..=b'~').contains(&byte)));
}

#[test]
fn linux_stat_birth_parsing_does_not_split_the_comm_field() {
    let mut fields = vec!["S".to_string(), "55".to_string()];
    fields.extend((0..17).map(|_| "0".to_string()));
    fields.push("99999".to_string());
    let stat = format!("123 (a command (with parens)) {}", fields.join(" "));
    let (parent, birth, exited) = parse_linux_stat(stat.as_bytes()).unwrap();
    assert_eq!(parent, 55);
    assert_eq!(birth, "99999");
    assert!(!exited);
}
