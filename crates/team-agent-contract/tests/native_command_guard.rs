//! Static guard: contract providers launch their public command through PATH.
//! Install directories, inner images and launcher hacks must not reappear in
//! production code; behavior is proven separately by the wrapper tests.
use std::path::{Path, PathBuf};

const FORBIDDEN: &[&str] = &[
    "/usr/local/",
    "/opt/homebrew",
    "/Applications/",
    ".local/bin",
    "node_modules",
    "grok-native",
    "team-agent-grok-name",
    "GROK_REAL",
    "hard_link(",
    "--use-system-ca",
    "CURSOR_INVOKED_AS",
    "NODE_COMPILE_CACHE",
];

fn sources(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

/// Production lines only: comments are skipped and a file's trailing test
/// module (the first top-level `#[cfg(test)]`) ends the scan.
fn production(path: &Path) -> Vec<(usize, String)> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .enumerate()
        .take_while(|(_, line)| !line.starts_with("#[cfg(test)]") && !line.starts_with("#[cfg(all(test"))
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .map(|(index, line)| (index + 1, line.to_owned()))
        .collect()
}

fn scanned() -> Vec<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = vec![];
    sources(&manifest.join("src"), &mut files);
    sources(&manifest.join("../team-agent/src/contract_runtime"), &mut files);
    assert!(files.len() > 20, "guard must scan the contract tree: {files:?}");
    files
}

#[test]
fn no_provider_install_path_inner_image_or_launcher_hack_in_production_code() {
    let mut violations = vec![];
    for path in scanned() {
        for (number, line) in production(&path) {
            for forbidden in FORBIDDEN {
                if line.contains(forbidden) {
                    violations.push(format!("{}:{number}: {forbidden}", path.display()));
                }
            }
        }
    }
    assert!(violations.is_empty(), "hardcoded provider paths or launch hacks:\n{}", violations.join("\n"));
}

#[test]
fn every_provider_plan_execs_the_framework_resolved_public_command() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut plans = 0;
    for path in [manifest.join("src/kiro.rs"), manifest.join("src/providers/cursor.rs"), manifest.join("src/providers/grok_build.rs")] {
        let text = std::fs::read_to_string(&path).unwrap();
        for (offset, _) in text.match_indices("LaunchPlan {") {
            if text[..offset].ends_with("Ok(") {
                plans += 1;
                let body: String = text[offset..].chars().take(600).collect();
                assert!(body.contains("launcher: r.paths.launcher.clone()") || body.contains("launcher: request.paths.launcher.clone()"),
                    "{} plans a launcher other than the framework-resolved public command", path.display());
            }
        }
    }
    assert_eq!(plans, 3, "one plan per contract provider");
    // Discovery resolves each provider's public command through PATH, as found.
    let discovery = std::fs::read_to_string(manifest.join("../team-agent/src/contract_runtime/discovery.rs")).unwrap();
    assert!(discovery.contains("public_command(descriptor.identity.binary)"));
}
