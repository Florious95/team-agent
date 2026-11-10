#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::path::{Path, PathBuf};

fn sources(root: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            sources(&entry.path(), files);
        } else if kind.is_file() && entry.path().extension().is_some_and(|ext| ext == "rs") {
            files.push(entry.path());
        }
    }
}

fn patterns() -> [regex::Regex; 3] {
    [
        r"while\s+(?::|true)\s*;\s*do\s+sleep\s+[0-9.]+\s*;?\s*done",
        r"(?s)\bloop\s*\{[^}]*\bfrom_secs\s*\(\s*3600\s*\)",
        r"&\s*echo\s+\$!",
    ]
    .map(|pattern| regex::Regex::new(pattern).unwrap())
}

#[test]
fn test_fixtures_cannot_reintroduce_unbounded_loops_or_unguarded_detachment() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let roots = [
        manifest.join("tests"),
        manifest.join("../team-agent-contract/tests"),
    ];
    let guarded_detachment = manifest
        .join("tests/support/proc_guard.rs")
        .canonicalize()
        .unwrap();
    let patterns = patterns();
    let mut files = Vec::new();
    for root in &roots {
        sources(root, &mut files);
    }
    files.sort();
    let mut violations = Vec::new();
    for path in &files {
        let source = std::fs::read_to_string(path).unwrap();
        for (rule, pattern) in patterns.iter().enumerate() {
            if rule == 2 && path.canonicalize().unwrap() == guarded_detachment {
                continue;
            }
            for found in pattern.find_iter(&source) {
                let line = source[..found.start()]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count()
                    + 1;
                violations.push(format!(
                    "{}:{line}: unsafe fixture rule {}",
                    path.display(),
                    rule + 1
                ));
            }
        }
    }
    assert!(violations.is_empty(), "{}", violations.join("\n"));
    println!(
        "O3: {} test sources scanned; no unsafe fixture patterns",
        files.len()
    );
}

#[test]
fn lifetime_ratchet_recognizes_each_forbidden_shape() {
    let patterns = patterns();
    for kind in [":", "true"] {
        assert!(patterns[0].is_match(&format!("while {kind}; do sleep 0.02; done")));
    }
    assert!(patterns[1].is_match(
        &[
            "loop { std::thread::sleep(Duration::from_secs(",
            "3600",
            ")); }"
        ]
        .concat()
    ));
    assert!(patterns[2].is_match(&["& echo ", "$!"].concat()));
    assert!(!patterns
        .iter()
        .any(|pattern| pattern.is_match("sleep 60 & wait $!")));
}
