//! Active team workspace selector.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::model::paths::{canonical_run_workspace, runtime_spec_path, team_workspace};
use crate::state::persist::{load_runtime_state, runtime_state_path};
use crate::state::projection::{select_runtime_state, team_state_key};
use crate::state::StateError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorMode {
    RuntimeOnly,
    RequireSpec,
}

#[derive(Debug, Clone)]
pub struct SelectedTeam {
    pub run_workspace: PathBuf,
    pub team_key: String,
    pub state: Value,
    /// E5 §3 解耦:**角色定义目录**(用户目录,含 TEAM.md+agents/*.md+profiles)。
    /// 给 compile_team / 找角色定义 / profiles。**永远是用户目录**,不是 spec 落点。
    /// 来源 = state.team_dir;缺则回落 run_workspace(自含 team-dir 布局)。
    pub team_dir: PathBuf,
    /// spec yaml 所在目录(demote 后 = .team/runtime/<team_key>/)。读写 spec yaml 用。
    pub spec_workspace: Option<PathBuf>,
    /// spec yaml 路径(= runtime_spec_path(run_ws, team_key))。读写 spec yaml 用。
    pub spec_path: Option<PathBuf>,
}

pub fn resolve_active_team(
    input: &Path,
    team: Option<&str>,
    mode: SelectorMode,
) -> Result<SelectedTeam, StateError> {
    let explicit_spec = input.join("team.spec.yaml");
    let (run_workspace, state) = if explicit_spec.exists() {
        let team_run = team_workspace(input).map_err(|e| StateError::TeamSelect(e.to_string()))?;
        let run = if runtime_state_path(input).exists() || !runtime_state_path(&team_run).exists() {
            input.to_path_buf()
        } else {
            team_run
        };
        let state = select_runtime_state(&run, team).or_else(|_| load_runtime_state(&run))?;
        (run, state)
    } else {
        let run = canonical_run_workspace(input)
            .map_err(|e| StateError::TeamSelect(e.to_string()))?;
        if !input.exists()
            && !runtime_state_path(&run).exists()
            && !run.join(".team").exists()
            && !run.join("team.spec.yaml").exists()
        {
            return Err(StateError::TeamSelect(format!(
                "invalid workspace: {}",
                input.display()
            )));
        }
        let state = select_runtime_state(&run, team).or_else(|_| load_runtime_state(&run))?;
        (run, state)
    };

    // E5 spec 迁移·读序 B(architect+leader 裁定):
    //   1) runtime spec 优先严格:<run_ws>/.team/runtime/<team_key>/team.spec.yaml 存在即必用。
    //   2) 缺失才**只读回落**用户目录旧 spec(过渡腿;绝不在此写/迁移——迁移+清理只属启动重建)。
    // TODO(E5 后续版本):新 team 永不写用户目录(G1),回落腿可在 legacy 清零后移除。
    let team_key = selected_team_key(&state, team);
    let runtime_spec = runtime_spec_path(&run_workspace, &team_key);
    let (spec_workspace, spec_path) = if runtime_spec.exists() {
        (
            runtime_spec.parent().map(Path::to_path_buf),
            Some(runtime_spec.clone()),
        )
    } else {
        // 回落(只读):优先 explicit input/team.spec.yaml,其次 state 推断的 spec_workspace。
        let legacy_ws = if explicit_spec.exists() {
            Some(input.to_path_buf())
        } else {
            spec_workspace_from_state(&state)
                .or_else(|| run_workspace.join("team.spec.yaml").exists().then(|| run_workspace.clone()))
        };
        let legacy_spec = legacy_ws.as_ref().map(|ws| ws.join("team.spec.yaml"));
        (legacy_ws, legacy_spec)
    };
    if matches!(mode, SelectorMode::RequireSpec) && !spec_path.as_ref().is_some_and(|path| path.exists()) {
        // 期望路径报 canonical runtime spec(重建落点),非用户目录。
        let expected = spec_path.as_ref().cloned().unwrap_or(runtime_spec);
        // E5 Bug2 N38:spec=中间产物,运行期由 restart 以角色定义重建;首装走 quick-start;
        // 加新角色用 add-agent。不再提 reconcile(已废)。
        return Err(StateError::TeamSelect(format!(
            "active team spec not found: input_workspace={} run_workspace={} team_key={} expected_spec_path={} hint=run `team-agent restart` to rebuild it from the role docs, or `team-agent quick-start <teamdir>` for first launch (to add a role at runtime use `team-agent add-agent <id> --role-file <path>`)",
            input.display(),
            run_workspace.display(),
            team_key,
            expected.display()
        )));
    }

    // E5 §3 解耦:team_dir = 角色定义目录(用户目录),恒取 state.team_dir;缺则回落 run_workspace。
    let team_dir = state
        .get("team_dir")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| run_workspace.clone());

    Ok(SelectedTeam {
        run_workspace,
        team_key,
        state,
        team_dir,
        spec_workspace,
        spec_path,
    })
}

fn spec_workspace_from_state(state: &Value) -> Option<PathBuf> {
    state
        .get("spec_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .and_then(|s| Path::new(s).parent().map(Path::to_path_buf))
        .or_else(|| {
            state
                .get("team_dir")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        })
}

fn selected_team_key(state: &Value, team: Option<&str>) -> String {
    state
        .get("active_team_key")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .or_else(|| team.filter(|s| !s.is_empty()).map(ToString::to_string))
        .unwrap_or_else(|| team_state_key(state))
}
