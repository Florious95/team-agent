//! §19/§3 移植 `permissions.py`:role/provider/alias → 规范化权限矩阵。
//!
//! Python 全程裸 `str` + `dict[str, Any]`(`expand_tools`/`resolve_permissions`/
//! `missing_tools`)。这里把"工具名"收成 `enums::Tool` 穷尽集,"provider×tool→强度"
//! 收成 `enums::Enforcement` 查表。
//!
//! **与 Python 的一处刻意收紧**(§3):Python `expand_tools` 对**未知工具串**原样
//! passthrough(`['banana']` → `['banana']`)。typed 层无法表示未知 `Tool`,故
//! 未知串 → `ModelError::Validation`(混传/拼错编不过 → 提前炸,而非把脏串带进矩阵)。
//! 已知别名(`fs_*`/`@builtin`/`@team-orchestrator`/`@cao-mcp-server`/`*`)与 8 个
//! 规范名照常展开。
//!
//! **排序差异说明**:Python 按工具**字符串**字母序排;`BTreeSet<Tool>` 按 `Tool`
//! 的 `Ord`(即 `enums.rs` 声明序)排,两者不同。集合**成员**完全一致(行为对拍锁
//! 死);需要字节级对齐 Python `tools` 数组时用 `sorted_tool_strings()` 取字母序。

use std::collections::BTreeSet;

use crate::model::enums::{Enforcement, Provider, Tool};
use crate::model::errors::ModelError;
use crate::model::ids::AgentId;

impl Tool {
    /// 规范名串 → `Tool`(`CANONICAL_TOOLS` `permissions.py:5-14`)。未知串 → `None`。
    fn from_canonical_str(s: &str) -> Option<Self> {
        Some(match s {
            "fs_read" => Self::FsRead,
            "fs_write" => Self::FsWrite,
            "fs_list" => Self::FsList,
            "execute_bash" => Self::ExecuteBash,
            "git_diff" => Self::GitDiff,
            "network" => Self::Network,
            "mcp_team" => Self::McpTeam,
            "provider_builtin" => Self::ProviderBuiltin,
            _ => return None,
        })
    }

    /// 规范名串(serde rename 的等价值;查表 / 字节对拍用)。
    fn canonical_str(self) -> &'static str {
        match self {
            Self::FsRead => "fs_read",
            Self::FsWrite => "fs_write",
            Self::FsList => "fs_list",
            Self::ExecuteBash => "execute_bash",
            Self::GitDiff => "git_diff",
            Self::Network => "network",
            Self::McpTeam => "mcp_team",
            Self::ProviderBuiltin => "provider_builtin",
        }
    }
}

/// `ROLE_DEFAULTS`(`permissions.py:16-40`)+ fallback 到 `developer`
/// (`default_tools_for_role` `permissions.py:84-85`)。未知 role → `developer` 集。
///
/// 注:返回**声明序集合**;Python 返回的是 role 字面顺序的 list,但下游 `resolve_*`
/// 立即过 `expand_tools`(去重+排序),顺序不参与对拍,故此处用 `BTreeSet` 即可。
pub fn default_tools_for_role(role: &str) -> BTreeSet<Tool> {
    use Tool::*;
    let tools: &[Tool] = match role {
        "leader" | "supervisor" => &[FsRead, FsList, McpTeam, ProviderBuiltin],
        "researcher" => &[FsRead, FsList, Network, McpTeam, ProviderBuiltin],
        "reviewer" | "code_reviewer" => &[FsRead, FsList, GitDiff, McpTeam, ProviderBuiltin],
        // implementation_engineer / developer / 未知 role 全落 developer 集。
        _ => &[
            FsRead,
            FsWrite,
            FsList,
            ExecuteBash,
            GitDiff,
            McpTeam,
            ProviderBuiltin,
        ],
    };
    tools.iter().copied().collect()
}

/// 别名/规范名串展开为 `Tool` 集(`expand_tools` `permissions.py:68-81`)。
///
/// - `fs_*` → {fs_read, fs_write, fs_list}
/// - `@builtin` → {provider_builtin}
/// - `@team-orchestrator` / `@cao-mcp-server` → {mcp_team}
/// - `*` → 全部 8 个规范工具
/// - 规范名 → 自身
/// - 其余(未知串)→ `Err(Validation)`(见模块头注:刻意收紧)
pub fn expand_tools<I, S>(tools: I) -> Result<BTreeSet<Tool>, ModelError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    use Tool::*;
    let mut out: BTreeSet<Tool> = BTreeSet::new();
    for raw in tools {
        let t = raw.as_ref();
        match t {
            "fs_*" => out.extend([FsRead, FsWrite, FsList]),
            "@builtin" => {
                out.insert(ProviderBuiltin);
            }
            "@team-orchestrator" | "@cao-mcp-server" => {
                out.insert(McpTeam);
            }
            "*" => out.extend([
                FsRead,
                FsWrite,
                FsList,
                ExecuteBash,
                GitDiff,
                Network,
                McpTeam,
                ProviderBuiltin,
            ]),
            other => match Tool::from_canonical_str(other) {
                Some(tool) => {
                    out.insert(tool);
                }
                None => {
                    return Err(ModelError::Validation(format!(
                        "unknown tool or alias: {other:?}"
                    )))
                }
            },
        }
    }
    Ok(out)
}

/// Python `expand_tools` 的**字符串级**镜像(`permissions.py:68-81`):别名展开 + 未知串
/// **passthrough** + `sorted(set(...))`。`validate_spec` 用它(再逐个查 canonical 报
/// `unknown tool`);typed [`expand_tools`] 则对未知串 `Err`(见模块头注的刻意收紧)。
pub fn expand_tool_strings<I, S>(tools: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    for raw in tools {
        match raw.as_ref() {
            "fs_*" => out.extend(["fs_read", "fs_write", "fs_list"].into_iter().map(String::from)),
            "@builtin" => out.push("provider_builtin".to_string()),
            "@team-orchestrator" | "@cao-mcp-server" => out.push("mcp_team".to_string()),
            "*" => out.extend(
                [
                    "fs_read",
                    "fs_write",
                    "fs_list",
                    "execute_bash",
                    "git_diff",
                    "network",
                    "mcp_team",
                    "provider_builtin",
                ]
                .into_iter()
                .map(String::from),
            ),
            other => out.push(other.to_string()),
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 是否 8 个规范工具之一(`CANONICAL_TOOLS`)。
pub fn is_canonical_tool(name: &str) -> bool {
    Tool::from_canonical_str(name).is_some()
}

/// provider×tool → 强度(`PROVIDER_ENFORCEMENT` `permissions.py:42-65`)。
///
/// 表里没有的 provider(如 `claude`)= Python 的 `.get(provider, {})` → 空表 →
/// 每个 tool 再 `.get(tool, "prompt_only")` → 全 `prompt_only`(对拍 `R-unknownprov`)。
pub fn provider_enforcement(provider: Provider, tool: Tool) -> Enforcement {
    use Enforcement::{Hard, PromptOnly};
    use Tool::*;
    match provider {
        Provider::ClaudeCode => match tool {
            Network => PromptOnly,
            FsRead | FsWrite | FsList | ExecuteBash | GitDiff | McpTeam | ProviderBuiltin => Hard,
        },
        Provider::GeminiCli => match tool {
            Network | McpTeam => PromptOnly,
            FsRead | FsWrite | FsList | ExecuteBash | GitDiff | ProviderBuiltin => Hard,
        },
        Provider::Fake => Hard,
        // Copilot(C-2-1 cr verdict):execute_bash/fs_write/network/mcp_team = hard,
        // fs_read/fs_list/git_diff/provider_builtin = prompt_only(诚实:copilot 无
        // 对应 deny kind,framework 不替决,留给 provider prompt 控制;MUST-NOT-13)。
        Provider::Copilot => match tool {
            FsRead | FsList | GitDiff | ProviderBuiltin => PromptOnly,
            Network | FsWrite | ExecuteBash | McpTeam => Hard,
        },
        // codex: 全 prompt_only。claude: 不在表中 → 全 prompt_only(同 fallback)。
        Provider::Codex | Provider::Claude => PromptOnly,
    }
}

/// 单条已解析工具(对应 Python `resolved_tools[]` 的 `{tool, enforcement}`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedTool {
    pub tool: Tool,
    pub enforcement: Enforcement,
}

/// `resolve_permissions` 的最小 typed 入参(替 Python `agent: dict[str, Any]`)。
///
/// `tools = Some(..)` 对应显式 `agent["tools"]`;`None`(或 Python 的空 list →
/// `or` falsy)对应回退 `default_tools_for_role(role)`。
#[derive(Debug, Clone)]
pub struct AgentPermissionInput {
    pub id: Option<AgentId>,
    pub provider: Provider,
    pub role: Option<String>,
    /// 显式工具/别名串;`None` → 按 role 取默认。
    pub tools: Option<Vec<String>>,
}

/// `resolve_permissions` 的结果(对应 Python 返回 dict)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPermissions {
    pub agent_id: Option<AgentId>,
    pub provider: Provider,
    /// 展开+去重的工具集(`BTreeSet` 序;字节对齐用 `sorted_tool_strings`)。
    pub tools: BTreeSet<Tool>,
    pub resolved_tools: Vec<ResolvedTool>,
    pub has_prompt_only: bool,
}

impl ResolvedPermissions {
    /// 工具名按**字符串字母序**(= Python `tools` 数组顺序),字节对拍用。
    pub fn sorted_tool_strings(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self.tools.iter().map(|t| t.canonical_str()).collect();
        v.sort_unstable();
        v
    }
}

/// `resolve_permissions`(`permissions.py:88-106`)。
///
/// Python `tools = agent.get("tools") or default_tools_for_role(role)`:空 list 在
/// Python 是 falsy → 也回退默认。这里 `Some(vec![])` 同样视作"无显式工具"回退默认。
pub fn resolve_permissions(
    agent: &AgentPermissionInput,
) -> Result<ResolvedPermissions, ModelError> {
    let resolved: BTreeSet<Tool> = match &agent.tools {
        Some(tools) if !tools.is_empty() => expand_tools(tools)?,
        _ => {
            let role = agent.role.as_deref().unwrap_or("developer");
            default_tools_for_role(role)
        }
    };

    let resolved_tools: Vec<ResolvedTool> = resolved
        .iter()
        .map(|&tool| ResolvedTool {
            tool,
            enforcement: provider_enforcement(agent.provider, tool),
        })
        .collect();

    let has_prompt_only = resolved_tools
        .iter()
        .any(|e| e.enforcement == Enforcement::PromptOnly);

    Ok(ResolvedPermissions {
        agent_id: agent.id.clone(),
        provider: agent.provider,
        tools: resolved,
        resolved_tools,
        has_prompt_only,
    })
}

/// task 类型 → 附加必需工具(`task_required_tools` `permissions.py:109-118`)。
/// 未知/缺失 type 不附加。`requires_tools` 也走 `expand_tools`(同未知串收紧规则)。
pub fn task_required_tools(
    task_type: Option<&str>,
    requires_tools: &[String],
) -> Result<BTreeSet<Tool>, ModelError> {
    let mut required: Vec<String> = requires_tools.to_vec();
    match task_type {
        Some("implementation") | Some("bug_fix") | Some("test") => {
            required.push("fs_write".to_string());
            required.push("execute_bash".to_string());
        }
        Some("review") | Some("risk_check") => {
            required.push("fs_read".to_string());
            required.push("git_diff".to_string());
        }
        Some("research") | Some("architecture") => {
            required.push("fs_read".to_string());
        }
        _ => {}
    }
    expand_tools(required)
}

/// `missing_tools`(`permissions.py:121-123`):task 需要但 agent 不被允许的工具。
/// 返回**字母序**(= Python list 顺序)。
pub fn missing_tools(
    agent: &AgentPermissionInput,
    task_type: Option<&str>,
    requires_tools: &[String],
) -> Result<Vec<Tool>, ModelError> {
    let allowed = resolve_permissions(agent)?.tools;
    let required = task_required_tools(task_type, requires_tools)?;
    // required 已是去重集;按字符串字母序输出,对齐 Python。
    let mut missing: Vec<Tool> = required.into_iter().filter(|t| !allowed.contains(t)).collect();
    missing.sort_unstable_by_key(|t| t.canonical_str());
    Ok(missing)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// `BTreeSet<Tool>` → 字母序串(对齐 Python `sorted(...)` list)。
    fn strs(set: &BTreeSet<Tool>) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = set.iter().map(|t| t.canonical_str()).collect();
        v.sort_unstable();
        v
    }

    fn agent(provider: Provider, role: Option<&str>, tools: Option<&[&str]>) -> AgentPermissionInput {
        AgentPermissionInput {
            id: Some(AgentId::new("x")),
            provider,
            role: role.map(str::to_string),
            tools: tools.map(|t| t.iter().map(|s| s.to_string()).collect()),
        }
    }

    // ---- expand_tools 行为对拍(golden from team-agent-public@439bef8) ----
    #[test]
    fn expand_tools_matches_python_golden() {
        // E1
        assert_eq!(strs(&expand_tools(["fs_*"]).unwrap()), ["fs_list", "fs_read", "fs_write"]);
        // E2
        assert_eq!(strs(&expand_tools(["@builtin"]).unwrap()), ["provider_builtin"]);
        // E3
        assert_eq!(
            strs(&expand_tools(["*"]).unwrap()),
            [
                "execute_bash",
                "fs_list",
                "fs_read",
                "fs_write",
                "git_diff",
                "mcp_team",
                "network",
                "provider_builtin"
            ]
        );
        // E4
        assert_eq!(
            strs(&expand_tools(["@team-orchestrator", "@cao-mcp-server"]).unwrap()),
            ["mcp_team"]
        );
        // E5 — 去重
        assert_eq!(
            strs(&expand_tools(["fs_read", "fs_read", "network"]).unwrap()),
            ["fs_read", "network"]
        );
        // E6
        assert!(expand_tools(Vec::<String>::new()).unwrap().is_empty());
        // E7
        assert_eq!(
            strs(&expand_tools(["fs_*", "@builtin", "network"]).unwrap()),
            ["fs_list", "fs_read", "fs_write", "network", "provider_builtin"]
        );
        // E8 — * 已含 fs_read,去重无变化
        assert_eq!(
            strs(&expand_tools(["*", "fs_read"]).unwrap()),
            [
                "execute_bash",
                "fs_list",
                "fs_read",
                "fs_write",
                "git_diff",
                "mcp_team",
                "network",
                "provider_builtin"
            ]
        );
    }

    /// §3 刻意收紧:未知工具串 → Validation(Python 是 passthrough,见模块头注)。
    #[test]
    fn expand_tools_rejects_unknown_string() {
        let err = expand_tools(["banana"]).unwrap_err();
        assert!(matches!(err, ModelError::Validation(_)));
        // 别名/规范名混入未知串也整体失败。
        assert!(expand_tools(["fs_read", "fs_*", "banana"]).is_err());
    }

    // ---- default_tools_for_role 行为对拍 ----
    #[test]
    fn default_tools_for_role_matches_python_golden() {
        assert_eq!(
            strs(&default_tools_for_role("leader")),
            ["fs_list", "fs_read", "mcp_team", "provider_builtin"]
        );
        assert_eq!(
            strs(&default_tools_for_role("supervisor")),
            ["fs_list", "fs_read", "mcp_team", "provider_builtin"]
        );
        assert_eq!(
            strs(&default_tools_for_role("researcher")),
            ["fs_list", "fs_read", "mcp_team", "network", "provider_builtin"]
        );
        assert_eq!(
            strs(&default_tools_for_role("reviewer")),
            ["fs_list", "fs_read", "git_diff", "mcp_team", "provider_builtin"]
        );
        assert_eq!(
            strs(&default_tools_for_role("code_reviewer")),
            ["fs_list", "fs_read", "git_diff", "mcp_team", "provider_builtin"]
        );
        let dev = [
            "execute_bash",
            "fs_list",
            "fs_read",
            "fs_write",
            "git_diff",
            "mcp_team",
            "provider_builtin",
        ];
        assert_eq!(strs(&default_tools_for_role("developer")), dev);
        assert_eq!(strs(&default_tools_for_role("implementation_engineer")), dev);
        // 未知 role → developer 集。
        assert_eq!(strs(&default_tools_for_role("qa")), dev);
    }

    // ---- provider_enforcement 行为对拍(矩阵逐格) ----
    #[test]
    fn provider_enforcement_matches_python_matrix() {
        use Enforcement::{Hard, PromptOnly};
        // claude_code: network=prompt_only,其余 hard。
        assert_eq!(provider_enforcement(Provider::ClaudeCode, Tool::Network), PromptOnly);
        for t in [Tool::FsRead, Tool::FsWrite, Tool::FsList, Tool::ExecuteBash, Tool::GitDiff, Tool::McpTeam, Tool::ProviderBuiltin] {
            assert_eq!(provider_enforcement(Provider::ClaudeCode, t), Hard);
        }
        // gemini_cli: network + mcp_team = prompt_only,其余 hard。
        assert_eq!(provider_enforcement(Provider::GeminiCli, Tool::Network), PromptOnly);
        assert_eq!(provider_enforcement(Provider::GeminiCli, Tool::McpTeam), PromptOnly);
        for t in [Tool::FsRead, Tool::FsWrite, Tool::FsList, Tool::ExecuteBash, Tool::GitDiff, Tool::ProviderBuiltin] {
            assert_eq!(provider_enforcement(Provider::GeminiCli, t), Hard);
        }
        // codex: 全 prompt_only。fake: 全 hard。
        for t in [Tool::FsRead, Tool::Network, Tool::McpTeam, Tool::ProviderBuiltin] {
            assert_eq!(provider_enforcement(Provider::Codex, t), PromptOnly);
            assert_eq!(provider_enforcement(Provider::Fake, t), Hard);
            // claude 不在表中 → fallback prompt_only。
            assert_eq!(provider_enforcement(Provider::Claude, t), PromptOnly);
        }
    }

    // ---- resolve_permissions 行为对拍(golden) ----
    #[test]
    fn resolve_permissions_leader_claude_code() {
        // R-leader-cc
        let r = resolve_permissions(&agent(Provider::ClaudeCode, Some("leader"), None)).unwrap();
        assert_eq!(r.sorted_tool_strings(), ["fs_list", "fs_read", "mcp_team", "provider_builtin"]);
        assert!(!r.has_prompt_only);
        for e in &r.resolved_tools {
            assert_eq!(e.enforcement, Enforcement::Hard);
        }
    }

    #[test]
    fn resolve_permissions_developer_codex_all_prompt_only() {
        // R-dev-codex
        let r = resolve_permissions(&agent(Provider::Codex, Some("developer"), None)).unwrap();
        assert_eq!(
            r.sorted_tool_strings(),
            ["execute_bash", "fs_list", "fs_read", "fs_write", "git_diff", "mcp_team", "provider_builtin"]
        );
        assert!(r.has_prompt_only);
        for e in &r.resolved_tools {
            assert_eq!(e.enforcement, Enforcement::PromptOnly);
        }
    }

    #[test]
    fn resolve_permissions_impl_gemini_mcp_team_prompt_only() {
        // R-impl-gem:mcp_team=prompt_only,其余 hard → has_prompt_only=true。
        let r = resolve_permissions(&agent(Provider::GeminiCli, Some("implementation_engineer"), None)).unwrap();
        assert!(r.has_prompt_only);
        let mcp = r.resolved_tools.iter().find(|e| e.tool == Tool::McpTeam).unwrap();
        assert_eq!(mcp.enforcement, Enforcement::PromptOnly);
        let gd = r.resolved_tools.iter().find(|e| e.tool == Tool::GitDiff).unwrap();
        assert_eq!(gd.enforcement, Enforcement::Hard);
    }

    #[test]
    fn resolve_permissions_explicit_tools_and_norole_and_unknownprov() {
        // R-explicit:显式 tools 走 expand_tools,provider=claude_code。
        let r = resolve_permissions(&agent(Provider::ClaudeCode, None, Some(&["fs_*", "network"]))).unwrap();
        assert_eq!(r.sorted_tool_strings(), ["fs_list", "fs_read", "fs_write", "network"]);
        assert!(r.has_prompt_only); // network=prompt_only

        // R-norole:无 role → developer 默认,全 hard(claude_code)。
        let r2 = resolve_permissions(&AgentPermissionInput {
            id: Some(AgentId::new("a6")),
            provider: Provider::ClaudeCode,
            role: None,
            tools: None,
        })
        .unwrap();
        assert_eq!(
            r2.sorted_tool_strings(),
            ["execute_bash", "fs_list", "fs_read", "fs_write", "git_diff", "mcp_team", "provider_builtin"]
        );
        assert!(!r2.has_prompt_only);

        // R-unknownprov:provider=claude 不在矩阵 → 全 prompt_only。
        let r3 = resolve_permissions(&agent(Provider::Claude, Some("developer"), None)).unwrap();
        assert!(r3.has_prompt_only);
        for e in &r3.resolved_tools {
            assert_eq!(e.enforcement, Enforcement::PromptOnly);
        }

        // agent_id passthrough。
        assert_eq!(r.agent_id, Some(AgentId::new("x")));
    }

    #[test]
    fn resolve_permissions_empty_tools_falls_back_to_role_default() {
        // Python: agent.get("tools") or default → 空 list falsy → 用 role 默认。
        let r = resolve_permissions(&agent(Provider::ClaudeCode, Some("leader"), Some(&[]))).unwrap();
        assert_eq!(r.sorted_tool_strings(), ["fs_list", "fs_read", "mcp_team", "provider_builtin"]);
    }

    // ---- task_required_tools 行为对拍(golden) ----
    #[test]
    fn task_required_tools_matches_python_golden() {
        // T1
        assert_eq!(strs(&task_required_tools(Some("implementation"), &[]).unwrap()), ["execute_bash", "fs_write"]);
        // T2
        assert_eq!(strs(&task_required_tools(Some("review"), &[]).unwrap()), ["fs_read", "git_diff"]);
        // T3
        assert_eq!(strs(&task_required_tools(Some("research"), &[]).unwrap()), ["fs_read"]);
        // T4
        assert_eq!(
            strs(&task_required_tools(Some("bug_fix"), &["network".to_string()]).unwrap()),
            ["execute_bash", "fs_write", "network"]
        );
        // T5 未知 type
        assert!(task_required_tools(Some("unknown_type"), &[]).unwrap().is_empty());
        // T6 无 type
        assert!(task_required_tools(None, &[]).unwrap().is_empty());
        // T7 requires_tools 走 expand_tools(fs_* 展开)
        assert_eq!(
            strs(&task_required_tools(Some("architecture"), &["fs_*".to_string()]).unwrap()),
            ["fs_list", "fs_read", "fs_write"]
        );
    }

    // ---- missing_tools 行为对拍(golden) ----
    #[test]
    fn missing_tools_matches_python_golden() {
        // M1:reviewer(claude_code)缺 implementation 所需 fs_write/execute_bash。
        let m1 = missing_tools(&agent(Provider::ClaudeCode, Some("reviewer"), None), Some("implementation"), &[]).unwrap();
        assert_eq!(m1, [Tool::ExecuteBash, Tool::FsWrite]);
        // M2:developer 不缺。
        let m2 = missing_tools(&agent(Provider::ClaudeCode, Some("developer"), None), Some("implementation"), &[]).unwrap();
        assert!(m2.is_empty());
        // M3:researcher 缺 git_diff(review 需要),但有 fs_read。
        let m3 = missing_tools(&agent(Provider::ClaudeCode, Some("researcher"), None), Some("review"), &[]).unwrap();
        assert_eq!(m3, [Tool::GitDiff]);
    }
}
