//! Phase 1d Batch 0 C-4 grep guard: compact/default `status --json`
//! MUST stay byte-stable while backend fields are added elsewhere.
//!
//! Concretely: the `compact_status` serializer path in
//! `crates/team-agent/src/cli/status_port.rs` must NOT reference
//! backend/pipe/shim fields when it strips the payload down to
//! compact shape. Callers that want backend detail have to opt into
//! `--detail`.
//!
//! Rationale: Batch 3 will add backend detail to `--json --detail`
//! (design §Batch 3, medium/high golden risk); we lock the compact
//! shape here so tmux teams see byte-equivalent compact output after
//! the factory migration lands.
//!
//! (CR C-4, `.team/artifacts/0.5.x-backend-factory-cr-verdict.md:198-202`.)

use std::path::PathBuf;

fn status_port_src() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("cli")
        .join("status_port.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("cannot read cli/status_port.rs at {}: {e}", path.display())
    })
}

/// Locate the body of the `fn compact_status(...)` helper, or return
/// None if the fn is not present at all.
fn compact_status_body(src: &str) -> Option<String> {
    let sig = src.find("fn compact_status")?;
    // Find the opening `{` of the body.
    let after = &src[sig..];
    let brace = after.find('{')?;
    let start = sig + brace;
    // Walk brace depth to find the matching close.
    let bytes = src.as_bytes();
    let mut depth: i32 = 0;
    for (i, &b) in bytes[start..].iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(src[start..start + i + 1].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[test]
fn c4_compact_status_serializer_does_not_reference_backend_fields() {
    let src = status_port_src();
    let Some(body) = compact_status_body(&src) else {
        // If `compact_status` fn does not exist, the guard trivially
        // passes — compact output has no separate serializer to police.
        // Emit a warning so someone re-checks if the shape changes.
        eprintln!(
            "note: `fn compact_status` not found in cli/status_port.rs; guard passes vacuously"
        );
        return;
    };
    for forbidden in [
        "backend_kind",
        "pipe_name",
        "shim_pid",
        "child_pid",
        "transport.kind",
        "transport.source",
    ] {
        assert!(
            !body.contains(forbidden),
            "CR C-4 grep guard: compact status serializer references backend field \
             {forbidden:?} — compact JSON must stay byte-stable after Batch 3. Move \
             this field into the `--detail` branch."
        );
    }
}

#[test]
fn c4_no_top_level_status_json_backend_kind_leak() {
    // Belt-and-braces: the *full* status JSON builder (before the
    // compact strip) is allowed to include backend detail behind the
    // detail flag. Here we assert only that no early-return
    // path leaks backend fields OUTSIDE a `detail` branch. We approximate
    // this by scanning every line that inserts a `"backend_kind"` /
    // `"pipe_name"` etc. and requiring the enclosing scope mentions
    // `detail`.
    let src = status_port_src();
    let interesting = [
        "\"backend_kind\"",
        "\"pipe_name\"",
        "\"shim_pid\"",
        "\"child_pid\"",
    ];
    let mut offenders = Vec::new();
    for (idx, line) in src.lines().enumerate() {
        for pat in interesting {
            if line.contains(pat) {
                let start = idx.saturating_sub(30);
                let end = (idx + 5).min(src.lines().count());
                let window: String = src.lines().skip(start).take(end - start).collect::<Vec<_>>().join("\n");
                let scope_ok =
                    window.contains("detail") || window.contains("--detail") || window.contains("if detail");
                if !scope_ok {
                    offenders.push(format!("line {}: `{}` — {pat}", idx + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "CR C-4: {} status-port insertions of backend-detail fields appear OUTSIDE a `detail`-guarded scope:\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}
