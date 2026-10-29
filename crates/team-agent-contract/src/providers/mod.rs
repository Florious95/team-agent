//! Provider-specific contract leaves; registration is owned by the host application.
pub mod claude;
pub mod codex;
pub mod cursor;
pub mod grok_build;
pub mod pi;

/// Generation-owned launch files stay in the runtime root, not the project cwd.
pub(crate) fn owned_file(owner: &crate::contract::types::InstanceIdentity, name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from("seats").join(owner.seat.as_str()).join(owner.instance.as_str()).join(name)
}
