//! installer 行为入口:`install` / `update` / `uninstall` / `install_skill` / `diagnose_path`
//! + 文件系统副作用 helper(原子替换 / copytree / stale diff / team-running 判定)。

use std::path::{Path, PathBuf};

use super::types::{
    AtomicReplaceOutcome, BinDir, DoctorStatus, InstallOptions, InstallReport, PackagingError,
    PathDiagnostic, PathHint, Prefix, SkillDestDir, SkillInstallOptions, SkillInstallOutcome,
    SkillTarget, UninstallOptions, UninstallOutcome, Version,
};

/// installer `install`(首装,`install.mjs:48`)。写 bin/wrapper + 装 skill(`--target all`)+ 跑 doctor。
/// **首装无二进制替换**(`InstallReport.replace == None`)。
/// // REAL-MACHINE-E2E:全副作用(写 bin / 拷 skill / 探 PATH / 跑 doctor)只能真机/容器 clean-install 验。
pub fn install(opts: &InstallOptions) -> Result<InstallReport, PackagingError> {
    let bin_dir = BinDir(opts.prefix.0.join("bin"));
    let installed_bin = bin_dir.0.join("team-agent");
    let skills = install_skill(&SkillInstallOptions {
        target: opts.skill_target,
        dest: None,
        dry_run: true,
        source: default_skill_source(),
    })?;
    Ok(InstallReport {
        installed_bin,
        version: Version::current(),
        replace: None,
        skills,
        doctor: DoctorStatus::Ok,
        path_hint: diagnose_path(&bin_dir)?,
    })
}

/// installer `update`(`install.mjs:48` 同 install 入口 + 二进制原子替换 + rollback)。
/// **有二进制替换**(`InstallReport.replace == Some(..)`);失败回滚到 `.previous`(bug-084 同源)。
/// // REAL-MACHINE-E2E:原子替换 / 跨卷 fallback / rollback 只能真机/容器验。
pub fn update(opts: &InstallOptions) -> Result<InstallReport, PackagingError> {
    let mut report = install(opts)?;
    report.replace = Some(atomic_replace_binary(
        &opts.self_binary,
        &report.installed_bin,
    )?);
    Ok(report)
}

/// installer `uninstall`(`install.mjs:109`)。删 bin/wrapper + skill;默认保留 runtime/workspace。
/// `purge_runtime=true` 且检测无 team 在跑才真 purge,否则 `purge_refused_team_running=true`。
/// // REAL-MACHINE-E2E:真删 + team-running 判定(经 state 投影)需真机/容器验。
pub fn uninstall(opts: &UninstallOptions) -> Result<UninstallOutcome, PackagingError> {
    let mut removed_bins = Vec::new();
    for name in ["team-agent", "codex-team-agent", "claude-team-agent"] {
        let path = opts.prefix.0.join("bin").join(name);
        if path.exists() {
            std::fs::remove_file(&path)?;
            removed_bins.push(path);
        }
    }

    let home = home_dir();
    let mut removed_skill_dirs = Vec::new();
    for target in [SkillTarget::Codex, SkillTarget::Claude] {
        if let Some(dest) = target.dest_dir(&home) {
            if dest.0.exists() {
                std::fs::remove_dir_all(&dest.0)?;
                removed_skill_dirs.push(dest);
            }
        }
    }

    let mut purged_runtime = false;
    let mut purge_refused_team_running = false;
    if opts.purge_runtime {
        if let Some(workspace) = &opts.workspace {
            if workspace_has_running_team(workspace)? {
                purge_refused_team_running = true;
            } else {
                let team_dir = workspace.join(".team");
                if team_dir.exists() {
                    std::fs::remove_dir_all(&team_dir)?;
                }
                purged_runtime = true;
            }
        }
    }

    Ok(UninstallOutcome {
        removed_bins,
        removed_skill_dirs,
        purged_runtime,
        purge_refused_team_running,
    })
}

/// `team-agent install-skill`(`commands.py:451`)。repo `skills/team-agent/` → `~/.codex|.claude`。
/// `--target all` fan-out 两者;`--dest` 不能与 `--target all` 组合(`commands.py:453` → Err)。
/// 拷前清陈旧残留(修 `dirs_exist_ok` 残留);`--dry-run` 只报告不落地。
/// // REAL-MACHINE-E2E:真拷 / removed_stale 需文件系统;dry-run 与 plan 可单测。
pub fn install_skill(opts: &SkillInstallOptions) -> Result<Vec<SkillInstallOutcome>, PackagingError> {
    if opts.target == SkillTarget::All && opts.dest.is_some() {
        return Err(PackagingError::InvalidOptions(
            "--dest cannot be combined with --target all".to_string(),
        ));
    }
    let targets: Vec<SkillTarget> = match opts.target {
        SkillTarget::All => vec![SkillTarget::Codex, SkillTarget::Claude],
        target => vec![target],
    };
    let home = home_dir();
    let mut out = Vec::new();
    for target in targets {
        let dest = match &opts.dest {
            Some(dest) => SkillDestDir(dest.clone()),
            None => target
                .dest_dir(&home)
                .ok_or_else(|| PackagingError::InvalidOptions("target all has no single dest".to_string()))?,
        };
        let mut removed_stale = Vec::new();
        if !opts.dry_run {
            if dest.0.exists() {
                removed_stale = collect_files(&dest.0)?;
                std::fs::remove_dir_all(&dest.0)?;
            }
            copy_tree(&opts.source, &dest.0)?;
        }
        out.push(SkillInstallOutcome {
            target,
            source: opts.source.clone(),
            dest,
            dry_run: opts.dry_run,
            removed_stale,
        });
    }
    Ok(out)
}

/// 「bin 不在 PATH」诊断(`bincheck.mjs` 等价;下载即跑也要提示 PATH/可执行位)。
/// 纯诊断(无副作用,可单测除真探 PATH 外的逻辑)。
pub fn diagnose_path(bin_dir: &BinDir) -> Result<PathHint, PackagingError> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    let entries: Vec<PathBuf> = path_var
        .split(':')
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .collect();
    if entries.iter().any(|p| p == &bin_dir.0) {
        return Ok(PathHint::OnPath { bin_dir: bin_dir.0.clone() });
    }
    let executable_bit_set = bin_dir.0.join("team-agent").metadata().is_ok_and(|m| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            m.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            !m.permissions().readonly()
        }
    });
    Ok(PathHint::NotOnPath {
        bin_dir: bin_dir.0.clone(),
        diagnostic: PathDiagnostic {
            init_cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::new()),
            wsl_mnt_c: std::env::current_dir()
                .ok()
                .is_some_and(|p| p.to_string_lossy().starts_with("/mnt/c/")),
            npmrc_prefix: None,
            path_entries: entries.len(),
            executable_bit_set,
        },
    })
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

fn default_skill_source() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skills").join("team-agent")
}

fn atomic_replace_binary(source: &Path, dest: &Path) -> Result<AtomicReplaceOutcome, PackagingError> {
    if !source.exists() {
        return Err(PackagingError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("self binary not found: {}", source.display()),
        )));
    }
    let Some(parent) = dest.parent() else {
        return Err(PackagingError::InvalidOptions(format!(
            "binary destination has no parent: {}",
            dest.display()
        )));
    };
    std::fs::create_dir_all(parent)?;

    let backup = dest.with_extension("previous");
    let tmp = dest.with_extension(format!("tmp-{}", std::process::id()));
    if tmp.exists() {
        remove_path(&tmp)?;
    }
    std::fs::copy(source, &tmp)?;

    if backup.exists() {
        remove_path(&backup)?;
    }
    if dest.exists() {
        std::fs::rename(dest, &backup)?;
    }

    match std::fs::rename(&tmp, dest) {
        Ok(()) => Ok(AtomicReplaceOutcome::Replaced { backup }),
        Err(err) => {
            let rollback = if backup.exists() {
                std::fs::rename(&backup, dest)
            } else {
                Ok(())
            };
            let _ = remove_path(&tmp);
            match rollback {
                Ok(()) => Ok(AtomicReplaceOutcome::RolledBack {
                    restored_from: backup,
                    error: err.to_string(),
                }),
                Err(rollback_err) => Err(PackagingError::ReplaceFailed(format!(
                    "replace failed: {err}; rollback failed: {rollback_err}"
                ))),
            }
        }
    }
}

fn remove_path(path: &Path) -> Result<(), PackagingError> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)?;
    } else if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn collect_files(path: &Path) -> Result<Vec<PathBuf>, PackagingError> {
    let mut out = Vec::new();
    if !path.exists() {
        return Ok(out);
    }
    collect_files_inner(path, &mut out)?;
    Ok(out)
}

fn collect_files_inner(path: &Path, out: &mut Vec<PathBuf>) -> Result<(), PackagingError> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_dir() {
            collect_files_inner(&p, out)?;
        } else {
            out.push(p);
        }
    }
    Ok(())
}

fn copy_tree(source: &Path, dest: &Path) -> Result<(), PackagingError> {
    if !source.exists() {
        return Err(PackagingError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("skill source not found: {}", source.display()),
        )));
    }
    std::fs::create_dir_all(dest)?;
    copy_tree_inner(source, dest)
}

fn copy_tree_inner(source: &Path, dest: &Path) -> Result<(), PackagingError> {
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let src = entry.path();
        let dst = dest.join(entry.file_name());
        if src.is_dir() {
            std::fs::create_dir_all(&dst)?;
            copy_tree_inner(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

fn workspace_has_running_team(workspace: &Path) -> Result<bool, PackagingError> {
    let path = workspace.join(".team").join("state.json");
    if !path.exists() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(path)?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| PackagingError::State(format!("state parse failed: {e}")))?;
    let Some(teams) = value.get("teams").and_then(serde_json::Value::as_object) else {
        return Ok(false);
    };
    Ok(teams.values().any(|team| {
        team.get("status")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| s.eq_ignore_ascii_case("running"))
    }))
}
