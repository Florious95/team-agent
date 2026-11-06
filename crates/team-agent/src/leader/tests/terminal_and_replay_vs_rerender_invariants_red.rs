//! Retained guard against regressing delivered rows through legacy SQL.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::PathBuf;
use serial_test::serial;

#[test]
#[serial(env)]
fn restart_never_marks_delivered_as_accepted() {
    // Source-scan: no file may contain a SQL literal that translates a
    // `delivered` row to `accepted`. We check for the substring
    // `set status = 'accepted'` inside any block that also mentions
    // `delivered`. This is a coarse guard; the more precise test lives
    // in tooth 1 (behavioral). Both are cheap and complementary.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("src");
    let mut violations: Vec<PathBuf> = Vec::new();
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        let Ok(es) = std::fs::read_dir(dir) else {
            return;
        };
        for e in es.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().and_then(|s| s.to_str()) == Some("tests") {
                    continue;
                }
                walk(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                if let Ok(t) = std::fs::read_to_string(&p) {
                    let lines: Vec<&str> = t.lines().collect();
                    for (i, l) in lines.iter().enumerate() {
                        if l.contains("set status = 'accepted'") {
                            let start = i.saturating_sub(20);
                            let end = (i + 20).min(lines.len());
                            let window = lines[start..end].join("\n");
                            // Trigger only if `delivered` (not
                            // `delivered_at` alone) is treated as
                            // eligible.
                            if window.contains("status = 'delivered'")
                                || window.contains("'delivered'")
                            {
                                out.push(p.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    walk(&src, &mut violations);
    // 0.5.55 storm proved this invariant held (delivered was untouched).
    // Green at baseline; locked here.
    assert!(
        violations.is_empty(),
        "regression: SQL that flips a `delivered` row to `accepted` found: {:?}. \
         §3.3#5 storm proof — delivered must be irreversibly terminal.",
        violations
    );
}
