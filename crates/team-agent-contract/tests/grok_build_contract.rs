//! Controlled Grok 1.0.52 grammar/identity regressions, not native Team G1–G5.
#![cfg(unix)]
mod support;
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, path::Path, time::Duration};
use team_agent_contract::contract::{delivery::*, descriptor::*, hooks::*, plan::*, probe::*, session::*, types::*};
use team_agent_contract::host::{command::*, digest, process::*};
use team_agent_contract::providers::grok_build::{interaction::*, native::*, *};
const SID: &str = "1a021870-30bc-4a89-984c-f61fab7b5269";
const HEADER: &str = "Team Agent message from leader for task-1:";
const CATALOG: &str = "You are logged in with grok.com.\n\nDefault model: grok-4.7\n\nAvailable models:\n  * grok-4.7 (default)\n  - grok-4.7-build-fast\n  - grok-4.5\n  - grok-4.6\n";
fn adapter() -> GrokAdapter { GrokAdapter::new(McpStdio { executable: "/fake/candidate".into(), arguments: vec!["contract-mcp".into(), "a b/中文".into()], environment: BTreeMap::from([("OWNED_VALUE".into(), "quoted \"text\"\nnext".into())]) }).unwrap() }
fn scope(sequence: u64) -> EvidenceScope {
    EvidenceScope { identity: InstanceIdentity { scope: ScopeId::new("scope").unwrap(), seat: SeatId::new("worker").unwrap(), instance: InstanceId::new("instance").unwrap(), generation: Generation(1) },
        endpoint: "/owned/tmux.sock".into(), pane: "%0".into(), binding: "owned".into(), session: Some(NativeSessionId::new(SID).unwrap()),
        sequence, observed_at: Duration::from_millis(sequence * 100), valid_for: Duration::from_secs(2), source: "controlled-fixture".into() }
}
fn screen(history: &str, input: &str) -> String {
    format!(" /owned/cwd   1.3K / 256K │ [Dashboard]\n{history}\n ╭──────────────────────────────────────────────────────────────────────────╮\n │ ❯ {input} │\n ╰────────────────────────────── Grok 4.7 (xhigh) · always-approve ─╯\n\n Ctrl+x:shortcuts\n")
}
fn paste_preview(rows: &[&str]) -> String {
    let mut text = format!("╭{}╮\n", "─".repeat(86));
    for row in rows {
        assert!(row.chars().count() <= 84);
        text.push_str(&format!("│ {row:<84} │\n"));
    }
    let caption = " paste again or double-click to expand ";
    text.push_str(&format!("╰─{caption}{}╯\n", "─".repeat(85 - caption.chars().count())));
    text
}
fn frame() -> CaptureFrame {
    CaptureFrame { scope: scope(2), text: screen(&paste_preview(&[HEADER, "…", "[team-agent-token:message-1]"]), "[Pasted: 161 lines]"),
        cwd: Some("/owned/cwd".into()), input: Some(InputShape { sha256: digest(b"immutable input"), bytes: 10507, lines: 161, first_line: HEADER.into() }),
        baseline: Some(CaptureBaseline { scope: scope(1), text: screen("", "") }), profile_id: PROFILE.into(), operation: Operation::FirstBusiness,
        message: Some(MessageId::new("message-1").unwrap()), attempt: Some(AttemptId::new("attempt-1").unwrap()), after_step: Some(StepKind::Paste), paste_latch: PasteLatch::NeverSeen }
}
fn modal(rows: &[&str]) -> String {
    // R6 captured native modal: 76 inner cells, square corners, close button.
    let mut text = format!("┌{} [✗] ─┐\n", "─".repeat(70));
    for row in rows {
        assert!(row.chars().count() <= 74);
        text.push_str(&format!("│ {row:<74} │\n"));
    }
    text.push_str(&format!("└{}┘\n", "─".repeat(76)));
    text
}
fn session(cwd: &str, id: &str) -> String {
    modal(&["Context usage  Usage limit  Session info", "Session info   click or drag to copy", "",
        "Shell version:", "1.0.52", "Session ID:", id, "Working directory:", cwd,
        "Model: Grok 4.7", "Tab switch  |  ↑/↓ scroll  |  c copy session ID  |  y copy all", "Esc close"])
}
fn over_history(panel: &str) -> String {
    // Background character/byte offsets deliberately differ. They are not the
    // modal's width, nor session evidence. Include distracting border glyphs.
    panel.lines().enumerate().map(|(i, row)| {
        let left = if i % 2 == 0 { "旧的中文对话🦀│ " } else { "prior turn: │ " };
        format!("{left}{row} resumed history │ 第三轮\n")
    }).collect()
}
fn session_evidence(text: String) -> ScopedSessionEvidence {
    ScopedSessionEvidence { scope: scope(1), provider: ProviderId::new("grok_build").unwrap(),
        native: NativeIdentity { version: "1.0.52 (0920c8169e27)".into(), harness: "grok-build".into(), ui: "tui".into(), platform: Platform::MacOs, executable_sha256: digest(b"native") },
        cwd: CwdIdentity { path: "/owned/cwd".into(), identity: digest(b"cwd") }, origin: CaptureOrigin::CurrentNativeSession,
        evidence_kind: EvidenceKind::Fixture, evidence_sha256: digest(text.as_bytes()), record: text.into_bytes() }
}
#[test]
fn all_facets_and_hooks_disclose_owned_server_coexistence_without_legacy_migration() {
    let a = adapter(); let hooks = a.hooks();
    assert_eq!(GROK_BUILD_DESCRIPTOR.identity.id, "grok_build");
    assert_eq!(GROK_BUILD_DESCRIPTOR.identity.aliases, &["grok"]);
    for operation in [Operation::Catalog, Operation::Fresh, Operation::Resume, Operation::FirstBusiness, Operation::Stop] {
        validate_descriptor(&GROK_BUILD_DESCRIPTOR, &hooks, operation).unwrap();
    }
    assert_eq!(GROK_BUILD_DESCRIPTOR.mcp.isolation, McpIsolation::OwnedServerIsolated);
    assert_eq!(GROK_BUILD_DESCRIPTOR.tool_names.naming, Support::Supported(ToolNaming::RuntimeBound));
    hooks.catalog.require("H1").unwrap(); hooks.plan.require("H2").unwrap(); hooks.materialize.require("H3").unwrap();
    hooks.session.require("H4").unwrap(); hooks.semantic.require("H5").unwrap(); hooks.interaction.require("H6").unwrap(); hooks.fork.require("H7").unwrap();
    assert_eq!(GROK_BUILD_DESCRIPTOR.session.fresh, FreshSession::CaptureAfterLaunch);
    assert_eq!(GROK_BUILD_DESCRIPTOR.session.resume, Support::Supported(ResumeMode::ExactId));
    assert!(matches!(GROK_BUILD_DESCRIPTOR.effort.admission[4], EffortAdmission::Reject(_)));
    assert!(matches!(GROK_BUILD_DESCRIPTOR.effort.admission[5], EffortAdmission::Reject(_)));
}
#[test]
fn evergreen_version_keeps_actual_revision_and_bounded_bad_input() {
    for version in ["1.0.46 (2765805b9442)", "1.0.52 (0920c8169e27)", "2.3.101 (abcdef)"] {
        assert_eq!(parse_version(format!("grok {version}\n").as_bytes()).unwrap(), version);
    }
    for bad in ["1.0.52", "grok 1.0.52", "grok 1.0.52 (not-hex)", "grok 1.0.52 (aa)\nextra", "grok 01.0.52 (aa)"] { assert!(parse_version(bad.as_bytes()).is_err()); }
    let error = parse_version(&[b'x'; 900]).unwrap_err().to_string();
    assert!(error.contains("truncated=true")); assert!(error.len() < 450);
}
#[test]
fn exact_catalog_never_falls_back_or_invents_luna() {
    let records = parse_catalog(CATALOG.as_bytes()).unwrap();
    assert_eq!(records.iter().map(|record| record.id.as_str()).collect::<Vec<_>>(), ["grok-4.7", "grok-4.7-build-fast", "grok-4.5", "grok-4.6"]);
    for bad in [CATALOG.replace("logged in", "not logged in"), CATALOG.replace("  - grok-4.6", "  - grok-4.5"), CATALOG.replace("Default model: grok-4.7", "Default model: absent"), CATALOG.replace(" (default)", ""), format!("{CATALOG}auth error")] { assert!(parse_catalog(bad.as_bytes()).is_err()); }
}
#[test]
fn owned_toml_has_exact_stdio_env_and_per_generation_unique_key() {
    let identity = scope(1).identity;
    let resource = adapter().mcp_resource(identity.clone(), "/owned/cwd".into()).unwrap();
    assert_eq!(resource.path.relative(), Path::new(".grok/config.toml"));
    let text = String::from_utf8(resource.contents).unwrap();
    let key = server_key(&identity);
    assert!(text.contains(&format!("[mcp_servers.{key}]")));
    assert!(text.contains(&format!("[mcp_servers.{key}.env]")));
    assert!(text.contains("command = \"/fake/candidate\""));
    assert!(text.contains("enabled = true")); assert!(text.contains("a b/中文"));
    assert!(text.contains("\\\"text\\\"\\nnext"));
    let mut other = identity.clone(); other.generation = Generation(2); assert_ne!(key, server_key(&other));
    other = identity; other.seat = SeatId::new("peer").unwrap(); assert_ne!(key, server_key(&other));
}
#[test]
fn folded_seen_is_bound_to_confirmed_paste_payload_and_empty_composer() {
    let f = frame(); let observed = adapter().interpret(&f);
    assert_eq!(observed.surface, InputSurface::ComposerContainsPaste);
    assert!(matches!(observed.paste_latch, PasteLatch::Seen { .. }));
    assert_eq!(observed.current_attempt, f.attempt);
    for kind in 0..7 {
        let mut f = frame();
        match kind {
            0 => f.after_step = None, 1 => f.input = None,
            2 => f.input.as_mut().unwrap().lines = 160,
            3 => f.baseline.as_mut().unwrap().text = screen("", "unsent text"),
            4 => f.baseline.as_mut().unwrap().scope.identity.generation = Generation(9),
            5 => f.text = screen(&paste_preview(&["unrelated preview", "[team-agent-token:message-1]"]), "[Pasted: 161 lines]"),
            _ => f.attempt = None,
        }
        assert!(matches!(adapter().interpret(&f).paste_latch, PasteLatch::NeverSeen), "case {kind}");
    }
}
#[test]
fn long_paste_kb_label_reaches_seen_with_owned_proof() {
    let mut f = frame();
    f.input.as_mut().unwrap().lines = 190;
    f.text = screen(&paste_preview(&[HEADER, "…", "[team-agent-token:message-1]"]), "[Pasted: 10 KB]");
    let observed = adapter().interpret(&f);
    assert!(matches!(observed.paste_latch, PasteLatch::Seen { .. }));
    assert_eq!(observed.current_message, f.message);
    assert_eq!(observed.current_attempt, f.attempt);
}
#[test]
fn long_paste_kb_forms_keep_ownership_and_size_guards() {
    for (label, bytes) in [("10 KB", 10507), ("10.3 KB", 10507), ("12.5 KB", 12800)] {
        let mut f = frame();
        f.input.as_mut().unwrap().bytes = bytes;
        let preview = paste_preview(&[HEADER, "…", "[team-agent-token:message-1]"]);
        f.text = screen(&preview, &format!("[Pasted: {label}]"));
        assert!(matches!(adapter().interpret(&f).paste_latch, PasteLatch::Seen { .. }), "{label}");
        for case in 0..8 {
            let mut bad = f.clone();
            match case {
                0 => bad.input.as_mut().unwrap().bytes = 1024,
                1 => bad.input.as_mut().unwrap().bytes = 0,
                2 => bad.baseline.as_mut().unwrap().text = screen("", "foreign draft"),
                3 => bad.text = bad.text.replace("[team-agent-token:message-1]", "[team-agent-token:other]"),
                4 => bad.text = bad.text.replace(HEADER, "an unrelated preview row"),
                5 => bad.after_step = None,
                6 => bad.baseline.as_mut().unwrap().text = screen(&format!("[Pasted: {label}]"), ""),
                _ => bad.scope.identity.generation = Generation(2),
            }
            assert_eq!(adapter().interpret(&bad).paste_latch, PasteLatch::NeverSeen, "{label} case {case}");
        }
    }
    for label in ["0 KB", "01 KB", "+10 KB", "-10 KB", "1e1 KB", "10. KB", "10..3 KB", "10.0000 KB", "10 kb", "10 MB", "10 KB] suffix", "9 KB", "11 KB"] {
        let mut f = frame();
        f.text = screen(&paste_preview(&[HEADER, "…", "[team-agent-token:message-1]"]), &format!("[Pasted: {label}]"));
        assert_eq!(adapter().interpret(&f).paste_latch, PasteLatch::NeverSeen, "{label}");
    }
}
#[test]
fn long_paste_cjk_preview_uses_display_cells() {
    let mut f = frame();
    // Captured 1.0.67 preview: an 86-cell inner width, not 86 scalars.
    // One leading space + ten Han characters (20 cells) + 65 spaces.
    let wide_row = format!("│ {}{}│", "中文".repeat(5), " ".repeat(65));
    let preview = paste_preview(&[HEADER, "…", "[team-agent-token:message-1]"])
        .replace(&format!("│ {:<84} │", "…"), &wide_row);
    f.text = screen(&preview, "[Pasted: 161 lines]");
    let observed = adapter().interpret(&f);
    assert!(matches!(observed.paste_latch, PasteLatch::Seen { .. }));
    assert_eq!(observed.current_attempt, f.attempt);
    let mut bad = f;
    bad.text = bad.text.replace(&wide_row, &wide_row.replace('│', " "));
    assert_eq!(adapter().interpret(&bad).paste_latch, PasteLatch::NeverSeen);
}
#[test]
fn long_paste_combining_and_emoji_preview_keep_closed_edges() {
    for (text, padding) in [("e\u{301}", 84), ("👩‍💻", 83)] {
        let mut f = frame();
        let row = format!("│ {text}{}│", " ".repeat(padding));
        let preview = paste_preview(&[HEADER, "…", "[team-agent-token:message-1]"])
            .replace(&format!("│ {:<84} │", "…"), &row);
        f.text = screen(&preview, "[Pasted: 161 lines]");
        assert!(matches!(adapter().interpret(&f).paste_latch, PasteLatch::Seen { .. }));
        f.text = f.text.replace(&row, &format!("│ {text}{}│", " ".repeat(padding - 1)));
        assert_eq!(adapter().interpret(&f).paste_latch, PasteLatch::NeverSeen);
    }
}
#[test]
fn folded_enter_requires_new_matching_transcript_not_disappearance_or_busy_alone() {
    let mut f = frame(); f.paste_latch = adapter().interpret(&f).paste_latch;
    f.scope = scope(3); f.after_step = Some(StepKind::InitialSubmit);
    f.text = screen(&format!("    {HEADER}          12:10 PM\n    line-000 …\n\n  ⠼ Waiting for response… 0.2s       0.2s ⇣1.30k [stop]"), "");
    let accepted = adapter().interpret(&f);
    assert_eq!(accepted.surface, InputSurface::NativeAccepted);
    assert!(matches!(accepted.paste_latch, PasteLatch::Gone { .. }));
    for kind in 0..6 {
        let mut bad = f.clone();
        match kind {
            0 => bad.text = screen("⠼ Waiting for response… 0.2s", ""),
            1 => bad.after_step = Some(StepKind::Paste),
            2 => bad.attempt = Some(AttemptId::new("other").unwrap()),
            3 => bad.text = screen(HEADER, "remaining input"),
            4 => bad.baseline.as_mut().unwrap().text = screen(HEADER, ""),
            _ => bad.input.as_mut().unwrap().sha256 = digest(b"different payload"),
        }
        assert_ne!(adapter().interpret(&bad).surface, InputSurface::NativeAccepted, "case {kind}");
    }
}
#[test]
fn folded_preview_above_restored_history_and_scrollbar_proves_current_header_and_token() {
    let mut f = frame(); f.input.as_mut().unwrap().lines = 140;
    f.baseline.as_mut().unwrap().text = screen("previous generation transcript", "");
    let preview = paste_preview(&[HEADER, "…", "[team-agent-token:message-1]"]);
    for history in [over_history(&preview), preview.lines().map(|row| format!("{row}               █\n")).collect()] {
        f.text = screen(&history, "[Pasted: 140 lines]");
        let seen = adapter().interpret(&f);
        assert!(matches!(&seen.paste_latch, PasteLatch::Seen { native_identity } if native_identity.starts_with("grok-owned-folded-v3:")));
        assert_eq!(seen.current_attempt, f.attempt);
        assert_eq!(seen.current_message, f.message);
        assert_eq!(seen.surface, InputSurface::ComposerContainsPaste);
    }
}
#[test]
fn folded_preview_rejects_foreign_hidden_duplicate_tokens_and_unclosed_or_history_only_proofs() {
    let valid = paste_preview(&[HEADER, "…", "[team-agent-token:message-1]"]);
    for kind in 0..11 {
        let mut f = frame();
        let history = match kind {
            0 => paste_preview(&[HEADER, "[team-agent-token:other]"]),
            1 => paste_preview(&[HEADER, "…"]),
            2 => paste_preview(&[HEADER, "[team-agent-token:message-1]", "[team-agent-token:message-1]"]),
            3 => format!("{HEADER}\n{}", paste_preview(&["unrelated preview", "[team-agent-token:message-1]"])),
            4 => format!("[team-agent-token:message-1]\n{}", paste_preview(&[HEADER, "…"])),
            5 => valid.replace("paste again or double-click to expand", "not a native preview footer"),
            6 => valid.lines().filter(|row| !row.starts_with('╰')).collect::<Vec<_>>().join("\n"),
            7 => valid.replace("│ Team", "│Team"),
            8 => format!("{valid}{valid}"),
            9 => { f.baseline.as_mut().unwrap().text = screen("[Pasted: 161 lines]", ""); valid.clone() },
            _ => { f.baseline.as_mut().unwrap().text = screen("[team-agent-token:message-1]", ""); valid.clone() },
        };
        f.text = screen(&history, "[Pasted: 161 lines]");
        assert_eq!(adapter().interpret(&f).paste_latch, PasteLatch::NeverSeen, "case {kind}");
    }
}
#[test]
fn submitted_transcript_scrollbar_never_requires_cropping_or_accepts_preview_as_history() {
    let mut f = frame();
    let header = "Team Agent message from leader for msg_a942d285204881a258b2e48dba84bec4_7:";
    f.input.as_mut().unwrap().first_line = header.into();
    let preview = paste_preview(&[header, "…", "[team-agent-token:message-1]"]);
    f.text = screen(&format!("{}               █", preview.trim_end()), "[Pasted: 161 lines]");
    f.paste_latch = adapter().interpret(&f).paste_latch;
    assert!(matches!(f.paste_latch, PasteLatch::Seen { .. }));
    f.scope = scope(3); f.after_step = Some(StepKind::InitialSubmit);
    for row in [format!("    {header}             4:14 PM █"), format!("    {header}█"), format!("    {}…          █", &header[..40])] {
        f.text = screen(&row, "");
        assert_eq!(adapter().interpret(&f).surface, InputSurface::NativeAccepted);
        assert!(matches!(adapter().interpret(&f).paste_latch, PasteLatch::Gone { .. }));
    }
    f.text = screen(&preview, "");
    assert_ne!(adapter().interpret(&f).surface, InputSurface::NativeAccepted);
    f.text = screen(&format!("    {header}            █"), "");
    f.baseline.as_mut().unwrap().text = f.text.clone();
    assert_ne!(adapter().interpret(&f).surface, InputSurface::NativeAccepted);
    f.baseline.as_mut().unwrap().text = screen("", "");
    if let PasteLatch::Seen { native_identity } = &mut f.paste_latch {
        *native_identity = native_identity.replace("grok-owned-folded-v3:", "grok-owned-folded-v1:");
    }
    assert_ne!(adapter().interpret(&f).surface, InputSurface::NativeAccepted);
}
#[test]
fn idle_single_enter_has_no_unconditional_confirmation_and_trust_has_separate_key() {
    assert_eq!(POLICY, digest(POLICY_RECIPE.as_bytes()));
    let business = PROFILES[0].policy.require("business").unwrap(); business.validate().unwrap();
    assert_eq!(business.initial_submit, PhysicalKey::Enter); assert!(business.confirmation_steps.is_empty());
    assert_eq!(business.retry_budget, RetryBudget::Never); assert_eq!(business.queue_flush.len(), 1);
    assert_eq!(business.queue_flush[0].max_presses, 1); assert_eq!(business.max_submit_keys, 2);
    let trust = PROFILES[1].policy.require("startup").unwrap(); trust.validate().unwrap();
    assert!(trust.queue_flush.is_empty()); assert_eq!(STARTUP.key, PhysicalKey::ConfirmYes);
    assert!(CONTROLS.iter().any(|control| control.command == "/exit" && control.operation == Operation::Stop));
    assert!(CONTROLS.iter().all(|control| control.command != "/tools"));
}
#[test]
fn queue_flush_requires_the_exact_current_visible_token_and_footer() {
    let mut f = frame(); f.input = None; f.scope = scope(3); f.after_step = Some(StepKind::InitialSubmit);
    let token = "[team-agent-token:message-1]";
    f.paste_latch = PasteLatch::Seen { native_identity: token.into() };
    f.text = screen(&format!("#1 {token}\nQueued · Enter to send now"), "");
    let queued = adapter().interpret(&f);
    assert_eq!(queued.surface, InputSurface::Queued);
    assert_eq!(queued.predicate.as_deref(), Some(PROFILES[0].policy.require("queue").unwrap().queue_flush[0].predicate));
    let mut legacy = f.clone();
    legacy.text = screen(&format!("#1 {token}\nEnter:send now"), "");
    assert_eq!(adapter().interpret(&legacy).predicate.as_deref(), Some(PROFILES[0].policy.require("queue").unwrap().queue_flush[0].predicate));
    for bad in ["1 queued · Enter: send now", "Queued · Enter to send now old", "enter send now"] {
        let mut wrong = f.clone(); wrong.text = screen(&format!("#1 {token}\n{bad}"), "");
        assert!(adapter().interpret(&wrong).predicate.is_none(), "{bad}");
    }
    let mut other = f.clone(); other.text = screen("#1 another message\nQueued · Enter to send now", "");
    assert!(adapter().interpret(&other).predicate.is_none());
    f.after_step = Some(StepKind::QueueFlush); f.text = screen(token, "");
    assert_eq!(adapter().interpret(&f).surface, InputSurface::NativeAccepted);
}
#[test]
fn swallowed_folded_capsule_requests_one_more_enter_and_busy_empty_composer_does_not() {
    let mut f = frame();
    f.paste_latch = adapter().interpret(&f).paste_latch;
    f.scope = scope(3);
    f.after_step = Some(StepKind::InitialSubmit);
    let held = adapter().interpret(&f);
    assert_eq!(held.surface, InputSurface::Queued);
    assert_eq!(held.predicate.as_deref(), Some(PROFILES[0].policy.require("queue").unwrap().queue_flush[0].predicate));
    f.text = screen("⠼ Waiting for response… 0.2s", "");
    let busy = adapter().interpret(&f);
    assert_ne!(busy.surface, InputSurface::Queued);
    assert!(busy.predicate.is_none());
}
#[test]
fn visible_multiline_token_uses_existing_correlation_not_folded_fallback() {
    let mut f = frame(); f.input = None;
    f.text = "╭────────────────────────╮\n│ ❯ message first line │\n│ [team-agent-token:message-1] │\n╰──────── Grok 4.7 (xhigh) ─╯\n".into();
    let observed = adapter().interpret(&f);
    assert_eq!(observed.paste_latch, PasteLatch::Seen { native_identity: "[team-agent-token:message-1]".into() });
}
#[test]
fn trust_requires_exact_owned_cwd_and_never_uses_history_mentions() {
    let mut f = frame(); f.profile_id = STARTUP_PROFILE.into(); f.operation = Operation::StartupBypassAck;
    f.message = None;
    f.text = "Do you trust the contents of this directory?\n/owned/cwd\nGrok Build may run or modify contents in this directory,\nposing security risks.\nYes, proceed       y\nNo, quit        n\n".into();
    assert_eq!(adapter().interpret(&f).surface, InputSurface::StartupRiskWarning);
    f.cwd = Some("/other/cwd".into()); assert_ne!(adapter().interpret(&f).surface, InputSurface::StartupRiskWarning);
    f.cwd = Some("/owned/cwd".into()); f.text = screen(&f.text, ""); assert_ne!(adapter().interpret(&f).surface, InputSurface::StartupRiskWarning);
}
#[test]
fn current_session_is_native_modal_uuid_with_matching_cwd_not_launch_origin() {
    let text = session("/owned/cwd", SID);
    assert_eq!(session_id(&text, Path::new("/owned/cwd")).unwrap().as_str(), SID);
    let duplicate = text.replace("└", &format!("│ {:<74} │\n│ {SID:<74} │\n└", "Session ID:"));
    for bad in [text.replace("Esc close", "         "), session("/other", SID), session("/owned/cwd", "latest"), format!("Here is your Session ID: {SID}"), duplicate] { assert!(session_id(&bad, Path::new("/owned/cwd")).is_err()); }
    let mut evidence = session_evidence(text);
    assert_eq!(adapter().bind(&evidence).unwrap().native_session.as_str(), SID);
    evidence.origin = CaptureOrigin::ExplicitResumeLaunch; assert!(adapter().bind(&evidence).is_err());
    evidence.origin = CaptureOrigin::CurrentNativeSession; evidence.native.version = "1.0.53 (aaaa)".into(); assert!(adapter().bind(&evidence).is_err());
}
#[test]
fn restored_history_modal_binds_and_accepts_only_after_owned_control_submit() {
    let mut f = frame();
    f.operation = Operation::SessionInspect; f.message = None; f.input = None;
    f.baseline.as_mut().unwrap().text = screen("prior turn mentions /session-info", "");
    f.text = screen("prior turn mentions /session-info", "/session-info");
    let seen = adapter().interpret(&f);
    assert_eq!(seen.paste_latch, PasteLatch::Seen { native_identity: "/session-info".into() });
    f.paste_latch = seen.paste_latch; f.scope = scope(3); f.after_step = Some(StepKind::InitialSubmit);
    f.text = screen(&over_history(&session("/owned/cwd", SID)), "");
    let accepted = adapter().interpret(&f);
    assert_eq!(accepted.surface, InputSurface::NativeAccepted);
    assert_eq!(accepted.paste_latch, PasteLatch::Gone { native_identity: "/session-info".into() });
    assert_eq!(accepted.current_attempt, f.attempt);
    assert_eq!(accepted.predicate.as_deref(), Some(SESSION_PANEL.predicate));
    assert_eq!(adapter().bind(&session_evidence(f.text.clone())).unwrap().native_session.as_str(), SID);
    for kind in 0..4 {
        let mut bad = f.clone();
        match kind {
            0 => bad.paste_latch = PasteLatch::NeverSeen,
            1 => bad.paste_latch = PasteLatch::Seen { native_identity: "/exit".into() },
            2 => bad.after_step = Some(StepKind::Paste),
            _ => bad.cwd = Some("/other/cwd".into()),
        }
        assert_ne!(adapter().interpret(&bad).surface, InputSurface::NativeAccepted);
    }
    // The existing journaled Esc-close operator asks H6 without a business latch.
    f.after_step = None; f.attempt = None; f.baseline = None; f.paste_latch = PasteLatch::NeverSeen;
    assert_eq!(adapter().interpret(&f).surface, InputSurface::Starting);
    assert_eq!(adapter().interpret(&f).predicate.as_deref(), Some(SESSION_PANEL.predicate));
    f.text = screen("restored history after Esc", "");
    assert_eq!(adapter().interpret(&f).surface, InputSurface::ComposerReady);
    assert_eq!(adapter().interpret(&f).predicate, None);
}
#[test]
fn session_modal_rejects_open_wrong_width_duplicate_and_ambiguous_rectangles() {
    let text = session("/owned/cwd", SID);
    let malformed_row = text.replace("│ Shell version:", "  Shell version:");
    let short_row = text.replace("│ Shell version:", "│Shell version:");
    let ambiguous_row = text.lines().map(|row| {
        if row.starts_with('│') { format!("{row} {row}\n") } else { format!("{row}\n") }
    }).collect::<String>();
    let missing_top = text.lines().skip(1).collect::<Vec<_>>().join("\n");
    let missing_bottom = text.lines().filter(|line| !line.starts_with('└')).collect::<Vec<_>>().join("\n");
    let wrong_bottom = text.replace(&format!("└{}┘", "─".repeat(76)), &format!("└{}┘", "─".repeat(75)));
    for bad in [malformed_row, short_row, ambiguous_row, missing_top, missing_bottom, wrong_bottom, format!("{text}{text}")] {
        assert!(session_id(&bad, Path::new("/owned/cwd")).is_err());
    }
}
#[test]
fn session_binding_uses_only_modal_fields_not_matching_background_metadata() {
    let background = "│ Shell version: │\n│ 1.0.52 │\n│ Session ID: │\n│ 11111111-1111-1111-1111-111111111111 │\n│ Working directory: │\n│ /owned/cwd │\n│ Esc close │";
    let wrap = |panel: String| format!("{background}\n{}{background}", over_history(&panel));
    let text = wrap(session("/owned/cwd", SID));
    assert_eq!(adapter().bind(&session_evidence(text)).unwrap().native_session.as_str(), SID);
    for bad in [session("/wrong/cwd", SID), session("/owned/cwd", "bad-id"),
        session("/owned/cwd", SID).replace("Esc close", "         "),
        session("/owned/cwd", SID).replace("1.0.52", "1.0.53")]
    {
        assert!(adapter().bind(&session_evidence(wrap(bad))).is_err());
    }
    let duplicate_version = session("/owned/cwd", SID).replace("└", &format!("│ {:<74} │\n│ {:<74} │\n└", "Shell version:", "1.0.52"));
    assert!(adapter().bind(&session_evidence(wrap(duplicate_version))).is_err());
}
struct Runner;
impl CommandRunner for Runner {
    fn run(&mut self, request: &CommandRequest) -> CommandReceipt {
        // Probes run the PATH entry itself and leave the user's environment alone.
        assert_eq!(request.arguments, [std::ffi::OsString::from("--version")]);
        assert_eq!(request.executable.file_name().unwrap(), "grok");
        assert!(request.environment.set.is_empty() && request.environment.remove.is_empty());
        CommandReceipt { end: CommandEnd::Exited, exit_code: Some(0), child_pid: Some(42), child_reaped: true,
            stdout: b"grok 1.0.52 (0920c8169e27)\n".to_vec(), stderr: vec![], elapsed: Duration::from_millis(1) }
    }
}
fn write_executable(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
/// vendor/bin/grok -> vendor/lib/grok-native, the layout an npm/Homebrew
/// install leaves on PATH. `image` is the native file the symlink ends in.
fn vendor_install(root: &Path, image: Option<&Path>) -> (std::path::PathBuf, std::path::PathBuf) {
    let (bin, native) = (root.join("vendor/bin"), root.join("vendor/lib/grok-native"));
    std::fs::create_dir_all(&bin).unwrap();
    match image {
        Some(image) => { std::fs::create_dir_all(native.parent().unwrap()).unwrap(); std::os::unix::fs::symlink(image, &native).unwrap(); }
        None => write_executable(&native, b"controlled final native"),
    }
    std::os::unix::fs::symlink(&native, bin.join("grok")).unwrap();
    (bin, native.canonicalize().unwrap())
}
fn grok_launch(sandbox: &support::Sandbox, command: &PublicCommand) -> (GrokAdapter, ResolvedLaunch, LaunchPlan, RuntimeInstallation) {
    let bounds = ReadBounds { deadline: Duration::from_secs(10), max_output_bytes: 65536 };
    let metadata = probe_launcher(command, &mut Runner, bounds).unwrap();
    let mut runtime = probe_runtime(command, &sandbox.parent, &mut Runner, bounds).unwrap(); runtime.native.platform = Platform::MacOs;
    let catalog = CatalogObservation { provider: ProviderId::new("grok_build").unwrap(), native: metadata, schema: CATALOG_SOURCE.schema.into(), models: parse_catalog(CATALOG.as_bytes()).unwrap() };
    let catalog = runtime.bind_catalog(&catalog).unwrap();
    let project = sandbox.parent.join("actual-project");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    let a = adapter().with_runtime(runtime.clone()).with_project(project.clone()).unwrap();
    std::fs::create_dir_all(&sandbox.root).unwrap();
    let mut r = support::request(&sandbox.root, "worker"); r.provider = "grok_build".into(); r.native = runtime.native.clone();
    r.paths.cwd = resolve_cwd(&project).unwrap();
    r.paths.launcher = runtime.launcher(); r.paths.executable = runtime.engine.clone(); r.paths.candidate = "/fake/candidate".into(); r.model = Some("grok-4.7".into());
    r.prompt = Some("user\nproject\nrole\nTEAM".into()); r.input_profile = Some(PROFILE.into()); r.bypass = true;
    let resolved = resolve_launch(&GROK_BUILD_DESCRIPTOR, &a.hooks(), &r, Some(&catalog)).unwrap();
    let plan = a.plan(&resolved).unwrap(); validate_launch_plan(&GROK_BUILD_DESCRIPTOR, &resolved, &plan).unwrap();
    (a, resolved, plan, runtime)
}
#[test]
fn runtime_catalog_and_launch_identity_fence_exact_native_without_release_pins() {
    let sandbox = support::Sandbox::new();
    let (bin, image) = vendor_install(&sandbox.parent, None);
    let command = PublicCommand::resolve("grok", bin.as_os_str()).unwrap();
    let (a, resolved, plan, runtime) = grok_launch(&sandbox, &command);
    // The vendor symlink is the entry as found; the inner image is only the fence.
    assert_eq!(plan.launcher.command, "grok");
    assert_eq!(plan.cwd, sandbox.parent.join("actual-project"));
    assert!(plan.arguments.windows(2).any(|args| args[0] == "--cwd" && args[1] == plan.cwd.as_os_str()));
    assert!(plan.arguments.windows(2).any(|args| args[0] == "--leader-socket" && std::path::Path::new(&args[1]).starts_with(&sandbox.root)));
    assert_eq!(plan.materialization[0].path.root(), plan.cwd);
    assert_eq!(plan.materialization[1].path.root(), sandbox.root);
    assert_eq!(plan.launcher.executable, bin.join("grok"));
    assert_eq!(plan.executable, image);
    assert_eq!(runtime.native.executable_sha256, digest(b"controlled final native"));
    assert!(!plan.environment.set.contains_key("GROK_REAL") && !plan.environment.remove.contains("GROK_REAL"));
    assert!(matches!(plan.expected_session, ExpectedSession::CaptureAfterLaunch));
    assert!(!plan.arguments.iter().any(|arg| arg == "--session-id" || arg == "--continue" || arg == "--add-dir"));
    let r = resolved.request().clone();
    assert_eq!(plan.materialization[1].contents, r.prompt.as_ref().unwrap().as_bytes());
    let catalog = CatalogObservation { provider: ProviderId::new("grok_build").unwrap(), native: runtime.native.clone(), schema: CATALOG_SOURCE.schema.into(), models: parse_catalog(CATALOG.as_bytes()).unwrap() };
    for bypass in [false, true] {
        let mut request = r.clone(); request.bypass = bypass;
        let resolved = resolve_launch(&GROK_BUILD_DESCRIPTOR, &a.hooks(), &request, Some(&catalog)).unwrap();
        let plan = a.plan(&resolved).unwrap();
        assert_eq!(plan.arguments.iter().any(|arg| arg == "--always-approve"), bypass);
    }
    let mut descriptor = GROK_BUILD_DESCRIPTOR.clone(); descriptor.tool_names.naming = Support::Supported(ToolNaming::StaticServer { key: "team" });
    assert!(resolve_launch(&descriptor, &a.hooks(), &r, Some(&catalog)).is_err());
    descriptor = GROK_BUILD_DESCRIPTOR.clone(); descriptor.teardown.resources = &[];
    assert!(resolve_launch(&descriptor, &a.hooks(), &r, Some(&catalog)).is_err());
    let mut wrong = r.clone(); wrong.model = Some("luna-invented".into());
    assert!(resolve_launch(&GROK_BUILD_DESCRIPTOR, &a.hooks(), &wrong, Some(&catalog)).is_err());
    // Even an otherwise valid request may not redirect this project into a private cwd.
    let mut redirected = r.clone(); redirected.paths.cwd = resolve_cwd(&sandbox.root).unwrap();
    let redirected = resolve_launch(&GROK_BUILD_DESCRIPTOR, &a.hooks(), &redirected, Some(&catalog)).unwrap();
    assert!(a.plan(&redirected).is_err());
    // A launcher other than the probed PATH route can never be planned.
    let mut wrong = r.clone(); wrong.paths.launcher.sha256 = digest(b"another entry");
    let resolved = resolve_launch(&GROK_BUILD_DESCRIPTOR, &a.hooks(), &wrong, Some(&catalog)).unwrap();
    assert!(a.plan(&resolved).is_err());
}
#[test]
fn grok_launch_runs_the_user_wrapper_prepended_to_path_then_the_native_image() {
    let sandbox = support::Sandbox::new();
    // The "native image" echoes its argv, so exec-through is observable.
    let (vendor, image) = vendor_install(&sandbox.parent, Some(Path::new("/bin/echo")));
    let wrappers = sandbox.parent.join("user wrappers");
    let marker = sandbox.parent.join("wrapper-invocations");
    write_executable(&wrappers.join("grok"), format!(
        "#!/bin/sh\nprintf '%s|%s\\n' \"${{0##*/}}\" \"$1\" >> '{}'\nexec '{}' \"$@\"\n",
        marker.display(), vendor.join("grok").display()).as_bytes());
    let path = std::env::join_paths([wrappers.as_path(), vendor.as_path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
    let command = PublicCommand::resolve("grok", &path).unwrap();
    assert_eq!(command.entry, wrappers.join("grok"), "the user wrapper is the public entry");
    let (_, _, plan, runtime) = grok_launch(&sandbox, &command);
    assert_eq!(plan.launcher.executable, wrappers.join("grok"));
    assert_eq!(plan.executable, image);
    assert_eq!(runtime.engine, Path::new("/bin/echo").canonicalize().unwrap());
    let script = team_agent_contract::host::shell::launch_script(&plan, &Default::default()).unwrap();
    let text = String::from_utf8(script.clone()).unwrap();
    assert!(text.contains("\nexec 'grok' '--model' 'grok-4.7'"), "{text}");
    for forbidden in ["grok-native", "GROK_REAL", "/usr/local/bin/grok", "/opt/homebrew", "team-agent-grok-name"] {
        assert!(!text.contains(forbidden), "{forbidden} in {text}");
    }
    assert!(!text.contains(&format!("exec '{}", image.display())));
    // Run the generated script with the real system shell: PATH lookup of bare
    // `grok` must reach the user wrapper, which then execs the native image.
    let launch = sandbox.parent.join("launch.sh");
    std::fs::write(&launch, &script).unwrap();
    let output = std::process::Command::new("/bin/sh").arg(&launch).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(std::fs::read_to_string(&marker).expect("the user wrapper genuinely ran"), "grok|--model\n");
    let echoed = String::from_utf8(output.stdout).unwrap();
    assert!(echoed.starts_with("--model grok-4.7 --cwd "), "{echoed}");
    assert!(echoed.contains("--always-approve"));
}
#[test]
fn private_primary_rejects_git_and_inherited_config_without_reading_bodies() {
    let sandbox = support::Sandbox::new();
    let parent = sandbox.parent.join("primary-owner"); let cwd = parent.join("seat");
    std::fs::create_dir_all(&cwd).unwrap(); let identity = resolve_cwd(&cwd).unwrap();
    require_private_primary(&identity).unwrap();
    std::fs::write(parent.join(".git"), b"not read").unwrap();
    assert!(require_private_primary(&identity).is_err());
    std::fs::remove_file(parent.join(".git")).unwrap();
    std::fs::create_dir(parent.join(".grok")).unwrap();
    std::fs::write(parent.join(".grok/config.toml"), b"not parsed or merged").unwrap();
    assert!(require_private_primary(&identity).is_err());
}
#[test]
fn launch_script_execs_the_bare_public_command_and_entry_bytes_are_fenced() {
    let sandbox = support::Sandbox::new(); let wrapper = sandbox.parent.join("approved-wrapper");
    std::fs::write(&wrapper, b"exec final image after approved route").unwrap();
    let stamp = capture_launch_file(&wrapper, digest(b"exec final image after approved route"), 1024, Duration::from_secs(2)).unwrap();
    verify_executable_inputs(std::slice::from_ref(&stamp)).unwrap();
    let descriptor = support::descriptor(); let request = support::request(&sandbox.root, "worker");
    let resolved = resolve_launch(&descriptor, &support::hooks(), &request, None).unwrap();
    let mut plan = support::Fake.plan(&resolved).unwrap();
    validate_launch_plan(&descriptor, &resolved, &plan).unwrap();
    let script = String::from_utf8(team_agent_contract::host::shell::launch_script(&plan, &Default::default()).unwrap()).unwrap();
    assert!(script.contains("\nexport PATH='/fake'\nexec 'fake' 'chat' "), "{script}");
    assert!(!script.contains("exec '/"), "no absolute image or entry is ever exec'd: {script}");
    // Neither the entry path nor the inner image can replace the bare name.
    plan.launcher.command = "/fake/bin".into();
    assert!(team_agent_contract::host::shell::launch_script(&plan, &Default::default()).is_err());
    std::fs::write(&wrapper, b"replaced").unwrap(); assert!(verify_executable_inputs(&[stamp]).is_err());
    assert!(capture_launch_file(&wrapper, digest(b"original"), 1024, Duration::from_secs(2)).is_err());
}
