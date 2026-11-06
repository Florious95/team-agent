//!
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

    // ---- find_dependency_cycle:golden 来自 Python 真相源(§4.2 双跑) ----

    #[test]
    fn dependency_cycle_python_golden_cases() {
        // (Python id, 旧函数名, note, tasks, 完整环路径)。保持输入和发现顺序。
        let cases = [
            ("C1", "cycle_linear_chain_has_no_cycle", "Python C1 -> []",
                vec![t("a", &[], TaskStatus::Pending), t("b", &["a"], TaskStatus::Pending), t("c", &["b"], TaskStatus::Pending)], vec![]),
            ("C2", "cycle_two_node", "Python C2 -> ['a','b','a']",
                vec![t("a", &["b"], TaskStatus::Pending), t("b", &["a"], TaskStatus::Pending)], vec!["a", "b", "a"]),
            ("C3", "cycle_three_node", "Python C3 -> ['a','b','c','a']",
                vec![t("a", &["b"], TaskStatus::Pending), t("b", &["c"], TaskStatus::Pending), t("c", &["a"], TaskStatus::Pending)], vec!["a", "b", "c", "a"]),
            ("C4", "cycle_self_loop", "Python C4 -> ['a','a'](走 stack.index 分支,非 [node,node] 退化分支)",
                vec![t("a", &["a"], TaskStatus::Pending)], vec!["a", "a"]),
            ("C5", "cycle_reached_from_acyclic_prefix", "Python C5:x->a->b->a -> ['a','b','a'](前缀 x 不在环里,被 stack.index 切掉)",
                vec![t("x", &["a"], TaskStatus::Pending), t("a", &["b"], TaskStatus::Pending), t("b", &["a"], TaskStatus::Pending)], vec!["a", "b", "a"]),
            ("C6", "cycle_missing_dep_is_ignored", "Python C6:a 依赖图外 'zzz' -> 不入环 -> []",
                vec![t("a", &["zzz"], TaskStatus::Pending), t("b", &["a"], TaskStatus::Pending)], vec![]),
            ("C7", "cycle_empty_input", "Python C7 -> []", vec![], vec![]),
        ];
        for (id, name, note, tasks, expected) in cases {
            assert_eq!(cycle_ids(&find_dependency_cycle(&tasks)), expected, "case={id}/{name}; {note}");
        }
    }

    // ---- ready_tasks:golden 来自 Python 真相源 ----


    // ---- update_task_status:golden 来自 Python 真相源 ----




}
