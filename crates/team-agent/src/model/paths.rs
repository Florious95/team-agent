//! `.team/` 布局路径(移植 `paths.py` 的 workspace/team-dir 部分)。
//!
//! Python 端 `runtime_dir`/`logs_dir`/`artifacts_dir`/`messages_dir` 把 `.team/<sub>`
//! 拼在 workspace 下;`team_workspace` 反推:先 `Path.resolve()`(非严格,规整 `.`/`..`
//! 并相对 cwd 绝对化,**不要求路径存在**),再看父目录名是否 `.team` —— 是则 workspace
//! = 祖父目录(`<ws>/.team/<sub>` → `<ws>`),否则 = 父目录。
//!
//! §10:无 panic;`team_workspace` 取 cwd 失败(进程无 cwd 权限)返 `Err`,不 unwrap。
//! 行为对拍:golden 路径串由真相源 `team-agent-public` Python 跑出,硬编码进测试断言。

use std::path::{Component, Path, PathBuf};

use crate::model::errors::ModelError;

/// `<workspace>/.team/runtime`。
pub fn runtime_dir(workspace: &Path) -> PathBuf {
    team_subdir(workspace, "runtime")
}

/// E5:**spec.yaml 的唯一落地点** = `<workspace>/.team/runtime/<team_key>/team.spec.yaml`。
/// 用户终裁:角色定义(TEAM.md/agents/*.md)=第一真相源;spec=中间产物,**绝不落用户目录**。
/// 所有 spec 读写经此单点,杜绝 `<user_dir>/team.spec.yaml`。
pub fn runtime_spec_path(workspace: &Path, team_key: &str) -> PathBuf {
    runtime_dir(workspace).join(team_key).join("team.spec.yaml")
}

/// `<workspace>/.team/logs`。
pub fn logs_dir(workspace: &Path) -> PathBuf {
    team_subdir(workspace, "logs")
}

/// `<workspace>/.team/artifacts`。
pub fn artifacts_dir(workspace: &Path) -> PathBuf {
    team_subdir(workspace, "artifacts")
}

/// `<workspace>/.team/messages`。
pub fn messages_dir(workspace: &Path) -> PathBuf {
    team_subdir(workspace, "messages")
}

fn team_subdir(workspace: &Path, sub: &str) -> PathBuf {
    let mut p = workspace.to_path_buf();
    p.push(".team");
    p.push(sub);
    p
}

/// 从 team 目录反推 workspace(Python `team_workspace`)。
///
/// 等价 Python `team_dir.resolve()` 后:`parent.name == ".team"` → `parent.parent`,
/// 否则 `parent`。`resolve()` 非严格——这里用 [`resolve_nonstrict`] 复刻:相对路径相对
/// cwd 绝对化、规整 `.`/`..`,不访问文件系统(不解析符号链接,与 Python 在路径不存在
/// 时的行为一致)。无父目录(如根 `/`)时退回路径本身,与 `Path.parent` 在根上返回自身一致。
pub fn team_workspace(team_dir: &Path) -> Result<PathBuf, ModelError> {
    let resolved = resolve_nonstrict(team_dir)?;
    let parent = resolved.parent().unwrap_or(&resolved);
    let parent_name = parent.file_name().and_then(|n| n.to_str());
    if parent_name == Some(".team") {
        let grand = parent.parent().unwrap_or(parent);
        Ok(grand.to_path_buf())
    } else {
        Ok(parent.to_path_buf())
    }
}

/// Idempotent run-workspace resolver for runtime operations.
///
/// `team_workspace()` intentionally mirrors Python's one-way "team dir -> parent" helper and is not
/// idempotent. Runtime handlers need a stable workspace for both state and the per-team tmux socket:
/// passing an already-running workspace must keep it, while passing that workspace's team dir must map
/// back to the same run workspace.
pub fn canonical_run_workspace(input: &Path) -> Result<PathBuf, ModelError> {
    let resolved = resolve_nonstrict(input)?;
    if resolved.file_name().and_then(|n| n.to_str()) == Some(".team") {
        return Ok(resolved
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| resolved.clone()));
    }
    let parent = resolved.parent().unwrap_or(&resolved);
    if parent.file_name().and_then(|n| n.to_str()) == Some(".team") {
        return Ok(parent
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| parent.to_path_buf()));
    }
    if resolved.join(".team").exists() {
        return Ok(resolved);
    }
    if parent.join(".team").exists() {
        return Ok(parent.to_path_buf());
    }
    Ok(resolved)
}

/// 复刻 Python `Path.resolve()` 的**非严格**语义(不碰文件系统):
/// 相对路径以当前工作目录绝对化,再词法消解 `.` 与 `..`。
fn resolve_nonstrict(p: &Path) -> Result<PathBuf, ModelError> {
    let base = if p.is_absolute() {
        p.to_path_buf()
    } else {
        let cwd = std::env::current_dir()
            .map_err(|e| ModelError::Runtime(format!("cannot resolve cwd: {e}")))?;
        cwd.join(p)
    };

    let mut out = PathBuf::new();
    for comp in base.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => out.push(comp),
            Component::CurDir => {}
            Component::ParentDir => {
                // 只弹普通组件;不越过 root/prefix(与 Python resolve 在根处停住一致)。
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    // ---- §4.2 行为对拍:golden 由 team-agent-public Python 跑出 ----

    #[test]
    fn subdirs_match_python_golden() {
        let ws = Path::new("/ws/proj");
        assert_eq!(runtime_dir(ws), PathBuf::from("/ws/proj/.team/runtime"));
        assert_eq!(logs_dir(ws), PathBuf::from("/ws/proj/.team/logs"));
        assert_eq!(artifacts_dir(ws), PathBuf::from("/ws/proj/.team/artifacts"));
        assert_eq!(messages_dir(ws), PathBuf::from("/ws/proj/.team/messages"));

        let ws2 = Path::new("/Users/alauda/x");
        assert_eq!(runtime_dir(ws2), PathBuf::from("/Users/alauda/x/.team/runtime"));
    }

    #[test]
    fn subdirs_relative_workspace_join() {
        // Python: runtime_dir(Path("relative/path")) -> relative/path/.team/runtime
        // (无 resolve,纯拼接)。
        let ws = Path::new("relative/path");
        assert_eq!(runtime_dir(ws), PathBuf::from("relative/path/.team/runtime"));
    }

    #[test]
    fn team_workspace_parent_is_dot_team_returns_grandparent() {
        // golden: team_workspace(/ws/proj/.team/runtime) -> /ws/proj
        assert_eq!(
            team_workspace(Path::new("/ws/proj/.team/runtime")).unwrap(),
            PathBuf::from("/ws/proj")
        );
        assert_eq!(
            team_workspace(Path::new("/ws/proj/.team/messages")).unwrap(),
            PathBuf::from("/ws/proj")
        );
        // golden: /a/.team/runtime -> /a ; /a/b/c/.team/logs -> /a/b/c
        assert_eq!(team_workspace(Path::new("/a/.team/runtime")).unwrap(), PathBuf::from("/a"));
        assert_eq!(
            team_workspace(Path::new("/a/b/c/.team/logs")).unwrap(),
            PathBuf::from("/a/b/c")
        );
    }

    #[test]
    fn team_workspace_parent_not_dot_team_returns_parent() {
        // golden: /ws/proj/teamdir -> /ws/proj ; /ws/proj/.team -> /ws/proj ; /x/y -> /x
        assert_eq!(
            team_workspace(Path::new("/ws/proj/teamdir")).unwrap(),
            PathBuf::from("/ws/proj")
        );
        assert_eq!(team_workspace(Path::new("/ws/proj/.team")).unwrap(), PathBuf::from("/ws/proj"));
        assert_eq!(team_workspace(Path::new("/x/y")).unwrap(), PathBuf::from("/x"));
    }

    #[test]
    fn team_workspace_at_root() {
        // golden: /.team/runtime -> / (parent ".team", grandparent "/")
        assert_eq!(team_workspace(Path::new("/.team/runtime")).unwrap(), PathBuf::from("/"));
    }

    #[test]
    fn team_workspace_normalizes_dot_and_dotdot_like_python_resolve() {
        // Python: Path('/ws/proj/.team/../.team/runtime').resolve() -> /ws/proj/.team/runtime
        //         then team_workspace -> /ws/proj
        assert_eq!(
            team_workspace(Path::new("/ws/proj/.team/../.team/runtime")).unwrap(),
            PathBuf::from("/ws/proj")
        );
        assert_eq!(
            team_workspace(Path::new("/ws/proj/./.team/runtime")).unwrap(),
            PathBuf::from("/ws/proj")
        );
    }

    #[test]
    fn team_workspace_relative_resolved_against_cwd() {
        // Python: Path('foo/.team/runtime').resolve() -> <cwd>/foo/.team/runtime
        //         team_workspace -> <cwd>/foo
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            team_workspace(Path::new("foo/.team/runtime")).unwrap(),
            cwd.join("foo")
        );
    }

    #[test]
    fn canonical_run_workspace_is_idempotent_for_run_workspace_and_team_dir() {
        let base = std::env::temp_dir().join(format!("ta_rs_paths_runws_{}", std::process::id()));
        let run_ws = base.join("project");
        let team_dir = run_ws.join("agents");
        std::fs::create_dir_all(runtime_dir(&run_ws)).unwrap();
        std::fs::create_dir_all(&team_dir).unwrap();

        assert_eq!(canonical_run_workspace(&run_ws).unwrap(), run_ws);
        assert_eq!(canonical_run_workspace(&team_dir).unwrap(), base.join("project"));
    }

    #[test]
    fn canonical_run_workspace_maps_team_subdirs_to_workspace() {
        assert_eq!(
            canonical_run_workspace(Path::new("/ws/proj/.team/runtime")).unwrap(),
            PathBuf::from("/ws/proj")
        );
        assert_eq!(
            canonical_run_workspace(Path::new("/ws/proj/.team")).unwrap(),
            PathBuf::from("/ws/proj")
        );
    }
}
