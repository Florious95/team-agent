use std::fmt;
use std::path::{Component, Path, PathBuf};

/// A stable, non-secret explanation suitable for a static descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reason {
    pub code: &'static str,
    pub message: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Support<T> {
    Supported(T),
    Unsupported(Reason),
    Unverified(Reason),
}

impl<T> Support<T> {
    pub fn require(&self, field: &'static str) -> Result<&T, ContractError> {
        match self {
            Self::Supported(value) => Ok(value),
            Self::Unsupported(reason) => Err(ContractError::Unsupported {
                field,
                reason: *reason,
            }),
            Self::Unverified(reason) => Err(ContractError::Unverified {
                field,
                reason: *reason,
            }),
        }
    }

    pub fn is_supported(&self) -> bool {
        matches!(self, Self::Supported(_))
    }
}

/// No Default: an absent behavior must never become an empty successful hook.
pub enum HookBinding<T> {
    Bound(T),
    NotRequired(Reason),
    Unsupported(Reason),
    Unverified(Reason),
}

impl<T> HookBinding<T> {
    pub fn require(&self, hook: &'static str) -> Result<&T, ContractError> {
        match self {
            Self::Bound(value) => Ok(value),
            Self::NotRequired(_) => Err(ContractError::MissingHook(hook)),
            Self::Unsupported(reason) => Err(ContractError::Unsupported {
                field: hook,
                reason: *reason,
            }),
            Self::Unverified(reason) => Err(ContractError::Unverified {
                field: hook,
                reason: *reason,
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContractError {
    Invalid(&'static str),
    Mismatch(&'static str),
    MissingHook(&'static str),
    Unsupported { field: &'static str, reason: Reason },
    Unverified { field: &'static str, reason: Reason },
    UnknownProvider,
    AmbiguousProvider,
    UnknownModel,
    AmbiguousModel,
    UnknownEffort,
    EffortRejected(Reason),
    EffortNotAvailableForModel,
    ProfileUnavailable,
    AmbiguousProfile,
    BudgetOverflow,
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Error values never copy argv, prompt, environment values or native captures.
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ContractError {}

pub(crate) fn nonblank(value: &str) -> bool {
    !value.trim().is_empty() && !value.contains('\0')
}

pub(crate) fn valid_env_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
}

pub(crate) fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

macro_rules! id_type {
    ($($name:ident),+ $(,)?) => {$ (
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[derive(serde::Serialize, serde::Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
                let value = value.into();
                if !valid_id(&value) {
                    return Err(ContractError::Invalid(stringify!($name)));
                }
                Ok(Self(value))
            }
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl TryFrom<String> for $name {
            type Error = ContractError;
            fn try_from(value: String) -> Result<Self, Self::Error> { Self::new(value) }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self { value.0 }
        }
    )+ };
}

id_type!(
    ScopeId,
    SeatId,
    InstanceId,
    MessageId,
    AttemptId,
    OperationId
);

#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct ProviderId(String);

impl TryFrom<String> for ProviderId {
    type Error = ContractError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<ProviderId> for String {
    fn from(value: ProviderId) -> Self {
        value.0
    }
}

impl ProviderId {
    pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        if !valid_id(&value) || value.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(ContractError::Invalid("provider id"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Generation(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Digest(pub [u8; 32]);

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstanceIdentity {
    pub scope: ScopeId,
    pub seat: SeatId,
    pub instance: InstanceId,
    pub generation: Generation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Platform {
    MacOs,
    Linux,
    Windows,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativeIdentity {
    pub version: String,
    pub harness: String,
    pub ui: String,
    pub platform: Platform,
    pub executable_sha256: Digest,
}

impl NativeIdentity {
    pub fn validate(&self) -> Result<(), ContractError> {
        if [&self.version, &self.harness, &self.ui]
            .into_iter()
            .all(|s| nonblank(s))
        {
            Ok(())
        } else {
            Err(ContractError::Invalid("native identity"))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Catalog,
    Fresh,
    Resume,
    FirstBusiness,
    OrdinarySend,
    StartupBypassAck,
    SessionInspect,
    ToolInspect,
    InWindowBranch,
    NewSeatFullSnapshot,
    NativeNewSeat,
    Stop,
    Shutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Tmux,
    Acp,
    DirectStdin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogicalTool {
    SendMessage,
    ReportResult,
    GetTeamStatus,
}

pub const TEAM_TOOLS: [LogicalTool; 3] = [
    LogicalTool::SendMessage,
    LogicalTool::ReportResult,
    LogicalTool::GetTeamStatus,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EvidenceKind {
    Fixture,
    Native,
}

/// Acceptance is a separate, scoped record, never a mutable descriptor field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityEvidence {
    pub provider: ProviderId,
    pub native: NativeIdentity,
    pub profile_id: String,
    pub policy_sha256: Digest,
    pub candidate_sha256: Digest,
    pub evidence_sha256: Digest,
    pub kind: EvidenceKind,
    pub operation: Operation,
    pub channel: Channel,
}

pub fn require_absolute(path: &Path, field: &'static str) -> Result<(), ContractError> {
    if path.is_absolute()
        && !path.as_os_str().as_encoded_bytes().contains(&0)
        && !path.components().any(|c| matches!(c, Component::ParentDir))
    {
        Ok(())
    } else {
        Err(ContractError::Invalid(field))
    }
}

/// Lexically constrained request, NOT a proof against symlinks or concurrent writers.
/// The future owned-I/O implementation must additionally enforce no-follow and ownership.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "(PathBuf, PathBuf)", into = "(PathBuf, PathBuf)")]
pub struct OwnedPath {
    root: PathBuf,
    relative: PathBuf,
}

impl TryFrom<(PathBuf, PathBuf)> for OwnedPath {
    type Error = ContractError;
    fn try_from((root, relative): (PathBuf, PathBuf)) -> Result<Self, Self::Error> {
        Self::new(root, relative)
    }
}
impl From<OwnedPath> for (PathBuf, PathBuf) {
    fn from(value: OwnedPath) -> Self {
        (value.root, value.relative)
    }
}

impl OwnedPath {
    pub fn new(root: PathBuf, relative: PathBuf) -> Result<Self, ContractError> {
        require_absolute(&root, "owned root")?;
        if relative.as_os_str().is_empty()
            || relative.as_os_str().as_encoded_bytes().contains(&0)
            || relative
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(ContractError::Invalid("owned relative path"));
        }
        Ok(Self { root, relative })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn relative(&self) -> &Path {
        &self.relative
    }
    pub fn path(&self) -> PathBuf {
        self.root.join(&self.relative)
    }
}
