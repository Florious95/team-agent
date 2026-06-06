use super::*;

fn compile_team_dir(tag: &str) -> std::path::PathBuf {
    let team = tmp_workspace().join(tag);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: compileteam\nobjective: Compile probe.\nprovider: fake\n---\n\nCompile team.\n",
    )
    .unwrap();
    std::fs::write(
        team.join("agents").join("worker.md"),
        "---\nname: worker\nrole: Worker\nprovider: fake\nmodel: fake\ntools:\n  - mcp_team\n---\n\nWorker role.\n",
    )
    .unwrap();
    team
}

#[test]
fn cmd_compile_json_and_human_match_golden_shape_and_writes_out() {
    let team = compile_team_dir("compile-ok");
    let out = team.parent().unwrap().join("out.yaml");
    let args = CompileArgs { team: team.clone(), out: out.clone(), json: true };

    let result = cmd_compile(&args).expect("compile");
    assert_eq!(result.exit, ExitCode::Ok);
    assert!(out.exists(), "compile must write the compiled spec to --out");
    assert!(
        std::fs::read_to_string(&out).unwrap().contains("version: 1"),
        "compiled out file must contain the spec YAML"
    );

    let json_text = emit(&result.output, true).unwrap();
    let expected_json = format!(
        "{{\n  \"agents\": [\n    \"worker\"\n  ],\n  \"ok\": true,\n  \"out\": \"{}\",\n  \"team_dir\": \"{}\"\n}}",
        out.to_string_lossy(),
        team.to_string_lossy()
    );
    assert_eq!(json_text, expected_json, "golden --json is sorted pretty JSON");

    let human_text = emit(&result.output, false).unwrap();
    let expected_human = format!(
        "ok: True\nteam_dir: {}\nout: {}\nagents: [\"worker\"]",
        team.to_string_lossy(),
        out.to_string_lossy()
    );
    assert_eq!(human_text, expected_human, "golden human output preserves cmd_compile insertion order");
}

#[test]
fn run_dispatches_compile_and_error_path_exits_error() {
    let team = compile_team_dir("compile-dispatch");
    let out = team.parent().unwrap().join("dispatch.yaml");
    let argv = vec![
        "compile".to_string(),
        "--team".to_string(),
        team.to_string_lossy().to_string(),
        "--out".to_string(),
        out.to_string_lossy().to_string(),
        "--json".to_string(),
    ];
    assert_eq!(run(&argv, team.parent().unwrap()), ExitCode::Ok);
    assert!(out.exists(), "dispatch compile must route to cmd_compile and write --out");

    let bad = tmp_workspace().join("compile-bad");
    std::fs::create_dir_all(bad.join("agents")).unwrap();
    std::fs::write(
        bad.join("TEAM.md"),
        "---\nname: badteam\nobjective: Bad.\nprovider: fake\n---\n",
    )
    .unwrap();
    std::fs::write(
        bad.join("agents").join("broken.md"),
        "---\nname: broken\nrole: Broken\nmodel: fake\n---\n",
    )
    .unwrap();
    let bad_out = bad.parent().unwrap().join("bad.yaml");
    let bad_args = CompileArgs { team: bad.clone(), out: bad_out.clone(), json: true };
    let err = cmd_compile(&bad_args).unwrap_err().to_string();
    assert!(err.contains("missing front matter field provider"), "got {err}");
    assert_eq!(
        run(
            &[
                "compile".to_string(),
                "--team".to_string(),
                bad.to_string_lossy().to_string(),
                "--out".to_string(),
                bad_out.to_string_lossy().to_string(),
                "--json".to_string(),
            ],
            bad.parent().unwrap(),
        ),
        ExitCode::Error,
        "invalid compile input must exit 1, not fall through as an unknown subcommand"
    );
}
