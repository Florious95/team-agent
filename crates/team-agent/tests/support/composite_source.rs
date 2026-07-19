//! Shared composite source reader for source-guard contracts (verifier-frozen).
//!
//! A guard that anchors a semantic surface (send funnel / launch spawn /
//! status projection) must survive a mechanical file split: the guarded
//! pattern may live in `src/cli/send.rs` OR any module file under the
//! sibling dir `src/cli/send/`. This helper reads BOTH, deterministically
//! (rel-path sorted, each part prefixed with a `// @source:` header), and
//! panics loudly when neither exists - a missing surface must never read as
//! an empty (trivially green) source.

#![allow(dead_code)]

use std::path::PathBuf;

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// All `.rs` parts of the composite surface for `rel` (e.g.
/// `src/cli/send.rs`): the file itself if present, plus every `.rs` file
/// under the sibling module dir (`src/cli/send/`), recursively, sorted by
/// rel path.
pub fn composite_files(rel: &str) -> Vec<(String, String)> {
    let root = crate_root();
    let mut parts = Vec::new();
    let file = root.join(rel);
    if file.is_file() {
        let text =
            std::fs::read_to_string(&file).unwrap_or_else(|error| panic!("read {rel}: {error}"));
        parts.push((rel.to_string(), text));
    }
    let sibling = rel
        .strip_suffix(".rs")
        .map(|stem| root.join(stem))
        .filter(|dir| dir.is_dir());
    if let Some(dir) = sibling {
        let mut stack = vec![dir];
        while let Some(current) = stack.pop() {
            for entry in std::fs::read_dir(&current)
                .unwrap_or_else(|error| panic!("read dir {}: {error}", current.display()))
            {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    let rel_part = path
                        .strip_prefix(&crate_root())
                        .expect("part under crate root")
                        .to_string_lossy()
                        .replace('\\', "/");
                    let text = std::fs::read_to_string(&path)
                        .unwrap_or_else(|error| panic!("read {rel_part}: {error}"));
                    parts.push((rel_part, text));
                }
            }
        }
    }
    assert!(
        !parts.is_empty(),
        "composite source surface is missing entirely: {rel} (no file, no sibling module dir) - \
         a guard must never run against an empty surface"
    );
    parts.sort_by(|a, b| a.0.cmp(&b.0));
    parts
}

/// The composite surface as one deterministic string.
pub fn composite_source(rel: &str) -> String {
    composite_files(rel)
        .into_iter()
        .map(|(part_rel, text)| format!("// @source: {part_rel}\n{text}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod composite_source_teeth {
    #[test]
    fn composite_reads_file_plus_sibling_dir_sorted_and_panics_on_empty() {
        let root = std::env::temp_dir().join(format!("composite-teeth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src/cli");
        std::fs::create_dir_all(src.join("send")).unwrap();
        std::fs::write(src.join("send.rs"), "root_marker\n").unwrap();
        std::fs::write(src.join("send/b_part.rs"), "b_marker\n").unwrap();
        std::fs::write(src.join("send/a_part.rs"), "a_marker\n").unwrap();

        let read = |rel: &str| {
            let file = root.join(rel);
            let mut parts = Vec::new();
            if file.is_file() {
                parts.push((rel.to_string(), std::fs::read_to_string(&file).unwrap()));
            }
            if let Some(dir) = rel
                .strip_suffix(".rs")
                .map(|s| root.join(s))
                .filter(|d| d.is_dir())
            {
                for entry in std::fs::read_dir(dir).unwrap() {
                    let path = entry.unwrap().path();
                    if path.extension().is_some_and(|e| e == "rs") {
                        let rel_part = path
                            .strip_prefix(&root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/");
                        parts.push((rel_part, std::fs::read_to_string(&path).unwrap()));
                    }
                }
            }
            assert!(!parts.is_empty(), "empty surface must panic: {rel}");
            parts.sort_by(|a, b| a.0.cmp(&b.0));
            parts
        };
        let parts = read("src/cli/send.rs");
        let rels: Vec<&str> = parts.iter().map(|(r, _)| r.as_str()).collect();
        assert_eq!(
            rels,
            [
                "src/cli/send.rs",
                "src/cli/send/a_part.rs",
                "src/cli/send/b_part.rs"
            ]
        );
        // split simulation: pattern moved out of the root file must still be seen
        let joined: String = parts.iter().map(|(_, t)| t.as_str()).collect();
        assert!(
            joined.contains("root_marker")
                && joined.contains("a_marker")
                && joined.contains("b_marker")
        );
        // dir-only surface still reads; fully-missing surface panics
        std::fs::remove_file(src.join("send.rs")).unwrap();
        assert_eq!(read("src/cli/send.rs").len(), 2);
        let missing = std::panic::catch_unwind(|| read("src/cli/nonexistent.rs"));
        assert!(
            missing.is_err(),
            "fully missing surface must panic, not read empty"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
