use std::path::PathBuf;

use super::descriptor::ResumeMode;
use super::types::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeSessionId(String);

impl NativeSessionId {
    pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        if !nonblank(&value) {
            return Err(ContractError::Invalid("native session id"));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CwdIdentity {
    pub path: PathBuf,
    pub identity: Digest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStorage {
    Local,
    Cloud,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureOrigin {
    CurrentNativeSession,
    OwnedExitHint,
    ExactBacking,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeBinding {
    pub provider: ProviderId,
    pub native_session: NativeSessionId,
    pub storage: SessionStorage,
    pub cwd: CwdIdentity,
    pub native: NativeIdentity,
    pub source: InstanceIdentity,
    pub origin: CaptureOrigin,
    pub evidence_kind: EvidenceKind,
    pub evidence_sha256: Digest,
    pub backing: Option<PathBuf>,
}

pub struct ResumeExpectation<'a> {
    pub provider: &'a ProviderId,
    pub source: &'a InstanceIdentity,
    pub cwd: &'a CwdIdentity,
    pub native: &'a NativeIdentity,
    pub evidence_kind: EvidenceKind,
    pub mode: ResumeMode,
}

/// No listing, newest-file lookup, cloud fallback, or fresh-session fallback.
pub fn validate_resume(
    binding: &ResumeBinding,
    expected: &ResumeExpectation<'_>,
) -> Result<(), ContractError> {
    expected.native.validate()?;
    require_absolute(&expected.cwd.path, "resume cwd")?;
    if &binding.provider != expected.provider || &binding.source != expected.source {
        return Err(ContractError::Mismatch("resume owner"));
    }
    if binding.storage != SessionStorage::Local {
        return Err(ContractError::Invalid("resume requires local provenance"));
    }
    if &binding.cwd != expected.cwd {
        return Err(ContractError::Mismatch("resume cwd"));
    }
    if &binding.native != expected.native {
        return Err(ContractError::Mismatch("resume native identity"));
    }
    if binding.evidence_kind != expected.evidence_kind {
        return Err(ContractError::Mismatch("resume evidence"));
    }
    if let Some(path) = &binding.backing {
        require_absolute(path, "resume backing")?;
    }
    if expected.mode == ResumeMode::ExactPath && binding.backing.is_none() {
        return Err(ContractError::Invalid("exact backing path required"));
    }
    Ok(())
}
