//! Framework scope carried across shared-business boundaries.
use team_agent_contract::contract::{session::CwdIdentity, types::ScopeId};
use team_agent_contract::host::{digest, digest_hex};
use team_agent_contract::orchestration::Error;


/// Created from the selected framework Team, never from MCP tool arguments.
pub struct FrameworkContext {
    pub workspace: CwdIdentity,
    pub team: String,
    pub native_scope: ScopeId,
}
impl FrameworkContext {
    pub fn route(&self) -> Result<String, Error> {
        Ok(format!(
            "framework-{}",
            digest_hex(digest(&serde_json::to_vec(&(
                &self.workspace,
                &self.team,
                &self.native_scope
            ))?))
        ))
    }
}
