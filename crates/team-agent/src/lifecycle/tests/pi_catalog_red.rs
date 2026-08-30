use std::collections::BTreeSet;
use std::path::Path;

use crate::lifecycle::launch::pi_mcp::{
    parse_pi_list_models_table, select_exact_pi_model, verify_started_pi_model,
};
use crate::model::enums::ProviderEffort;
use crate::provider::adapters::pi::{build_pi_command_argv, PiCommandRequest, PiSessionSelector};

const CATALOG: &[u8] = include_bytes!("fixtures/pi_list_models_g0.stdout.txt");

fn argv_has_pair(argv: &[String], flag: &str, value: &str) -> bool {
    argv.windows(2)
        .any(|pair| pair[0] == flag && pair[1] == value)
}

fn command_for_model(model: &str) -> Vec<String> {
    build_pi_command_argv(PiCommandRequest {
        executable: Path::new("/verified/pi"),
        extension: Path::new("/workspace/.team/runtime/pi/t1/w1/team-mcp.ts"),
        model,
        effort: ProviderEffort::Medium,
        system_prompt: "worker contract",
        tool_categories: &["mcp_team"],
        session_dir: Path::new("/workspace/.team/runtime/pi/t1/w1/sessions"),
        session: PiSessionSelector::Fresh {
            session_id: "716ba0cb-c491-4471-b41a-43c8d5e1f05a",
        },
        agent_id: "w1",
    })
    .expect("catalog member must build a semantic Pi argv")
}

#[test]
fn pi_catalog_discovers_all_exact_ids_and_rejects_invalid_or_ambiguous_selection() {
    let models = parse_pi_list_models_table(CATALOG).expect("frozen unfiltered table must parse");
    let unique: BTreeSet<&str> = models.iter().map(String::as_str).collect();
    assert_eq!(models.len(), 221, "the frozen table has 221 data rows");
    assert_eq!(unique.len(), 221, "exact catalog IDs must be unique");
    assert!(unique.contains("team-agent/qwen3.8-27b"));
    assert!(unique.iter().any(|id| id.contains('@')));
    assert!(unique.iter().any(|id| id.ends_with(":fast")));
    assert!(unique.iter().any(|id| id.ends_with(":slow")));

    for exact in &models {
        let selected = select_exact_pi_model(&models, exact)
            .unwrap_or_else(|error| panic!("exact catalog member {exact} must select: {error}"));
        assert_eq!(selected, *exact, "exact catalog member must round-trip");
    }

    for invalid in [
        "",
        "foo/bar",
        "cursor/",
        "team-agent/*",
        "gpt-5.4-mini",
        "grok-4.5",
        "grok-4.6",
        "qwen3.8-27b",
        "sonnet:high",
    ] {
        assert!(
            select_exact_pi_model(&models, invalid).is_err(),
            "invalid, wildcard, or ambiguous model must refuse: {invalid:?}"
        );
    }
}

#[test]
fn pi_exact_catalog_id_is_forwarded_and_started_identity_must_match() {
    let models = parse_pi_list_models_table(CATALOG).expect("frozen unfiltered table must parse");
    assert_eq!(models.len(), 221);

    for exact in &models {
        let selected = select_exact_pi_model(&models, exact).expect("exact member");
        let argv = command_for_model(&selected);
        assert!(
            argv_has_pair(&argv, "--model", exact),
            "catalog ID must be forwarded byte-for-byte: {exact}; argv={argv:?}"
        );
        assert_eq!(
            argv.iter().filter(|arg| arg.as_str() == "--model").count(),
            1,
            "model selector must occur exactly once: {argv:?}"
        );
    }

    assert!(verify_started_pi_model("team-agent/qwen3.8-27b", "team-agent/qwen3.8-27b").is_ok());
    assert!(
        verify_started_pi_model("team-agent/qwen3.8-27b", "xai/grok-4.6").is_err(),
        "started identity mismatch must refuse rather than trust pane chrome"
    );
    assert!(
        verify_started_pi_model("team-agent/qwen3.8-27b", "").is_err(),
        "missing objective started-model evidence must not pass"
    );
}
