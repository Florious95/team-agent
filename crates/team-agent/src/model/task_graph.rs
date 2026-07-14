//! 移植 `team_agent/task_graph.py` 的三个纯函数:`find_dependency_cycle` /
//! `ready_tasks` / `update_task_status`(真相源 v0.2.11)。
//!
//! Python 在 `list[dict[str, Any]]` 上裸跑;此处把"一个 task"收成最小本地类型
//! [`TaskNode`](§3:id 用 [`TaskId`] newtype,status 用穷尽 [`TaskStatus`])。算法语义
//! **逐行对齐** Python:环检测的发现顺序(含 `stack.index` / `[node,node]` 两支)、ready
//! 判定的状态白名单 + deps 全 `done`、update 的状态校验与就地转移。
//!
//! §10:无 unwrap/expect/panic;未知状态 / 未知 id 返 [`ModelError`]。

use serde::{Deserialize, Serialize};

use crate::model::enums::TaskStatus;
use crate::model::errors::ModelError;
use crate::model::ids::TaskId;

/// 一个 task 节点 —— Python `dict` 里被 task_graph 触及的最小字段集。
///
/// Python `status` 缺省视作 `"pending"`(`ready_tasks` 第 55 行 `get(...,"pending")`);
/// 此处 status 为必填穷尽枚举,缺省语义靠构造时落 [`TaskStatus::Pending`] 体现。
/// `last_result_summary` / `artifact_refs` 仅由 [`update_task_status`] 写入,故用 `Option`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNode {
    pub id: TaskId,
    #[serde(default)]
    pub deps: Vec<TaskId>,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result_summary: Option<String>,
    /// `artifact_refs` 在 Python 是 `list[dict]` passthrough;此处保留为不透明 JSON 值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_refs: Option<Vec<serde_json::Value>>,
}

impl TaskNode {
    /// 便捷构造:summary/artifacts 为空(仅由 [`update_task_status`] 写入)。
    pub fn new(id: impl Into<TaskId>, deps: Vec<TaskId>, status: TaskStatus) -> Self {
        Self {
            id: id.into(),
            deps,
            status,
            last_result_summary: None,
            artifact_refs: None,
        }
    }
}

/// 找出依赖图中第一个被发现的环,返回其结点路径(含闭合的重复尾结点);无环返空 `Vec`。
///
/// 逐行对齐 Python `find_dependency_cycle`(`task_graph.py:19-48`):
/// - graph 只含有 id 的 task(无 id 的被丢弃),按 task 列表顺序建立(= dict 插入序)。
/// - DFS 用 `visiting`/`visited`/`stack` 三态;命中正在访问的结点时,若它在 `stack` 则
///   返回 `stack[idx..] + [node]`,否则退化返回 `[node, node]`(忠实保留此分支)。
/// - 外层按插入序遍历每个结点,返回首个命中的环。
pub fn find_dependency_cycle(tasks: &[TaskNode]) -> Vec<TaskId> {
    // graph:保持插入序的 (id -> deps)。Python 用 dict comprehension,后写覆盖先写。
    let mut order: Vec<&TaskId> = Vec::new();
    let mut graph: std::collections::HashMap<&TaskId, &Vec<TaskId>> =
        std::collections::HashMap::new();
    for t in tasks {
        if graph.insert(&t.id, &t.deps).is_none() {
            order.push(&t.id);
        }
    }

    let mut visiting: std::collections::HashSet<&TaskId> = std::collections::HashSet::new();
    let mut visited: std::collections::HashSet<&TaskId> = std::collections::HashSet::new();
    let mut stack: Vec<&TaskId> = Vec::new();

    for &node in &order {
        if let Some(cycle) = visit(node, &graph, &mut visiting, &mut visited, &mut stack) {
            return cycle;
        }
    }
    Vec::new()
}

/// `find_dependency_cycle` 内部 DFS(对齐 Python 闭包 `visit`,`task_graph.py:25-42`)。
/// 显式递归;命中环返回 `Some(path)`,否则 `None`。
fn visit<'a>(
    node: &'a TaskId,
    graph: &std::collections::HashMap<&'a TaskId, &'a Vec<TaskId>>,
    visiting: &mut std::collections::HashSet<&'a TaskId>,
    visited: &mut std::collections::HashSet<&'a TaskId>,
    stack: &mut Vec<&'a TaskId>,
) -> Option<Vec<TaskId>> {
    if visited.contains(node) {
        return None;
    }
    if visiting.contains(node) {
        // `node in stack` → stack[index(node):] + [node];否则退化 [node, node]。
        if let Some(idx) = stack.iter().position(|&n| n == node) {
            let mut cycle: Vec<TaskId> = stack[idx..].iter().map(|&n| n.clone()).collect();
            cycle.push(node.clone());
            return Some(cycle);
        }
        return Some(vec![node.clone(), node.clone()]);
    }
    visiting.insert(node);
    stack.push(node);
    if let Some(deps) = graph.get(node) {
        for dep in deps.iter() {
            // Python:`if dep in graph` —— 只跟随图内结点,缺失依赖不入环。
            if graph.contains_key(dep) {
                if let Some(cycle) = visit(dep, graph, visiting, visited, stack) {
                    return Some(cycle);
                }
            }
        }
    }
    stack.pop();
    visiting.remove(node);
    visited.insert(node);
    None
}

/// 返回所有"就绪"的 task(对齐 Python `ready_tasks`,`task_graph.py:51-60`):
/// status ∈ `{pending, ready, needs_retry}` **且** 所有依赖的 task 状态为 `done`。
///
/// 依赖若不在 `tasks` 中(查无此 id),Python `by_id.get(dep,{}).get("status")` 为 `None`
/// → 非 `done` → 该 task 不就绪。此处用 `HashMap` 查表,缺失即 `None`,语义一致。
/// 保持输入顺序(Python 顺序遍历 `tasks`)。
pub fn ready_tasks(tasks: &[TaskNode]) -> Vec<&TaskNode> {
    let by_id: std::collections::HashMap<&TaskId, &TaskNode> =
        tasks.iter().map(|t| (&t.id, t)).collect();

    let mut ready: Vec<&TaskNode> = Vec::new();
    for task in tasks {
        if !matches!(
            task.status,
            TaskStatus::Pending | TaskStatus::Ready | TaskStatus::NeedsRetry
        ) {
            continue;
        }
        let deps_done = task
            .deps
            .iter()
            .all(|dep| by_id.get(dep).map(|d| d.status) == Some(TaskStatus::Done));
        if deps_done {
            ready.push(task);
        }
    }
    ready
}

/// 就地更新某个 task 的状态(对齐 Python `update_task_status`,`task_graph.py:63-80`):
/// status 用穷尽 [`TaskStatus`] 已自带"合法状态集"约束(Python 的 `TASK_STATUSES` 校验
/// 由类型系统承担)。找到 id 匹配的 task 则写 status,并在 `summary`/`artifact_refs`
/// 为 `Some` 时一并写入(`None` 不触碰原字段,对齐 Python 的 `is not None` 守卫)。
///
/// 找不到 id → `Err(ModelError::Runtime("Unknown task id: ..."))`(对齐 Python `KeyError`)。
pub fn update_task_status(
    tasks: &mut [TaskNode],
    task_id: &TaskId,
    status: TaskStatus,
    summary: Option<&str>,
    artifact_refs: Option<Vec<serde_json::Value>>,
) -> Result<(), ModelError> {
    for task in tasks.iter_mut() {
        if &task.id == task_id {
            task.status = status;
            if let Some(s) = summary {
                task.last_result_summary = Some(s.to_string());
            }
            if let Some(refs) = artifact_refs {
                task.artifact_refs = Some(refs);
            }
            return Ok(());
        }
    }
    Err(ModelError::Runtime(format!("Unknown task id: {task_id}")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn t(id: &str, deps: &[&str], status: TaskStatus) -> TaskNode {
        TaskNode::new(id, deps.iter().map(|d| TaskId::new(*d)).collect(), status)
    }

    fn cycle_ids(c: &[TaskId]) -> Vec<&str> {
        c.iter().map(|i| i.as_str()).collect()
    }
    fn ready_ids<'a>(r: &[&'a TaskNode]) -> Vec<&'a str> {
        r.iter().map(|n| n.id.as_str()).collect()
    }

    // ---- find_dependency_cycle:golden 来自 Python 真相源(§4.2 双跑) ----

    #[test]
    fn cycle_linear_chain_has_no_cycle() {
        // Python C1 -> []
        let tasks = [
            t("a", &[], TaskStatus::Pending),
            t("b", &["a"], TaskStatus::Pending),
            t("c", &["b"], TaskStatus::Pending),
        ];
        assert!(find_dependency_cycle(&tasks).is_empty());
    }

    #[test]
    fn cycle_two_node() {
        // Python C2 -> ['a','b','a']
        let tasks = [
            t("a", &["b"], TaskStatus::Pending),
            t("b", &["a"], TaskStatus::Pending),
        ];
        assert_eq!(
            cycle_ids(&find_dependency_cycle(&tasks)),
            vec!["a", "b", "a"]
        );
    }

    #[test]
    fn cycle_three_node() {
        // Python C3 -> ['a','b','c','a']
        let tasks = [
            t("a", &["b"], TaskStatus::Pending),
            t("b", &["c"], TaskStatus::Pending),
            t("c", &["a"], TaskStatus::Pending),
        ];
        assert_eq!(
            cycle_ids(&find_dependency_cycle(&tasks)),
            vec!["a", "b", "c", "a"]
        );
    }

    #[test]
    fn cycle_self_loop() {
        // Python C4 -> ['a','a'](走 stack.index 分支,非 [node,node] 退化分支)
        let tasks = [t("a", &["a"], TaskStatus::Pending)];
        assert_eq!(cycle_ids(&find_dependency_cycle(&tasks)), vec!["a", "a"]);
    }

    #[test]
    fn cycle_reached_from_acyclic_prefix() {
        // Python C5:x->a->b->a -> ['a','b','a'](前缀 x 不在环里,被 stack.index 切掉)
        let tasks = [
            t("x", &["a"], TaskStatus::Pending),
            t("a", &["b"], TaskStatus::Pending),
            t("b", &["a"], TaskStatus::Pending),
        ];
        assert_eq!(
            cycle_ids(&find_dependency_cycle(&tasks)),
            vec!["a", "b", "a"]
        );
    }

    #[test]
    fn cycle_missing_dep_is_ignored() {
        // Python C6:a 依赖图外 'zzz' -> 不入环 -> []
        let tasks = [
            t("a", &["zzz"], TaskStatus::Pending),
            t("b", &["a"], TaskStatus::Pending),
        ];
        assert!(find_dependency_cycle(&tasks).is_empty());
    }

    #[test]
    fn cycle_empty_input() {
        // Python C7 -> []
        assert!(find_dependency_cycle(&[]).is_empty());
    }

    // ---- ready_tasks:golden 来自 Python 真相源 ----

    #[test]
    fn ready_pending_no_deps() {
        // Python R1 -> ['a'](running 不在白名单)
        let tasks = [
            t("a", &[], TaskStatus::Pending),
            t("b", &[], TaskStatus::Running),
        ];
        assert_eq!(ready_ids(&ready_tasks(&tasks)), vec!["a"]);
    }

    #[test]
    fn ready_default_pending() {
        // Python R2:无 status key 视作 pending -> ['a']。本类型用显式 Pending 表达缺省。
        let tasks = [t("a", &[], TaskStatus::Pending)];
        assert_eq!(ready_ids(&ready_tasks(&tasks)), vec!["a"]);
    }

    #[test]
    fn ready_requires_deps_done() {
        // Python R3:a done -> b 就绪;c 依赖未完成的 b -> 不就绪 -> ['b']
        let tasks = [
            t("a", &[], TaskStatus::Done),
            t("b", &["a"], TaskStatus::Pending),
            t("c", &["b"], TaskStatus::Pending),
        ];
        assert_eq!(ready_ids(&ready_tasks(&tasks)), vec!["b"]);
    }

    #[test]
    fn ready_status_whitelist() {
        // Python R4:needs_retry / ready 在白名单,blocked 不在 -> ['a','b']
        let tasks = [
            t("a", &[], TaskStatus::NeedsRetry),
            t("b", &[], TaskStatus::Ready),
            t("c", &[], TaskStatus::Blocked),
        ];
        assert_eq!(ready_ids(&ready_tasks(&tasks)), vec!["a", "b"]);
    }

    #[test]
    fn ready_missing_dep_not_done() {
        // Python R5:依赖查无此 id -> 视作非 done -> 不就绪 -> []
        let tasks = [t("b", &["missing"], TaskStatus::Pending)];
        assert!(ready_tasks(&tasks).is_empty());
    }

    #[test]
    fn ready_terminal_statuses_excluded() {
        // Python R6:done/failed/cancelled 均不在白名单 -> []
        let tasks = [
            t("a", &[], TaskStatus::Done),
            t("b", &[], TaskStatus::Failed),
            t("c", &[], TaskStatus::Cancelled),
        ];
        assert!(ready_tasks(&tasks).is_empty());
    }

    // ---- update_task_status:golden 来自 Python 真相源 ----

    #[test]
    fn update_sets_status_summary_and_artifacts() {
        // Python U1:a -> done + summary "ok" + artifact_refs [{"path":"x"}]
        let mut tasks = [
            t("a", &[], TaskStatus::Pending),
            t("b", &["a"], TaskStatus::Pending),
        ];
        let refs = vec![serde_json::json!({"path": "x"})];
        update_task_status(
            &mut tasks,
            &TaskId::new("a"),
            TaskStatus::Done,
            Some("ok"),
            Some(refs),
        )
        .unwrap();
        assert_eq!(tasks[0].status, TaskStatus::Done);
        assert_eq!(tasks[0].last_result_summary.as_deref(), Some("ok"));
        assert_eq!(
            tasks[0].artifact_refs,
            Some(vec![serde_json::json!({"path": "x"})])
        );
        // b 不动
        assert_eq!(tasks[1].status, TaskStatus::Pending);
    }

    #[test]
    fn update_status_only_leaves_other_fields() {
        // Python U2:仅改 status,既有 last_result_summary "old" 不被触碰
        let mut node = t("a", &[], TaskStatus::Pending);
        node.last_result_summary = Some("old".to_string());
        let mut tasks = [node];
        update_task_status(
            &mut tasks,
            &TaskId::new("a"),
            TaskStatus::Running,
            None,
            None,
        )
        .unwrap();
        assert_eq!(tasks[0].status, TaskStatus::Running);
        assert_eq!(tasks[0].last_result_summary.as_deref(), Some("old"));
    }

    #[test]
    fn update_unknown_id_errors() {
        // Python U4:未知 id -> KeyError "Unknown task id: zzz"。Rust -> Runtime。
        let mut tasks = [t("a", &[], TaskStatus::Pending)];
        let err = update_task_status(
            &mut tasks,
            &TaskId::new("zzz"),
            TaskStatus::Done,
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(err, ModelError::Runtime("Unknown task id: zzz".to_string()));
    }

    #[test]
    fn update_summary_none_but_artifacts_set() {
        // Python U5:summary=None 只写 artifact_refs
        let mut tasks = [t("a", &[], TaskStatus::Pending)];
        let refs = vec![serde_json::json!({"r": 1})];
        update_task_status(
            &mut tasks,
            &TaskId::new("a"),
            TaskStatus::Done,
            None,
            Some(refs),
        )
        .unwrap();
        assert_eq!(tasks[0].status, TaskStatus::Done);
        assert!(tasks[0].last_result_summary.is_none());
        assert_eq!(
            tasks[0].artifact_refs,
            Some(vec![serde_json::json!({"r": 1})])
        );
    }
}
