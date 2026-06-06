//! F032 / BUG-RS-F6.4-1 contract: coordinator/lifecycle/messaging are provider-adapter only.
//!
//! Provider-specific startup prompt behavior belongs behind `ProviderAdapter`.
//! Upper orchestration layers may parse provider data and request an adapter, but
//! they must not name Codex/Claude/compatible-api implementation functions or
//! keep Codex-only branches for startup prompt handling.

use std::path::{Path, PathBuf};

#[derive(Debug)]
struct Finding {
    file: String,
    line: usize,
    pattern: &'static str,
    text: String,
}

#[test]
fn f032_upper_layers_do_not_direct_call_provider_specific_prompt_handlers() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let roots = ["src/coordinator", "src/lifecycle", "src/messaging"];
    let mut findings = Vec::new();
    for root in roots {
        scan_tree(&manifest, &manifest.join(root), &mut findings);
    }

    assert!(
        findings.is_empty(),
        "F032/F6.4 violation: coordinator/lifecycle/messaging must use \
         ProviderAdapter::handle_startup_prompts instead of provider-specific \
         functions or Codex-only startup prompt branches. Findings:\n{}",
        format_findings(&findings)
    );
}

#[test]
fn f032_provider_adapter_trait_exposes_startup_prompt_capability() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let adapter_rs = manifest.join("src/provider/adapter.rs");
    let source = std::fs::read_to_string(adapter_rs).expect("read provider/adapter.rs");
    assert!(
        source.contains("fn handle_startup_prompts("),
        "ProviderAdapter trait must expose handle_startup_prompts(transport, target, checks, sleep_s) -> Vec<HandledPrompt>"
    );
    assert!(
        source.contains("Vec<crate::provider::HandledPrompt>")
            || source.contains("Vec<HandledPrompt>"),
        "ProviderAdapter::handle_startup_prompts must return Vec<HandledPrompt> so non-Codex adapters can return an empty vec"
    );
}

fn scan_tree(manifest: &Path, root: &Path, findings: &mut Vec<Finding>) {
    let entries = std::fs::read_dir(root).expect("read source root");
    for entry in entries {
        let path = entry.expect("read source entry").path();
        if path.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) == Some("tests") {
                continue;
            }
            scan_tree(manifest, &path, findings);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            scan_file(manifest, &path, findings);
        }
    }
}

fn scan_file(manifest: &Path, path: &Path, findings: &mut Vec<Finding>) {
    let rel = path
        .strip_prefix(manifest)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let source = std::fs::read_to_string(path).expect("read source file");
    for (idx, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("//!") {
            continue;
        }
        for pattern in forbidden_patterns() {
            if matches_forbidden_pattern(trimmed, pattern) {
                findings.push(Finding {
                    file: rel.clone(),
                    line: idx + 1,
                    pattern,
                    text: trimmed.to_string(),
                });
            }
        }
    }
}

fn forbidden_patterns() -> &'static [&'static str] {
    &[
        "provider::codex_handle_startup_prompts",
        "provider::claude_code",
        "provider::compatible_api",
        "Provider::Codex startup branch",
        "handle_codex_startup_prompts_after_spawn",
    ]
}

fn matches_forbidden_pattern(line: &str, pattern: &'static str) -> bool {
    match pattern {
        "Provider::Codex startup branch" => {
            line.contains("matches!(provider, Provider::Codex")
                || line.contains("!= Some(crate::model::enums::Provider::Codex")
                || line.contains("== Some(crate::model::enums::Provider::Codex")
                || line.contains("== Provider::Codex")
                || line.contains("!= Provider::Codex")
        }
        other => line.contains(other),
    }
}

fn format_findings(findings: &[Finding]) -> String {
    findings
        .iter()
        .map(|f| format!("{}:{} {} :: {}", f.file, f.line, f.pattern, f.text))
        .collect::<Vec<_>>()
        .join("\n")
}
