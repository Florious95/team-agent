//! Pure H1 tests using the exact native catalog capture, never invoking Kiro.
#![cfg(unix)]
mod support;
use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};
use team_agent_contract::contract::{hooks::*, plan::*, session::CwdIdentity, types::*};
use team_agent_contract::kiro::{
    native::CHAT_CATALOG_SOURCE, KiroAdapter, McpStdio, KIRO_DESCRIPTOR,
};

const NATIVE_CATALOG: &[u8] = include_bytes!("fixtures/kiro-2.28.0-models.json");
struct RawHost {
    reply: Option<Result<ReadOutput, ReadFailure>>,
    calls: usize,
}
impl BoundedReadHost for RawHost {
    fn read_catalog(&mut self, _: &CatalogRequest) -> Result<ReadOutput, ReadFailure> {
        self.calls += 1;
        self.reply.take().expect("no catalog retry or fallback")
    }
}
fn host(bytes: &[u8]) -> RawHost {
    RawHost {
        reply: Some(Ok(ReadOutput {
            stdout: bytes.to_vec(),
            elapsed: Duration::from_millis(1),
            exit_code: 0,
        })),
        calls: 0,
    }
}
fn request() -> CatalogRequest {
    CatalogRequest {
        provider: ProviderId::new("kiro").unwrap(),
        native: NativeIdentity {
            version: "2.28.0".into(),
            harness: "v3".into(),
            ui: "tui".into(),
            platform: Platform::MacOs,
            executable_sha256: Digest([1; 32]),
        },
        executable: "/fixture/kiro-cli-chat".into(),
        cwd: CwdIdentity {
            path: "/fixture/work".into(),
            identity: Digest([2; 32]),
        },
        source: CHAT_CATALOG_SOURCE,
        bounds: ReadBounds {
            deadline: Duration::from_secs(10),
            max_output_bytes: 65536,
        },
    }
}
fn adapter() -> KiroAdapter {
    KiroAdapter::new(McpStdio {
        executable: "/fixture/candidate".into(),
        arguments: vec![],
        environment: BTreeMap::new(),
    })
    .unwrap()
}
fn one() -> Value {
    json!({"models":[{"model_id":"MiXeD/model", "model_name":"Friendly display", "context_window_tokens":200000}], "default_model":"MiXeD/model"})
}
#[test]
fn observed_native_catalog_preserves_all_nine_ids_without_luna_or_effort_inference() {
    let request = request();
    let mut host = host(NATIVE_CATALOG);
    let observed = adapter().discover(&request, &mut host).unwrap();
    assert_eq!(host.calls, 1);
    assert_eq!(observed.provider, request.provider);
    assert_eq!(observed.native, request.native);
    assert_eq!(observed.schema, CHAT_CATALOG_SOURCE.schema);
    assert_eq!(
        observed
            .models
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        [
            "auto",
            "claude-sonnet-4.5",
            "claude-sonnet-4",
            "claude-haiku-4.5",
            "deepseek-3.2",
            "minimax-m2.5",
            "minimax-m2.1",
            "glm-5",
            "qwen3-coder-next"
        ]
    );
    assert!(observed.models.iter().all(|m| !m.id.contains("luna")));
    assert!(observed.models.iter().all(|m| matches!(&m.efforts, Support::Unverified(reason) if reason.code == "kiro-catalog-effort-not-reported")));
    // Deprecated descriptions and prices are not permission for the adapter to
    // silently remove IDs, alias names, choose auto, or infer reasoning levels.
}
#[test]
fn exact_selection_does_not_alias_display_name_case_default_or_unknown_effort_metadata() {
    let mut bytes = one();
    bytes["models"][0]["efforts"] = json!(["high"]); // unobserved field, not admitted
    let kiro = adapter();
    let catalog = kiro
        .discover(&request(), &mut host(&serde_json::to_vec(&bytes).unwrap()))
        .unwrap();
    let mut descriptor = support::descriptor();
    descriptor.identity.id = "kiro";
    descriptor.model = KIRO_DESCRIPTOR.model.clone();
    descriptor.effort = KIRO_DESCRIPTOR.effort.clone();
    let mut launch = support::request(std::path::Path::new("/fixture/work"), "worker");
    launch.provider = "kiro".into();
    launch.mode = LaunchMode::LaunchOnly;
    launch.native = request().native;
    let hooks = ProviderHooks {
        catalog: HookBinding::Bound(&kiro),
        ..support::hooks()
    };
    for model in [
        None,
        Some("Friendly display"),
        Some("mixed/model"),
        Some("luna"),
    ] {
        launch.model = model.map(str::to_owned);
        assert!(resolve_launch(&descriptor, &hooks, &launch, Some(&catalog)).is_err());
    }
    launch.model = Some("MiXeD/model".into());
    assert_eq!(
        resolve_launch(&descriptor, &hooks, &launch, Some(&catalog))
            .unwrap()
            .model(),
        Some("MiXeD/model")
    );
    launch.role_effort = Some("high".into());
    assert!(matches!(
        resolve_launch(&descriptor, &hooks, &launch, Some(&catalog))
            .unwrap()
            .effort(),
        EffortResolution::Pass(team_agent_contract::contract::descriptor::Effort::High)
    ));
    // Native flag pass-through is distinct from a per-model capability claim.
    assert!(matches!(catalog.models[0].efforts, Support::Unverified(_)));
}
#[test]
fn malformed_records_or_defaults_reject_the_whole_catalog_without_partial_results() {
    let mut cases = vec![
        json!([]),
        json!({}),
        json!({"models":[],"default_model":"auto"}),
    ];
    for (field, value) in [
        ("model_id", json!(" ")),
        ("model_id", json!("bad\u{0}id")),
        ("model_id", json!(17)),
        ("model_name", json!("")),
        ("context_window_tokens", json!(0)),
        ("context_window_tokens", json!(-1)),
        ("context_window_tokens", json!("200000")),
    ] {
        let mut value_json = one();
        value_json["models"][0][field] = value;
        cases.push(value_json);
    }
    for field in ["model_id", "model_name", "context_window_tokens"] {
        let mut value = one();
        value["models"][0].as_object_mut().unwrap().remove(field);
        cases.push(value);
    }
    let mut value = one();
    value["default_model"] = json!("not-listed");
    cases.push(value);
    let mut value = one();
    value.as_object_mut().unwrap().remove("default_model");
    cases.push(value);
    let mut value = one();
    value["models"]
        .as_array_mut()
        .unwrap()
        .push(json!({"model_id":"late-bad"}));
    cases.push(value);
    for value in cases {
        let mut host = host(&serde_json::to_vec(&value).unwrap());
        assert!(
            adapter().discover(&request(), &mut host).is_err(),
            "fixture: {value}"
        );
        assert_eq!(host.calls, 1);
    }
}
#[test]
fn duplicate_ids_duplicate_authority_keys_and_trailing_json_are_not_last_wins() {
    let mut duplicate = one();
    let model = duplicate["models"][0].clone();
    duplicate["models"].as_array_mut().unwrap().push(model);
    assert!(matches!(
        adapter().discover(
            &request(),
            &mut host(&serde_json::to_vec(&duplicate).unwrap())
        ),
        Err(ReadFailure::Error(ContractError::AmbiguousModel))
    ));
    // Each document would be valid if parsed through a last-wins Value first.
    let valid = serde_json::to_string(&one()).unwrap();
    for bytes in [
        valid.replacen("\"models\":", "\"models\":[],\"models\":", 1),
        valid.replacen(
            "\"default_model\":",
            "\"default_model\":\"not-listed\",\"default_model\":",
            1,
        ),
        valid.replacen("\"model_id\":", "\"model_id\":\"wrong\",\"model_id\":", 1),
        valid.replacen(
            "\"model_name\":",
            "\"model_name\":\"wrong\",\"model_name\":",
            1,
        ),
        valid.replacen(
            "\"context_window_tokens\":",
            "\"context_window_tokens\":0,\"context_window_tokens\":",
            1,
        ),
    ] {
        assert!(adapter()
            .discover(&request(), &mut host(bytes.as_bytes()))
            .is_err());
    }
    let mut trailing = NATIVE_CATALOG.to_vec();
    trailing.extend_from_slice(b" {}");
    assert!(adapter()
        .discover(&request(), &mut host(&trailing))
        .is_err());
}
#[test]
fn invalid_read_grants_are_rejected_before_the_host_is_invoked() {
    let base = request();
    let mut cases = vec![];
    let mut wrong = base.clone();
    wrong.provider = ProviderId::new("other").unwrap();
    cases.push(wrong);
    let mut wrong = base.clone();
    wrong.native.version = "2.29.0".into();
    cases.push(wrong);
    let mut wrong = base.clone();
    wrong.executable = "/fixture/kiro-cli".into();
    cases.push(wrong);
    let mut wrong = base.clone();
    wrong.cwd.path = "relative".into();
    cases.push(wrong);
    let mut wrong = base.clone();
    wrong.source.arguments = &["login"];
    cases.push(wrong);
    let mut wrong = base.clone();
    wrong.source.schema = "guessed-schema";
    cases.push(wrong);
    let mut wrong = base.clone();
    wrong.bounds.deadline = Duration::ZERO;
    cases.push(wrong);
    let mut wrong = base;
    wrong.bounds.max_output_bytes = 0;
    cases.push(wrong);
    for wrong in cases {
        let mut host = host(NATIVE_CATALOG);
        assert!(adapter().discover(&wrong, &mut host).is_err());
        assert_eq!(host.calls, 0);
    }
}
#[test]
fn auth_failure_exit_timeout_and_oversize_cannot_be_promoted_to_observed_models() {
    let request = request();
    let mut auth = RawHost {
        reply: Some(Err(ReadFailure::AuthRequired)),
        calls: 0,
    };
    assert!(matches!(
        adapter().discover(&request, &mut auth),
        Err(ReadFailure::AuthRequired)
    ));
    assert_eq!(auth.calls, 1);
    let mut exited = host(NATIVE_CATALOG);
    exited.reply.as_mut().unwrap().as_mut().unwrap().exit_code = 1;
    assert!(matches!(
        adapter().discover(&request, &mut exited),
        Err(ReadFailure::Exit { code: 1 })
    ));
    let mut late = host(NATIVE_CATALOG);
    late.reply.as_mut().unwrap().as_mut().unwrap().elapsed =
        request.bounds.deadline + Duration::from_millis(1);
    assert!(matches!(
        adapter().discover(&request, &mut late),
        Err(ReadFailure::TimedOut { .. })
    ));
    let mut oversized = host(&vec![b' '; request.bounds.max_output_bytes + 1]);
    assert!(matches!(
        adapter().discover(&request, &mut oversized),
        Err(ReadFailure::OutputLimit { .. })
    ));
}
