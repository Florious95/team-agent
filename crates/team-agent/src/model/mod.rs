//! step 2 · model — typed bedrock (REWRITE_PROMPT.md §6 step 2).
//!
//! 纯数据 + 纯函数核心:Python 的散字符串 / `dict[str,Any]` 在此变 §3 id-newtype +
//! §19 穷尽 enum。真相源 `team-agent-public` @ v0.2.11 (439bef8);序列化与 Python
//! **字节级一致**(§7),靠 golden 值 / fixture 双跑锁死。
//!
//! §10 + 02-model 卡 §152:纯层没有 panic 的理由 —— 整个 `model` deny
//! unwrap/expect/panic;所有校验/解析返 `Result`,所有 `Option` 穷尽 match。
//! (clippy tool-lint;`cargo build`/`test` 下被忽略,`cargo clippy` 下强制。)
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod enums;
pub mod errors;
pub mod ids;
// 0.5.45 naming-addressing (design §3.2/§4.1): shared pure name
// similarity ranking helper. crate-private consumer set = cli/emit.rs
// (subcommand hint), cli/named_address.rs (typo candidates),
// cli/send.rs (positional adapter), mcp_server/tools.rs (owner-scoped
// peer suggestions). Never a routing authority — advisory only.
pub(crate) mod name_similarity;
pub mod paths;
pub mod permissions;
pub mod routing;
pub mod spec;
pub mod task_graph;
pub mod yaml;

pub use errors::ModelError;
