use std::fs;
use std::path::Path;

pub fn source_tree(rels: &[&str]) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = String::new();
    for rel in rels {
        append_rs(&root.join(rel), &mut out);
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
