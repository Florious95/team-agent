//! MCP lifecycle tool facades.
//!
//! S0 keeps the old placeholder behavior behind stable module boundaries so
//! follow-up lanes can implement real lifecycle logic without touching tools.rs.

mod agent_ops;
mod state_status;

pub(crate) use agent_ops::{clone_agent, fork_agent, reset_agent, stop_agent};
pub(crate) use state_status::{get_team_status, update_state};
