//! §3 id newtypes —— Python 把 agent/task/team id 全用裸 `str` 混传(route/owner-gate/
//! tasks 串台)。newtype 让"混传"编不过。serde `transparent` 保证序列化字节 == 裸字符串。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::errors::ModelError;

/// 透明 String newtype:序列化/反序列化与裸字符串字节一致。
macro_rules! string_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }
    };
}

string_newtype!(
    /// agent id。leader 默认 `"leader"` 但 spec 可改 —— 不硬编码。
    AgentId
);
string_newtype!(
    /// task id。deps/assignee/cycle 按 str 比对,易与 AgentId 串台 → newtype 防混。
    TaskId
);
string_newtype!(
    /// multi-team 指针 key(`team_state_key()`)。
    TeamKey
);

/// owner epoch(§3)。first-time bind 写 0(`state.py:454,461`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OwnerEpoch(pub u64);

impl OwnerEpoch {
    /// first-time leader bind 的 epoch。
    pub const FIRST: OwnerEpoch = OwnerEpoch(0);
}

/// leader 身份/owner epoch 一等公民。公式锁死(`state.py:34-38`),跨 Python/Rust
/// 影子交叉验证靠它,**一字节不能改**(§7)。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LeaderSessionUuid(String);

impl LeaderSessionUuid {
    const SEP: char = '\0';

    /// `sha256(NUL.join([fp, ws, user, team]).encode("utf-8")).hexdigest()[:32]`。
    /// 任一输入含 NUL → `Err`(Python `state.py:36-37` raise ValueError)。
    pub fn derive(
        machine_fingerprint: &str,
        workspace_abspath: &str,
        os_user: &str,
        team_id: &str,
    ) -> Result<Self, ModelError> {
        let parts = [machine_fingerprint, workspace_abspath, os_user, team_id];
        if parts.iter().any(|p| p.contains(Self::SEP)) {
            return Err(ModelError::Validation(
                "leader_session_uuid inputs must not contain NUL".to_string(),
            ));
        }
        let joined = parts.join("\0");
        let digest = Sha256::digest(joined.as_bytes());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        // hexdigest()[:32] —— sha256 恒 64 hex,take(32) 无 panic 风险。
        Ok(LeaderSessionUuid(hex.chars().take(32).collect()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for LeaderSessionUuid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    // golden 值由 Python 真相源 `derive_leader_session_uuid` 算出(team-agent-public@439bef8)。
    // 这是 §4.2「行为 diff 双跑」对该函数的字节锁。
    #[test]
    fn leader_session_uuid_matches_python_golden() {
        assert_eq!(
            LeaderSessionUuid::derive("fp-abc", "/ws/proj", "alice", "team-1")
                .unwrap()
                .as_str(),
            "5742d595dbb2f5a16608f68c3050127b"
        );
        assert_eq!(
            LeaderSessionUuid::derive("", "", "", "").unwrap().as_str(),
            "709e80c88487a2411e1ee4dfb9f22a86"
        );
        assert_eq!(
            LeaderSessionUuid::derive("m", "/Users/alauda/x", "root", "default")
                .unwrap()
                .as_str(),
            "92c5e32dc5710ea095f956bfb594bc6b"
        );
    }

    #[test]
    fn leader_session_uuid_rejects_nul_in_any_part() {
        assert!(LeaderSessionUuid::derive("a\0b", "w", "u", "t").is_err());
        assert!(LeaderSessionUuid::derive("w", "x", "u\0", "t").is_err());
    }

    #[test]
    fn leader_session_uuid_is_32_lowercase_hex() {
        let u = LeaderSessionUuid::derive("x", "y", "z", "w").unwrap();
        assert_eq!(u.as_str().len(), 32);
        assert!(u.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn id_newtypes_serialize_as_bare_string() {
        assert_eq!(serde_json::to_string(&AgentId::new("leader")).unwrap(), "\"leader\"");
        assert_eq!(serde_json::from_str::<TaskId>("\"t1\"").unwrap(), TaskId::new("t1"));
        assert_eq!(TeamKey::from("default").as_str(), "default");
    }

    #[test]
    fn owner_epoch_first_is_zero_and_serializes_as_int() {
        assert_eq!(OwnerEpoch::FIRST.0, 0);
        assert_eq!(serde_json::to_string(&OwnerEpoch(3)).unwrap(), "3");
        assert_eq!(serde_json::from_str::<OwnerEpoch>("0").unwrap(), OwnerEpoch::FIRST);
    }
}
