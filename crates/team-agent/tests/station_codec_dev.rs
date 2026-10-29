//! Developer codec checks, separate from the unchanged independent red suite.
//! Corpus identities are synthetic, read-only examples, not native identity evidence.
use serde_json::{json, Value};
use std::{fs, io::Write, path::PathBuf};
use team_agent::{inbox_store as inbox, station::codec};

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "station-codec-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn segment(&self) -> PathBuf {
        self.0.join(".team/inbox/alpha/leader/00000001.jsonl")
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn message(text: &str) -> Value {
    inbox::record("worker", "leader", text, Some("alpha"), "message")
}

#[test]
fn u1_future_no_effect_reason_roundtrips() {
    let reason: codec::NoEffectReason =
        serde_json::from_str("\"future_composer_surface\"").unwrap();
    assert_eq!(
        reason,
        codec::NoEffectReason::Unknown("future_composer_surface".into())
    );
    assert_eq!(
        serde_json::to_string(&reason).unwrap(),
        "\"future_composer_surface\""
    );
}
#[test]
fn u2_future_outcome_is_uncertain_not_retry_permission() {
    let raw = json!({"state":"future_native_handoff","new_proof":{"opaque":true}});
    assert_eq!(codec::outcome(&raw), codec::ObservedOutcome::Uncertain(raw));
}
#[test]
fn u3_unknown_requirement_is_readable_but_not_mutable() {
    let raw = br#"{"v":1,"requires":["future.capability"],"revision":1}"#;
    let value = codec::read(raw).unwrap();
    assert!(codec::authorize_mutation(&value, &[])
        .unwrap_err()
        .contains("unsupported_capability"));
    assert!(codec::patch_scalar(
        std::str::from_utf8(raw).unwrap(),
        &["revision"],
        &json!(2),
        &[]
    )
    .is_err());
}
#[test]
fn u4_unknown_nested_bytes_are_untouched_by_known_leaf_patch() {
    let original = "{\n\"v\":1, \"requires\":[], \"runtime\":{\"revision\":1,\"futu\\u0072e\": {\"x\":1.000, \"s\":\"\\u0041\"}}\n}";
    let changed = codec::patch_scalar(original, &["runtime", "revision"], &json!(2), &[]).unwrap();
    assert_eq!(
        changed,
        original.replacen("\"revision\":1", "\"revision\":2", 1)
    );
}
#[test]
fn u5_duplicate_json_keys_are_rejected_at_every_depth() {
    for raw in [
        r#"{"v":1,"v":2}"#,
        r#"{"v":1,"future":{"x":1,"x":2}}"#,
        r#"[{"a":1,"\u0061":2}]"#,
    ] {
        assert!(codec::read(raw.as_bytes())
            .unwrap_err()
            .contains("duplicate_key"));
    }
}
#[test]
fn u6_checksum_matches_frozen_python_corpus_raw_token() {
    let line = include_bytes!("fixtures/station-v1/C3-record.jsonl");
    let mut frame: codec::Frame = serde_json::from_slice(line).unwrap();
    assert_eq!(
        codec::frame_checksum(
            frame.seq,
            &frame.kind,
            &frame.requires,
            frame.data.get().as_bytes()
        )
        .unwrap(),
        frame.checksum
    );
    assert!(codec::decode(line).is_ok());
    frame.checksum = "0".repeat(64);
    let mut damaged = serde_json::to_vec(&frame).unwrap();
    damaged.push(b'\n');
    assert!(codec::decode(&damaged).is_err());
}
#[test]
fn u7_sequence_gap_is_reported() {
    let workspace = Workspace::new();
    inbox::store(&workspace.0, "leader", &message("one")).unwrap();
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(workspace.segment())
        .unwrap();
    file.write_all(&codec::encode(&message("three"), 3).unwrap())
        .unwrap();
    file.sync_data().unwrap();
    assert!(inbox::read(&workspace.0, "leader", 10, Some("alpha"))
        .unwrap_err()
        .contains("seq_gap"));
}
#[test]
fn u8_torn_fragment_does_not_consume_sequence_or_get_rewritten() {
    let workspace = Workspace::new();
    inbox::store(&workspace.0, "leader", &message("one")).unwrap();
    let before = fs::read(workspace.segment()).unwrap();
    let fragment = b"{\"v\":1,\"seq\":2";
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(workspace.segment())
        .unwrap();
    file.write_all(fragment).unwrap();
    file.sync_data().unwrap();
    let second = inbox::store(&workspace.0, "leader", &message("two")).unwrap();
    assert_eq!(second.reference.seq, 2);
    let bytes = fs::read(workspace.segment()).unwrap();
    assert!(bytes.starts_with(&[before.as_slice(), fragment, b"\n"].concat()));
    assert_eq!(
        inbox::read(&workspace.0, "leader", 10, Some("alpha")).unwrap()["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
#[test]
fn u9_bounded_anonymous_coalescing_and_explicit_cross_window_reference() {
    let workspace = Workspace::new();
    let mut request = message("repeated");
    request["request_id"] = json!("repeat-key");
    let first = inbox::store(&workspace.0, "leader", &request).unwrap();
    let retry = inbox::store(&workspace.0, "leader", &request).unwrap();
    assert!(retry.duplicate);
    assert_eq!(retry.reference, first.reference);
    // Seed valid committed frames efficiently; exercise the production window and
    // reference lookup below, without doing a thousand unrelated fsync calls.
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(workspace.segment())
        .unwrap();
    for seq in 2..=1026 {
        file.write_all(&codec::encode(&message(&format!("other-{seq}")), seq).unwrap())
            .unwrap();
    }
    file.sync_data().unwrap();
    let fresh = inbox::store(&workspace.0, "leader", &request).unwrap();
    assert!(!fresh.duplicate);
    assert_eq!(fresh.reference.seq, 1027);
    request["dedupe_key"] = json!(first.reference.token());
    let explicit = inbox::store(&workspace.0, "leader", &request).unwrap();
    assert!(explicit.duplicate);
    assert_eq!(explicit.reference, first.reference);
    let mut expired = first.reference;
    expired.segment = 999_999;
    request["dedupe_key"] = json!(expired.token());
    assert!(inbox::store(&workspace.0, "leader", &request)
        .err()
        .unwrap()
        .contains("idempotency_window_expired"));
    assert!(!workspace.0.join(".team/inbox/alpha/.ids").exists());
}
#[test]
fn u10_all_frozen_root_shapes_and_future_enum_remain_readable() {
    for bytes in [
        include_str!("fixtures/station-v1/C1-endpoint.json"),
        include_str!("fixtures/station-v1/C2-request.json"),
        include_str!("fixtures/station-v1/C2-receipt.json"),
        include_str!("fixtures/station-v1/C3-record.json"),
        include_str!("fixtures/station-v1/C3-commit.json"),
        include_str!("fixtures/station-v1/C3-cursor.json"),
        include_str!("fixtures/station-v1/C3-message-index.json"),
        include_str!("fixtures/station-v1/C4-seat.json"),
        include_str!("fixtures/station-v1/C4-operation.json"),
        include_str!("fixtures/station-v1/C5-job.json"),
        include_str!("fixtures/station-v1/C5-acceptance.json"),
        include_str!("fixtures/station-v1/C6-hook.json"),
    ] {
        assert_eq!(codec::read(bytes.as_bytes()).unwrap()["v"], 1);
    }
    let future = b"{\"v\":2,\"kind\":\"future_record\",\"requires\":[\"future.cap\"],\"seq\":42,\"data\":{\"state\":\"unknown\"}}\n";
    // A newer root remains inspectable without assuming its checksum or action semantics.
    let value = codec::decode(future).unwrap();
    assert_eq!(value["kind"], "opaque");
    assert_eq!(value["record"]["data"]["state"], "unknown");
}
