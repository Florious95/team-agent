//! The sole new-runtime provider registration. This is not an admission claim:
//! the descriptor's Unverified gates still apply to each requested operation.
use team_agent_contract::contract::descriptor::ProviderDescriptor;
use team_agent_contract::kiro::KIRO_DESCRIPTOR;

const DESCRIPTORS: &[&ProviderDescriptor] = &[&KIRO_DESCRIPTOR];

pub fn descriptor(key: &str) -> Option<&'static ProviderDescriptor> {
    DESCRIPTORS.iter().copied().find(|descriptor| {
        descriptor.identity.id == key || descriptor.identity.aliases.contains(&key)
    })
}

/// Recognize a misspelled/case-modified contract declaration so it can fail
/// explicitly instead of accidentally selecting a legacy default provider.
pub fn recognizes(key: &str) -> bool {
    let key = key.trim();
    DESCRIPTORS.iter().any(|descriptor| {
        descriptor.identity.id.eq_ignore_ascii_case(key)
            || descriptor
                .identity
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(key))
    })
}
