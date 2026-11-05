use super::*;

// =========================================================================
// STEP-14 DIVERGENCE RED LANE — golden-pinned tests that FAIL against the
// current Rust port. Golden = team-agent-public @ v0.2.11 (439bef8),
// probed via PYTHONPATH=.../src python3 /tmp/probe_cli_all.py.
// Each test encodes the EXACT golden value; the porters green these next.
// =========================================================================

// ---- #1 / #15: classify_agent_bucket — raw "idle" with NO health => Unknown ----
// golden _agent_summary_counts has NO `raw == "idle"` arm (commands.py:320):
// idle is gated SOLELY on health.status=="idle". A raw "idle" with empty health
// falls through every branch to the final `else: unknown += 1`.
// golden probe: _agent_summary_counts({"a":{"status":"idle"}},{}) -> unknown=1.
// Rust cli.rs:601 adds `|| raw == "idle"` => Idle (WRONG).

// ---- #2 / #21 / #24: format_latest_result faithfulness ----
// golden _latest_result_line (commands.py:333-337):
//   summary = str(summary or "").replace("\n"," ")[:80]; printed as `{summary or '-'}`;
//   agent_id printed as `{agent_id or '-'}`;
//   created_at rendered through runtime._age_text ('-' for None/'' /invalid ISO; 'Nh ago' for valid).
// Rust format_latest_result passes summary verbatim, uses unwrap_or("-") (empty stays empty),
// and prints created_at raw (no age_text).



// ---- #3: format_status_summary — falsy first latest_results element -> "none" ----
// golden (commands.py:268,333-335): latest = (latest_results or [{}])[0] if latest_results else None;
// _latest_result_line returns 'latest result: none' when the first element is falsy (None or {}).
// Rust .first() returns Some for [Null]/[{}] -> renders '- -> - @ -'.

// ---- #4: format_status_summary — empty-string falsy fallbacks + current_command ----
// golden: '' is falsy via Python `or`:
//   coordinator status '' -> 'stopped' (commands.py:284)
//   pane_id '' -> '-' (line 285)
//   cmd = pane_current_command or current_command or '-' (line 285) — note the current_command fallback.
// Rust serde unwrap_or keeps '' verbatim and has NO current_command read.



// ---- #5 (P2): format_status_summary — Python bool() truthiness coercion ----
// golden: schema_ok / tmux via bool(...) — int 1 -> True, string 'yes' -> True (commands.py:284).
// Rust as_bool() returns None for int/str -> false.

// ---- #6 / #18 / #26: parse_inbox_entries block grouping + title strip + [:80] cap ----
// golden _leader_inbox_entries groups blocks on `[` + 'fallback'; _leader_inbox_entry_title
// strips bracket/'Team Agent'/'Message id:'/'Task id:'/'From:'/'To:'/'Requires ack:'/'Artifacts:'
// lines, joins remaining content with single spaces, [:80] cap.
#[test]
fn red_inbox_realistic_two_message_grouping_and_metadata_strip() {
    // golden full summary on a realistic 2-message fallback inbox (metadata + bodies):
    let ws = tmp_workspace();
    let inbox = ws.join(".team").join("runtime").join("leader-inbox.log");
    let raw = "[m1 fallback]\nTeam Agent\nMessage id: m1\nFrom: worker-1\nTo: leader\n\
Requires ack: yes\nPlease review the PR and approve the deploy. It is blocking the release.\n\
[m2 fallback]\nTeam Agent\nFrom: worker-2\nBuild failed on CI, see logs.";
    std::fs::write(&inbox, raw).unwrap();
    let summary = consume_leader_inbox_summary(&ws, 500).expect("Some");
    let expected = "Leader inbox: 2 new fallback entries\n\
- Please review the PR and approve the deploy. It is blocking the release.\n\
- Build failed on CI, see logs.\n\
Hint: team-agent inbox leader";
    assert_eq!(
        summary, expected,
        "golden groups into 2 entries, strips Team Agent/Message id/From/To/Requires ack metadata"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn red_inbox_non_fallback_bracket_stays_in_entry() {
    // golden: a `[...]` WITHOUT 'fallback' is content, not a header:
    //   '[only bracket no kw]\nbody line' -> ONE entry titled '[only bracket no kw] body line'.
    let ws = tmp_workspace();
    let inbox = ws.join(".team").join("runtime").join("leader-inbox.log");
    std::fs::write(&inbox, "[only bracket no kw]\nbody line").unwrap();
    let summary = consume_leader_inbox_summary(&ws, 500).expect("Some");
    assert_eq!(
            summary,
            "Leader inbox: 1 new fallback entry\n- [only bracket no kw] body line\nHint: team-agent inbox leader",
            "a non-'fallback' bracket line stays part of the entry body (golden grouping gate)"
        );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn red_inbox_plain_lines_join_into_single_entry() {
    // golden: no fallback header at all -> the whole text is ONE entry, lines joined w/ spaces.
    //   'alpha\nbeta\ngamma' -> 1 entry 'alpha beta gamma'.
    let ws = tmp_workspace();
    let inbox = ws.join(".team").join("runtime").join("leader-inbox.log");
    std::fs::write(&inbox, "alpha\nbeta\ngamma").unwrap();
    let summary = consume_leader_inbox_summary(&ws, 500).expect("Some");
    assert_eq!(
        summary,
        "Leader inbox: 1 new fallback entry\n- alpha beta gamma\nHint: team-agent inbox leader",
        "with no fallback header golden collapses ALL lines into one space-joined entry"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn red_inbox_title_capped_at_80_chars() {
    // golden: a 200-char body title is capped to exactly 80 chars ([:80] per entry).
    let ws = tmp_workspace();
    let inbox = ws.join(".team").join("runtime").join("leader-inbox.log");
    let body = "X".repeat(200);
    std::fs::write(&inbox, format!("[x fallback]\n{body}")).unwrap();
    let summary = consume_leader_inbox_summary(&ws, 500).expect("Some");
    assert_eq!(
        summary,
        format!(
            "Leader inbox: 1 new fallback entry\n- {}\nHint: team-agent inbox leader",
            "X".repeat(80)
        ),
        "each entry title MUST cap at 80 chars (golden [:80]); Rust applies no per-entry cap"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

// ---- #7 / #16: consume_leader_inbox_summary — mid-codepoint cursor must NOT panic ----
// golden seeks to a BYTE offset and decodes errors='replace': a cursor inside a multibyte
// char yields U+FFFD replacement chars and NEVER crashes (bug-084).
// Rust slices &text[offset..] which PANICS on a non-char-boundary. The panic IS the red.
#[test]
fn red_inbox_mid_codepoint_cursor_decodes_replacement_no_panic() {
    // golden probe (/tmp/probe_cli_all.py): inbox '[a fallback]\n世界 message', cursor='14'
    // (byte 14 is inside '世', whose bytes are 13..16) ->
    //   'Leader inbox: 1 new fallback entry\n- ��界 message\nHint: team-agent inbox leader'
    let ws = tmp_workspace();
    let runtime = ws.join(".team").join("runtime");
    std::fs::write(
        runtime.join("leader-inbox.log"),
        "[a fallback]\n世界 message",
    )
    .unwrap();
    std::fs::write(runtime.join("leader-inbox.cursor"), "14").unwrap();
    let summary = consume_leader_inbox_summary(&ws, 500)
        .expect("mid-codepoint cursor MUST degrade gracefully, not crash");
    assert_eq!(
            summary,
            "Leader inbox: 1 new fallback entry\n- \u{FFFD}\u{FFFD}界 message\nHint: team-agent inbox leader",
            "mid-codepoint byte offset MUST yield U+FFFD replacement chars (golden errors='replace')"
        );
    let _ = std::fs::remove_dir_all(&ws);
}

// ---- #8 / #17: consume_leader_inbox_summary — offset>size resets to 0; garbage cursor ----
// golden helpers.py:38-40: offset<0 or offset>size -> offset=0 (re-read whole file);
// a ValueError cursor ('abc') -> offset=0 AND size=0 -> offset==size -> None WITHOUT advancing.
#[test]
fn red_inbox_beyond_size_cursor_resets_to_zero_and_resummarizes() {
    // golden: file 'hello' inbox, cursor='99999' (> size) -> re-read from 0 -> summary; cursor advances.
    let ws = tmp_workspace();
    let runtime = ws.join(".team").join("runtime");
    std::fs::write(runtime.join("leader-inbox.log"), "[a fallback]\nhello").unwrap();
    std::fs::write(runtime.join("leader-inbox.cursor"), "99999").unwrap();
    let summary = consume_leader_inbox_summary(&ws, 500)
        .expect("over-size cursor MUST reset to 0 and re-summarize (golden offset>size => 0)");
    assert_eq!(
        summary, "Leader inbox: 1 new fallback entry\n- hello\nHint: team-agent inbox leader",
        "cursor beyond file size MUST re-read the whole inbox (NOT clamp-to-len-then-None)"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn red_inbox_garbage_cursor_returns_none_without_advancing() {
    // golden: cursor='abc' (ValueError) -> offset=0,size=0 -> offset==size -> None; cursor LEFT 'abc'.
    let ws = tmp_workspace();
    let runtime = ws.join(".team").join("runtime");
    std::fs::write(runtime.join("leader-inbox.log"), "[a fallback]\nhello").unwrap();
    let cursor_path = runtime.join("leader-inbox.cursor");
    std::fs::write(&cursor_path, "abc").unwrap();
    let result = consume_leader_inbox_summary(&ws, 500);
    assert_eq!(
            result, None,
            "an unparseable cursor MUST treat size as 0 (offset==size==0) and return None (golden ValueError)"
        );
    let cursor_after = std::fs::read_to_string(&cursor_path).unwrap();
    assert_eq!(
            cursor_after, "abc",
            "a garbage cursor MUST be left untouched (golden never advances it); Rust overwrites to len"
        );
    let _ = std::fs::remove_dir_all(&ws);
}

// ---- #9: render_inbox_summary — budget measured in CHARS (code points), not bytes ----
// golden uses Python str length (code points). Rust compares byte lengths.
#[test]
fn red_inbox_budget_is_char_count_not_bytes() {
    // golden probe: 30 CJK chars, budget=100 -> char-len 97 <= 100 so golden KEEPS the full title.
    let ws = tmp_workspace();
    let inbox = ws.join(".team").join("runtime").join("leader-inbox.log");
    std::fs::write(&inbox, format!("[x fallback]\n{}", "漢".repeat(30))).unwrap();
    let summary = consume_leader_inbox_summary(&ws, 100).expect("Some");
    assert_eq!(
        summary,
        format!(
            "Leader inbox: 1 new fallback entry\n- {}\nHint: team-agent inbox leader",
            "漢".repeat(30)
        ),
        "budget MUST count code points (97 chars <= 100), not bytes; Rust byte-len drops it"
    );
    assert!(
        !summary.contains("Truncated"),
        "the CJK title fits the char budget and MUST NOT be truncated (golden char-len semantics)"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

// ---- #10: render_inbox_summary — hard-trim when header+footer already exceed budget ----
// golden post-assembly: if len(summary) > budget, body='\n'.join(lines)[:keep].rstrip()
// with keep=max(0,budget-len(footer)-6), then `{body} ...\n{footer}` — even the HEADER is trimmed.
#[test]
fn red_inbox_small_budget_hard_trims_header() {
    // golden probe: budget=80 on 10 entries -> 'Lea ...\nTruncated: ...'
    let ws = tmp_workspace();
    let inbox = ws.join(".team").join("runtime").join("leader-inbox.log");
    let many = (0..10)
        .map(|i| format!("[e{i} fallback]\nMessage number {i} with some text padding here"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&inbox, &many).unwrap();
    let summary = consume_leader_inbox_summary(&ws, 80).expect("Some");
    assert_eq!(
        summary, "Lea ...\nTruncated: more fallback entries available; run team-agent inbox leader",
        "when header+footer alone exceed budget the body (incl header) MUST hard-trim (golden)"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

// ---- #19 / #25: emit human-dict scalar formatting (None/True/False) + nested string quotes ----
// golden emit (helpers.py:16-21): scalar via Python str() -> None->'None', True->'True', False->'False';
// dict/list via json.dumps(ensure_ascii=False) -> string ELEMENTS keep their double quotes.
#[test]
fn red_emit_human_dict_scalar_none_true_false_and_quoted_strings() {
    // golden probe: emit({"k":{"a":1,"b":[1,2]},"s":"hi","u":"世界","n":None,"f":False,"t":True}, False)
    let out = emit(
        &CmdOutput::Json(json!({
            "k": {"a": 1, "b": [1, 2]},
            "s": "hi",
            "u": "世界",
            "n": Value::Null,
            "f": false,
            "t": true,
        })),
        false,
    )
    .expect("dict human emit returns Some");
    assert_eq!(
        out, "k: {\"a\": 1, \"b\": [1, 2]}\ns: hi\nu: 世界\nn: None\nf: False\nt: True",
        "top-level scalar Null/false/true MUST render Python-str 'None'/'False'/'True' (golden)"
    );
}

#[test]
fn red_emit_human_dict_string_elements_keep_quotes_in_collections() {
    // golden probe: emit({"items":["a","b"],"mixed":["x",1,True]}, False)
    //   -> 'items: ["a", "b"]\nmixed: ["x", 1, true]'  (string elements KEEP double quotes;
    //   bool lowercased to json 'true' INSIDE the json.dumps collection).
    let out = emit(
        &CmdOutput::Json(json!({"items": ["a", "b"], "mixed": ["x", 1, true]})),
        false,
    )
    .expect("dict human emit returns Some");
    assert_eq!(
        out, "items: [\"a\", \"b\"]\nmixed: [\"x\", 1, true]",
        "string elements nested in a list MUST keep their double quotes (golden json.dumps)"
    );
}

// ---- #20: send_target None routes to assignee, NEVER broadcast ----
// golden _send_target returns None for a no-target send; send_message(target=None) routes to
// the task assignee / leader receiver — '*' is the ONLY broadcast trigger.
// Rust maps None => MessageTarget::Broadcast (WRONG recipient set).

// Doctor retains bounded triage navigation plus complete diagnostic details.
#[test]
fn cmd_doctor_comms_human_is_compact_triage_without_boundary_prefix() {
    let mut args = DoctorArgs {
        spec: None,
        workspace: PathBuf::from("."),
        gate: None,
        comms: true,
        team: None,
        fix: false,
        fix_schema: false,
        cleanup_orphans: false,
        confirm: false,
        json: false,
    };
    let result = cmd_doctor(&args).expect("comms doctor returns CmdResult");
    let text = match result.output {
        CmdOutput::Human(s) => s,
        other => panic!("comms WITHOUT --json must be human triage, got {other:?}"),
    };
    args.json = true;
    let CmdOutput::Json(report) = cmd_doctor(&args).unwrap().output else {
        panic!("--json must preserve the structured report");
    };
    assert_eq!(text, crate::cli::triage::render("doctor", &report));
    assert!(text.starts_with("doctor:"));
    assert_eq!(text.lines().count(), 2 + report["issues"].as_array().unwrap().len()
        + report["suggested_repairs"].as_array().unwrap().len());
    assert!(text.lines().last().unwrap().starts_with("details: "));
}

// ---- #13 / #27 (P2): run() must NOT treat 'claude_code' as a passthrough trigger ----
// golden parser.py:86: only raw_argv[0] in {'codex','claude'} triggers leader passthrough.
// 'claude_code' is the internal provider name, NOT a CLI subcommand (argparse would reject it).
// Rust run() (cli.rs:1305) adds a 'claude_code' arm.
#[test]
fn red_run_claude_code_is_not_a_passthrough_trigger() {
    // golden: `team-agent claude_code -h` is an invalid choice (NOT a clean passthrough exit).
    // Rust currently routes it to cmd_leader_passthrough("claude_code",["-h"]) -> CmdResult::none()
    //   -> ExitCode::Ok. Golden would NOT treat it as a valid leader passthrough.
    let exit = run(
        &["claude_code".to_string(), "-h".to_string()],
        Path::new("."),
    );
    assert_ne!(
            exit,
            ExitCode::Ok,
            "'claude_code' MUST NOT be a leader passthrough trigger (golden gate is {{codex,claude}} only)"
        );
    // codex/claude REMAIN valid passthrough triggers (the -h fast path returns Ok).
    assert_eq!(
        run(&["codex".to_string(), "-h".to_string()], Path::new(".")),
        ExitCode::Ok
    );
    assert_eq!(
        run(&["claude".to_string(), "-h".to_string()], Path::new(".")),
        ExitCode::Ok
    );
}
