use std::fs;
use std::path::Path;

pub fn source_tree(rels: &[&str]) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = String::new();
    for rel in rels {
        let path = root.join(rel);
        // Composite surface: a mechanically split `foo.rs` may continue in
        // its sibling module dir `foo/`; a guard anchors the surface, not
        // one file. At least one side must exist - never guard emptiness.
        let sibling = rel
            .strip_suffix(".rs")
            .map(|stem| root.join(stem))
            .filter(|dir| dir.is_dir());
        assert!(
            path.exists() || sibling.is_some(),
            "guarded source surface missing entirely: {rel}"
        );
        if path.exists() {
            append_rs(&path, &mut out);
        }
        if let Some(dir) = sibling {
            append_rs(&dir, &mut out);
        }
    }
    out
}

fn append_rs(path: &Path, out: &mut String) {
    if path.is_file() {
        if path.extension().is_some_and(|ext| ext == "rs") {
            let source = fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("read source {}: {e}", path.display()));
            for line in source.lines() {
                if !line.trim_start().starts_with("//") {
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        return;
    }
    let mut entries = fs::read_dir(path)
        .unwrap_or_else(|e| panic!("walk source {}: {e}", path.display()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|e| panic!("walk source {}: {e}", path.display()));
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        append_rs(&entry.path(), out);
    }
}
