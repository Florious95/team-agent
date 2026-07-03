//! Stage 0 of the identity-boundary unified plan (architect direction
//! 2026-06-23): `SessionAttributionKey`.
//!
//! Pure additive: nobody constructs this type yet. Stage 1 (Claude strict
//! capture) starts populating it inside `CaptureSessionContext`; Stage 4
//! (CLI/DB scope gate) starts using it as the DB/event filter key.
//!
//! Why a struct, not a tuple:
//! - The four fields are not interchangeable: `provider` selects the
//!   tombstone/claimed-session table, `team_key` is the multi-team boundary,
//!   `agent_id` is the worker identity inside a team, `session_id` is the
//!   provider-issued transcript identity.
//! - The architect's verdict §1 names this exact shape as the "shared
//!   identity primitive" that owner / capture / multi-team all need so they
//!   don't drift into per-module ad-hoc keys.
//!
//! Single-team behaviour: `team_key` is still populated (from
//! `state::projection::team_state_key` or the same selector path Stage 0
//! introduces). The key is invariant — capture/owner/CLI all read it from
//! the same source.

use crate::model::enums::Provider;

/// The canonical identity of a "this worker owns this provider transcript"
/// claim. Stage 1 + Stage 4 will both store and compare via this struct.
///
/// All four fields are non-empty by construction (the constructor refuses
/// empty `team_key`, `agent_id`, or `session_id` — those would collapse
/// distinct sessions into one).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionAttributionKey {
    provider: Provider,
    team_key: String,
    agent_id: String,
    session_id: String,
}

impl SessionAttributionKey {
    pub fn new(
        provider: Provider,
        team_key: impl Into<String>,
        agent_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Option<Self> {
        let team_key = team_key.into();
        let agent_id = agent_id.into();
        let session_id = session_id.into();
        if team_key.is_empty() || agent_id.is_empty() || session_id.is_empty() {
            return None;
        }
        Some(Self {
            provider,
            team_key,
            agent_id,
            session_id,
        })
    }

    pub fn provider(&self) -> Provider {
        self.provider
    }

    pub fn team_key(&self) -> &str {
        &self.team_key
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_empty_team_key() {
        assert!(SessionAttributionKey::new(Provider::Claude, "", "agent_a", "sess_1").is_none());
    }

    #[test]
    fn refuses_empty_agent_id() {
        assert!(SessionAttributionKey::new(Provider::Claude, "alpha", "", "sess_1").is_none());
    }

    #[test]
    fn refuses_empty_session_id() {
        assert!(SessionAttributionKey::new(Provider::Claude, "alpha", "agent_a", "").is_none());
    }

    #[test]
    fn accepts_well_formed_key_and_preserves_fields() {
        let key = SessionAttributionKey::new(
            Provider::Claude,
            "alpha",
            "reviewer",
            "36dd1d1a-2766-4636-856c-03f14a4bb803",
        )
        .unwrap();
        assert_eq!(key.provider(), Provider::Claude);
        assert_eq!(key.team_key(), "alpha");
        assert_eq!(key.agent_id(), "reviewer");
        assert_eq!(key.session_id(), "36dd1d1a-2766-4636-856c-03f14a4bb803");
    }

    /// Same provider + agent_id + session_id but different team_key → distinct
    /// identity. This is the multi-team isolation invariant the unified plan
    /// requires (architect §1 cross point: tombstone/claimed by full
    /// (provider, team_key, agent_id, session_id) tuple).
    #[test]
    fn different_team_keys_produce_different_keys() {
        let alpha =
            SessionAttributionKey::new(Provider::Claude, "alpha", "reviewer", "s1").unwrap();
        let beta = SessionAttributionKey::new(Provider::Claude, "beta", "reviewer", "s1").unwrap();
        assert_ne!(alpha, beta);
    }

    /// Same team but different agent_id → distinct (per-worker isolation).
    #[test]
    fn different_agent_ids_produce_different_keys() {
        let reviewer =
            SessionAttributionKey::new(Provider::Claude, "alpha", "reviewer", "s1").unwrap();
        let frontend =
            SessionAttributionKey::new(Provider::Claude, "alpha", "frontend", "s1").unwrap();
        assert_ne!(reviewer, frontend);
    }
}
