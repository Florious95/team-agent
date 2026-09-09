use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/test_support/models_timeout_fixture.rs");

    // The models tests are Unix-only because the helper relies on inherited
    // stdout surviving the parent's exit. Other targets do not need the
    // fixture and must not try to compile a Unix-specific probe.
    if env::var_os("CARGO_CFG_UNIX").is_none() {
        return;
    }

    let rustc = env::var_os("RUSTC").expect("Cargo must provide RUSTC to build the test helper");
    let target = env::var("TARGET").expect("Cargo must provide TARGET to build the test helper");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must provide OUT_DIR"));
    let helper = out_dir.join("team-agent-models-timeout-fixture");
    let status = Command::new(rustc)
        .args(["--crate-name", "team_agent_models_timeout_fixture"])
        .args(["--edition", "2021", "--crate-type", "bin", "--target"])
        .arg(target)
        .arg("src/test_support/models_timeout_fixture.rs")
        .arg("-o")
        .arg(&helper)
        .status()
        .expect("spawn rustc for the prebuilt models timeout helper");
    assert!(status.success(), "prebuilt models timeout helper failed to compile");
    println!(
        "cargo:rustc-env=TEAM_AGENT_MODELS_TIMEOUT_FIXTURE={}",
        helper.display()
    );
}
