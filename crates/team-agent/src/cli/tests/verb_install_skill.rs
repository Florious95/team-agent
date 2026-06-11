use super::*;

// RED-1 根治:install-skill 必须作为真 CLI 子命令存在(此前 packaging::install_skill 未接进
// 任何 dispatch=双重死代码,install.mjs 走自己的 JS 拷贝逻辑漏 copilot)。
// 这里钉:① 子命令路由 ② --target all 默认 ③ --source 必需 ④ 缺 source 显式 Usage 错。

fn skill_source() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-cli-installskill-src-{}-{}",
        std::process::id(),
        CTR.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(dir.join("team-agent")).unwrap();
    std::fs::write(dir.join("team-agent").join("SKILL.md"), b"---\nname: team-agent\n---\nbody\n").unwrap();
    dir.join("team-agent")
}

#[test]
fn dispatch_routes_install_skill_dry_run_all() {
    let ws = tmp_workspace();
    let source = skill_source();
    // --dry-run 不落地(不碰真实 HOME),只验路由 + exit 0。
    let code = run(
        &[
            "install-skill".to_string(),
            "--target".to_string(),
            "all".to_string(),
            "--source".to_string(),
            source.to_string_lossy().to_string(),
            "--dry-run".to_string(),
            "--json".to_string(),
        ],
        &ws,
    );
    assert_eq!(code, ExitCode::Ok, "`install-skill --target all --source <dir> --dry-run` must route + exit 0");
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(source.parent().unwrap());
}

#[test]
fn install_skill_missing_source_is_usage_error() {
    let ws = tmp_workspace();
    // 无 --source → Usage 错(exit 2),不静默。
    let code = run(
        &[
            "install-skill".to_string(),
            "--target".to_string(),
            "all".to_string(),
            "--json".to_string(),
        ],
        &ws,
    );
    assert_eq!(code, ExitCode::Error, "install-skill without --source must error (CliError::Usage -> emit_cli_error exit 1)");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn install_skill_invalid_target_is_usage_error() {
    let ws = tmp_workspace();
    let source = skill_source();
    let code = run(
        &[
            "install-skill".to_string(),
            "--target".to_string(),
            "bogus".to_string(),
            "--source".to_string(),
            source.to_string_lossy().to_string(),
        ],
        &ws,
    );
    assert_eq!(code, ExitCode::Error, "install-skill --target bogus must error (CliError::Usage -> emit_cli_error exit 1)");
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(source.parent().unwrap());
}
