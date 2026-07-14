//! 移植 `team_agent/routing.py` 的纯函数 `route_task`(真相源 v0.2.11)。
//!
//! 操作 spec/task 的 [`yaml::Value`]。返回 [`RouteResult`]{agent_id, reason}。
//! `when` 表达式 + `files` glob 用 `regex` 忠实复刻 Python `re.fullmatch` / `fnmatch.translate`
//! (`LazyLock<Option<Regex>>` 编译一次,`.ok()` 无 panic,§10)。
//!
//! §11/§132 铁律:未知 explicit assignee **静默回退 leader + reason**(不报错)—— 契约依赖此容错。

use std::sync::LazyLock;

use regex::Regex;

use crate::model::ids::AgentId;
use crate::model::yaml::Value as Yaml;

/// `route_task` 返回:被指派的 agent + 人读理由(字节对齐 Python dict)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteResult {
    pub agent_id: AgentId,
    pub reason: String,
}

// re.fullmatch 的三个 when 模式(anchored 双端;`.ok()` → 常量必 Some)。
static WHEN_TYPE_IN: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^task\.type\s+in\s+\[(.*)\]\z").ok());
static WHEN_EQ: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"^task\.(type|risk)\s*==\s*['"]([^'"]+)['"]\z"#).ok());
static WHEN_FILES: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^task\.files\s+matches\s+\[(.*)\]\z").ok());
static QUOTED: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r#"['"]([^'"]+)['"]"#).ok());

/// `routing.route_task(spec, task)`。
pub fn route_task(spec: &Yaml, task: &Yaml) -> RouteResult {
    let leader_id = spec
        .get("leader")
        .and_then(|l| l.get("id"))
        .and_then(Yaml::as_str)
        .unwrap_or("leader");

    // valid_agents = {leader_id} ∪ {agent.id}
    let agents: &[Yaml] = spec.get("agents").and_then(Yaml::as_list).unwrap_or(&[]);
    let valid = |id: &str| {
        id == leader_id
            || agents
                .iter()
                .any(|a| a.get("id").and_then(Yaml::as_str) == Some(id))
    };

    // explicit assignee(truthy)。
    if let Some(explicit) = task
        .get("assignee")
        .and_then(Yaml::as_str)
        .filter(|s| !s.is_empty())
    {
        if valid(explicit) {
            return RouteResult {
                agent_id: AgentId::from(explicit),
                reason: "explicit assignee on task".to_string(),
            };
        }
        return RouteResult {
            agent_id: AgentId::from(leader_id),
            reason: format!("unknown explicit assignee {}", py_repr_str(explicit)),
        };
    }

    // 规则按 priority 降序(稳定排序,平手保留原序)。
    let mut rules: Vec<&Yaml> = spec
        .get("routing")
        .and_then(|r| r.get("rules"))
        .and_then(Yaml::as_list)
        .unwrap_or(&[])
        .iter()
        .collect();
    rules.sort_by_key(|r| std::cmp::Reverse(r.get("priority").and_then(Yaml::as_i64).unwrap_or(0)));

    for rule in rules {
        if rule_matches(rule, task) {
            let id = rule
                .get("assign_to")
                .and_then(Yaml::as_str)
                .unwrap_or(leader_id);
            let rule_id = rule.get("id").and_then(Yaml::as_str).unwrap_or("<unnamed>");
            return RouteResult {
                agent_id: AgentId::from(id),
                reason: format!("matched routing rule {rule_id}"),
            };
        }
    }

    let default = spec
        .get("routing")
        .and_then(|r| r.get("default_assignee"))
        .and_then(Yaml::as_str)
        .unwrap_or(leader_id);
    RouteResult {
        agent_id: AgentId::from(default),
        reason: "no routing rule matched".to_string(),
    }
}

fn rule_matches(rule: &Yaml, task: &Yaml) -> bool {
    if let Some(m) = rule.get("match") {
        if m.is_map() && !structured_match(m, task) {
            return false;
        }
    }
    if let Some(when) = rule.get("when").filter(|w| w.is_truthy()) {
        // Python str(when):when 通常是字符串;非字符串退化为其 Debug(极少见)。
        let expr = when
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("{when:?}"));
        if !when_match(&expr, task) {
            return false;
        }
    }
    true
}

fn structured_match(m: &Yaml, task: &Yaml) -> bool {
    if let Some(t) = m.get("type") {
        if !as_list(t).iter().any(|v| Some(*v) == task.get("type")) {
            return false;
        }
    }
    if let Some(r) = m.get("risk") {
        if !as_list(r).iter().any(|v| Some(*v) == task.get("risk")) {
            return false;
        }
    }
    if let Some(a) = m.get("assignee") {
        if !as_list(a).iter().any(|v| Some(*v) == task.get("assignee")) {
            return false;
        }
    }
    if let Some(req) = m.get("requires_tools") {
        let required: Vec<&str> = as_list(req).iter().filter_map(|v| v.as_str()).collect();
        let actual: Vec<&str> = task
            .get("requires_tools")
            .and_then(Yaml::as_list)
            .unwrap_or(&[])
            .iter()
            .filter_map(Yaml::as_str)
            .collect();
        if !required.iter().all(|r| actual.contains(r)) {
            return false;
        }
    }
    if let Some(files_pat) = m.get("files") {
        let patterns: Vec<&str> = as_list(files_pat)
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        let files: Vec<&str> = task
            .get("files")
            .and_then(Yaml::as_list)
            .unwrap_or(&[])
            .iter()
            .filter_map(Yaml::as_str)
            .collect();
        if !files.iter().any(|f| patterns.iter().any(|p| fnmatch(f, p))) {
            return false;
        }
    }
    true
}

fn when_match(expr: &str, task: &Yaml) -> bool {
    let expr = expr.trim();
    if let Some(re) = WHEN_TYPE_IN.as_ref() {
        if let Some(c) = re.captures(expr) {
            let values = quoted_values(c.get(1).map_or("", |m| m.as_str()));
            return task
                .get("type")
                .and_then(Yaml::as_str)
                .is_some_and(|t| values.iter().any(|v| v == t));
        }
    }
    if let Some(re) = WHEN_EQ.as_ref() {
        if let Some(c) = re.captures(expr) {
            let field = c.get(1).map_or("", |m| m.as_str());
            let want = c.get(2).map_or("", |m| m.as_str());
            return task.get(field).and_then(Yaml::as_str) == Some(want);
        }
    }
    if let Some(re) = WHEN_FILES.as_ref() {
        if let Some(c) = re.captures(expr) {
            let patterns = quoted_values(c.get(1).map_or("", |m| m.as_str()));
            let files: Vec<&str> = task
                .get("files")
                .and_then(Yaml::as_list)
                .unwrap_or(&[])
                .iter()
                .filter_map(Yaml::as_str)
                .collect();
            return files.iter().any(|f| patterns.iter().any(|p| fnmatch(f, p)));
        }
    }
    false
}

fn quoted_values(raw: &str) -> Vec<String> {
    let Some(re) = QUOTED.as_ref() else {
        return Vec::new();
    };
    re.captures_iter(raw)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

/// Python `_as_list`:list → 元素;否则 → [self]。
fn as_list(value: &Yaml) -> Vec<&Yaml> {
    match value {
        Yaml::List(l) => l.iter().collect(),
        other => vec![other],
    }
}

/// 仿 Python `fnmatch.fnmatch`(posix,normcase=identity):translate → regex 全匹配。
fn fnmatch(name: &str, pattern: &str) -> bool {
    match Regex::new(&fnmatch_translate(pattern)) {
        Ok(re) => re.is_match(name),
        Err(_) => false,
    }
}

/// 仿 Python `fnmatch.translate`:`*`→`.*`,`?`→`.`,`[seq]`→字符类,其余 escape;
/// 包成 `^(?s:BODY)\z`(re.match 起锚 + `\Z` 终锚)。
fn fnmatch_translate(pat: &str) -> String {
    let chars: Vec<char> = pat.chars().collect();
    let n = chars.len();
    let mut body = String::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        i += 1;
        match c {
            '*' => body.push_str(".*"),
            '?' => body.push('.'),
            '[' => {
                let mut j = i;
                if j < n && chars[j] == '!' {
                    j += 1;
                }
                if j < n && chars[j] == ']' {
                    j += 1;
                }
                while j < n && chars[j] != ']' {
                    j += 1;
                }
                if j >= n {
                    body.push_str("\\[");
                } else {
                    let mut stuff: String =
                        chars[i..j].iter().collect::<String>().replace('\\', "\\\\");
                    i = j + 1;
                    if let Some(rest) = stuff.strip_prefix('!') {
                        stuff = format!("^{rest}");
                    } else if stuff.starts_with('^') || stuff.starts_with('[') {
                        stuff = format!("\\{stuff}");
                    }
                    body.push('[');
                    body.push_str(&stuff);
                    body.push(']');
                }
            }
            other => body.push_str(&regex::escape(&other.to_string())),
        }
    }
    format!("^(?s:{body})\\z")
}

/// Python `repr()` of a str(简化:identifier 类用单引号,含单引号无双引号用双引号)。
fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::model::yaml;

    fn spec() -> Yaml {
        yaml::loads(include_str!("testdata/team.spec.yaml")).unwrap()
    }
    fn task(json: &str) -> Yaml {
        yaml::loads(json).unwrap()
    }

    // golden 由真相源 route_task 双跑取(team-agent-public@439bef8)。
    #[test]
    fn route_task_matches_python_golden() {
        let s = spec();
        let cases: &[(&str, &str, &str)] = &[
            (
                r#"{"type":"implementation"}"#,
                "codex_implementer",
                "matched routing rule implementation-to-codex",
            ),
            (
                r#"{"type":"research"}"#,
                "codex_researcher",
                "matched routing rule research-to-codex",
            ),
            (
                r#"{"type":"review"}"#,
                "codex_reviewer",
                "matched routing rule review-to-codex",
            ),
            (r#"{"type":"unknown"}"#, "leader", "no routing rule matched"),
            (
                r#"{"assignee":"codex_implementer","type":"x"}"#,
                "codex_implementer",
                "explicit assignee on task",
            ),
            (
                r#"{"assignee":"ghost","type":"x"}"#,
                "leader",
                "unknown explicit assignee 'ghost'",
            ),
            (r#"{}"#, "leader", "no routing rule matched"),
        ];
        for (t, want_id, want_reason) in cases {
            let r = route_task(&s, &task(t));
            assert_eq!(r.agent_id.as_str(), *want_id, "task={t}");
            assert_eq!(r.reason, *want_reason, "task={t}");
        }
    }

    #[test]
    fn fnmatch_matches_python() {
        // 与 Python fnmatch.fnmatch 对拍样例。
        assert!(fnmatch("src/a.rs", "src/*.rs"));
        assert!(fnmatch("a.py", "*.py"));
        assert!(fnmatch("x/y.rs", "*.rs")); // fnmatch 的 * 跨 '/'(与 glob crate 不同)
        assert!(!fnmatch("FOO", "foo")); // 大小写敏感
        assert!(fnmatch("a.rs", "*.[rp]s")); // 字符类
        assert!(!fnmatch("a.xs", "*.[rp]s"));
    }

    #[test]
    fn structured_match_and_files() {
        // 构造带 match dict 的规则,验 type/files。
        let rule =
            task(r#"{"id":"r1","assign_to":"w","priority":1,"match":{"files":["src/*.rs"]}}"#);
        assert!(rule_matches(&rule, &task(r#"{"files":["src/main.rs"]}"#)));
        assert!(!rule_matches(&rule, &task(r#"{"files":["docs/x.md"]}"#)));
    }

    #[test]
    fn structured_match_assignee_is_a_real_predicate() {
        // Backlog control bug: compiler emits one route per agent with match.assignee.
        // match.assignee must be a real predicate; otherwise every route is unconditional and
        // tasks funnel into the first/highest-priority rule.
        let alpha_rule = task(
            r#"{"id":"route-alpha","match":{"assignee":["alpha"]},"assign_to":"alpha","priority":10}"#,
        );
        let bravo_rule = task(
            r#"{"id":"route-bravo","match":{"assignee":["bravo"]},"assign_to":"bravo","priority":10}"#,
        );
        let bravo_task = task(r#"{"id":"t1","assignee":"bravo"}"#);

        assert!(!rule_matches(&alpha_rule, &bravo_task));
        assert!(rule_matches(&bravo_rule, &bravo_task));
    }
}
