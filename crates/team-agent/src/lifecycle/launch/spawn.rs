//! unit-8 (Stage 3) — `lifecycle::launch::spawn` phase boundary.
//!
//! Dedicated home for the worker spawn executor. Future commits migrate
//! `spawn_first_with_env_unset` and `spawn_into_with_env_unset` from
//! launch.rs:376-405 here.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnPhase {
    SpawnFirstWithEnvUnset,
    SpawnIntoWithEnvUnset,
}

impl SpawnPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SpawnFirstWithEnvUnset => "launch.spawn.first_with_env_unset",
            Self::SpawnIntoWithEnvUnset => "launch.spawn.into_with_env_unset",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_labels_are_dotted_paths_under_launch_spawn() {
        assert!(SpawnPhase::SpawnFirstWithEnvUnset
            .as_str()
            .starts_with("launch.spawn."));
    }
}
