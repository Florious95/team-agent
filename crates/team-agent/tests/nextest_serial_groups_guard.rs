//! Guard for `.config/nextest.toml`. nextest runs each test in its own process,
//! so an in-process `#[serial(key)]` (other than `env`) or a static mutex that
//! serializes an integration binary's tests no longer serializes anything.
//! Every such site must be mapped to a `max-threads = 1` group or to the
//! exclusive tier, and the three copies of the exclusive set must agree.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

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

/// Keys of every serial_test attribute in `source` except the `env` key.
fn non_env_serial_keys(source: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for line in source.lines().map(str::trim) {
        let Some(rest) = line.strip_prefix("#[serial_test::serial").or_else(|| line.strip_prefix("#[serial")) else {
            continue;
        };
        match rest.strip_prefix('(').and_then(|args| args.split(')').next()) {
            Some(args) => keys.extend(args.split(',').map(str::trim).filter(|key| *key != "env").map(String::from)),
            None if rest.starts_with(']') => keys.push("<bare>".into()),
            None => {}
        }
    }
    keys
}

/// The nextest filter token that must cover tests defined in `path`.
fn covering_token(crate_root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(crate_root).unwrap();
    let mut parts: Vec<String> = relative.iter().map(|part| part.to_string_lossy().into_owned()).collect();
    if parts[0] == "src" {
        let file = parts.pop().unwrap();
        let stem = file.trim_end_matches(".rs");
        if stem != "mod" {
            parts.push(stem.to_string());
        }
        format!("test(/^{}::/)", parts[1..].join("::"))
    } else {
        assert_eq!(parts.len(), 2, "serialized test outside a top-level integration target: {}", relative.display());
        format!("binary(={})", parts[1].trim_end_matches(".rs"))
    }
}

fn exclusive_copies(config: &str) -> Vec<String> {
    let lines: Vec<&str> = config.lines().collect();
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains("EXCLUSIVE-SET (copy"))
        .map(|(index, _)| {
            let value = lines[index + 1].split_once('=').unwrap().1.trim().trim_matches('\'');
            value.strip_prefix("not (").and_then(|inner| inner.strip_suffix(')')).unwrap_or(value).to_string()
        })
        .collect()
}

#[test]
fn every_cross_process_serialization_need_is_mapped_in_nextest_config() {
    let root = repo_root();
    let config = std::fs::read_to_string(root.join(".config/nextest.toml")).unwrap();
    let crate_root = root.join("crates/team-agent");
    let mut files = Vec::new();
    rust_files(&crate_root.join("src"), &mut files);
    rust_files(&crate_root.join("tests"), &mut files);
    files.sort();

    let mut unmapped = Vec::new();
    let mut mapped = 0;
    for path in &files {
        let source = std::fs::read_to_string(path).unwrap();
        let keys = non_env_serial_keys(&source);
        let static_mutex = path.parent() == Some(crate_root.join("tests").as_path())
            && source.lines().any(|line| line.starts_with("static ") && line.contains("Mutex<()>"));
        if keys.is_empty() && !static_mutex {
            continue;
        }
        let token = covering_token(&crate_root, path);
        if config.contains(&token) {
            mapped += 1;
        } else {
            unmapped.push(format!("{} keys={keys:?} static_mutex={static_mutex} needs {token}", path.display()));
        }
    }

    let copies = exclusive_copies(&config);
    assert!(
        unmapped.is_empty() && mapped > 0 && copies.len() == 3 && copies.iter().all(|copy| copy == &copies[0]),
        "nextest config must serialize every in-process serial key and static-mutex binary across processes, \
         and keep three identical exclusive-set copies; mapped={mapped}; unmapped={unmapped:#?}; copies={copies:#?}"
    );
}
