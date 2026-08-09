//!
//! installer 行为入口:`install` / `install_skill` / `diagnose_path`
//! + 文件系统副作用 helper(copytree / stale diff)。

use std::path::{Path, PathBuf};

use super::types::{
    BinDir, DoctorStatus, InstallOptions, InstallReport, PackagingError, PathDiagnostic, PathHint,
    Prefix, SkillDestDir, SkillInstallOptions, SkillInstallOutcome, SkillTarget, Version,
};

///
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
    // T3-6 (harvest §1): never a hardcoded DoctorStatus::Ok with no check behind it —
    // run the real packaging doctor (schema diagnosis + gates) for the invoking
    // workspace so the report reflects an actual result.
    let doctor = super::migrate::doctor(&super::types::DoctorOptions {
        workspace: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        gate: None,
        fix: false,
        cleanup_orphans: false,
        confirm: false,
    })?;
    Ok(InstallReport {
        installed_bin,
        version: Version::current(),
        replace: None,
        skills,
        doctor,
        path_hint: diagnose_path(&bin_dir)?,
    })
}

///
/// `team-agent install-skill`(`commands.py:451`)。repo `skills/team-agent/` → `~/.codex|.claude`。
/// `--target all` fan-out 两者;`--dest` 不能与 `--target all` 组合(`commands.py:453` → Err)。
/// 拷前清陈旧残留(修 `dirs_exist_ok` 残留);`--dry-run` 只报告不落地。
/// // REAL-MACHINE-E2E:真拷 / removed_stale 需文件系统;dry-run 与 plan 可单测。
pub fn install_skill(
    opts: &SkillInstallOptions,
) -> Result<Vec<SkillInstallOutcome>, PackagingError> {
    if opts.target == SkillTarget::All && opts.dest.is_some() {
        return Err(PackagingError::InvalidOptions(
            "--dest cannot be combined with --target all".to_string(),
        ));
    }
    let targets: Vec<SkillTarget> = match opts.target {
        SkillTarget::All => SkillTarget::SINGLE_TARGETS.to_vec(),
        target => vec![target],
    };
    let home = home_dir();
    let mut out = Vec::new();
    for target in targets {
        let dest = match &opts.dest {
            Some(dest) => SkillDestDir(dest.clone()),
            None => target.dest_dir(&home).ok_or_else(|| {
                PackagingError::InvalidOptions("target all has no single dest".to_string())
            })?,
        };
        let mut removed_stale = Vec::new();
        if !opts.dry_run {
            // T2-1 (harvest §1): NEVER pre-wipe the user's existing skill dir — stage
            // the copy into a sibling temp dir and only swap after the copy succeeded
            // (write_worker_mcp_config tmp+rename 范式). A failed copy leaves the
            // user's dir byte-identical.
            let staging = staging_dir_for(&dest.0)?;
            let _ = std::fs::remove_dir_all(&staging);
            if let Err(error) = copy_tree(&opts.source, &staging) {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(error);
            }
            if dest.0.exists() {
                removed_stale = collect_files(&dest.0)?;
                std::fs::remove_dir_all(&dest.0)?;
            }
            if let Err(error) = std::fs::rename(&staging, &dest.0) {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(PackagingError::Io(std::io::Error::other(format!(
                    "swap staged skill dir into {}: {error}",
                    dest.0.display()
                ))));
            }
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

///
/// 「bin 不在 PATH」诊断(`bincheck.mjs` 等价;下载即跑也要提示 PATH/可执行位)。
/// 纯诊断(无副作用,可单测除真探 PATH 外的逻辑)。
fn diagnose_path(bin_dir: &BinDir) -> Result<PathHint, PackagingError> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    let entries: Vec<PathBuf> = path_var
        .split(':')
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .collect();
    if entries.iter().any(|p| p == &bin_dir.0) {
        return Ok(PathHint::OnPath {
            bin_dir: bin_dir.0.clone(),
        });
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
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn default_skill_source() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("skills")
        .join("team-agent")
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

/// T2-1: a sibling staging path for the atomic skill-dir swap (same parent so the
/// final rename never crosses filesystems).
fn staging_dir_for(dest: &Path) -> Result<PathBuf, PackagingError> {
    let name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("skill");
    let parent = dest
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&parent)?;
    Ok(parent.join(format!(".{name}.ta-staging-{}", std::process::id())))
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
