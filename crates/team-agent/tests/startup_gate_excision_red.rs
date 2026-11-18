//! 0.6.2 owner ruling: `send` is reliable physical tmux delivery. No startup,
//! MCP-handshake or readiness loop may gate it, and no dead gate helper may remain.
use std::path::{Path, PathBuf};

const GATE_IDENTIFIERS: &[&str] = &[
    "wait_for_startup",
    "legacy_startup_pending",
    "startup_observed",
    "startup_handshake_observed",
    "startup_checked",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Drop `//` line comments and `/* */` block comments; identifiers in prose do not count.
fn code_only(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix("//") {
            rest = tail.find('\n').map_or("", |at| &tail[at..]);
        } else if let Some(tail) = rest.strip_prefix("/*") {
            rest = tail.find("*/").map_or("", |at| &tail[at + 2..]);
        } else {
            let ch = rest.chars().next().unwrap();
            out.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    out
}

fn word_hits(text: &str, word: &str) -> usize {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word)
        .filter(|(at, _)| {
            !text[..*at].chars().next_back().is_some_and(ident)
                && !text[at + word.len()..].chars().next().is_some_and(ident)
        })
        .count()
}

#[test]
fn startup_gate_helpers_are_physically_excised_from_both_crates() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&manifest.join("src"), &mut files);
    rust_files(&manifest.join("../team-agent-contract/src"), &mut files);
    let mut hits = Vec::new();
    for file in &files {
        let code = code_only(&std::fs::read_to_string(file).unwrap());
        for word in GATE_IDENTIFIERS {
            let count = word_hits(&code, word);
            if count > 0 {
                hits.push(format!("{} x{count} in {}", word, file.display()));
            }
        }
    }
    assert!(hits.is_empty(), "startup-gate residue remains:\n{}", hits.join("\n"));
}

#[test]
fn send_help_and_operator_docs_promise_no_startup_wait() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let help = std::fs::read_to_string(manifest.join("src/cli/emit.rs")).unwrap();
    for phrase in ["cold starts wait", "launch-scoped initialization evidence", "readiness timeout injects no input"] {
        assert!(!help.contains(phrase), "send help still promises a startup wait: {phrase:?}");
    }
    for doc in [
        "../../docs/reference/team-agent-operator.md",
        "../../skills/team-agent/references/team-agent-operator.md",
        "../../skills/team-agent/docs/reference/team-agent-operator.md",
    ] {
        let text = std::fs::read_to_string(manifest.join(doc)).unwrap();
        assert!(!text.contains("wait up to 60 seconds for initialization evidence"), "{doc} still documents the gate");
        assert!(text.contains("never wait for peer startup or MCP handshake evidence"), "{doc} lost the delivery contract");
    }
}

#[test]
fn guard_helpers_ignore_comments_and_match_whole_words() {
    let code = code_only("// wait_for_startup\nlet a = 1; /* startup_observed */\nfn startup_observed_extra() {}\nfn x() { startup_observed(); }\n");
    assert_eq!(word_hits(&code, "wait_for_startup"), 0);
    assert_eq!(word_hits(&code, "startup_observed"), 1);
}
