//! Shared typed catalog for ambient tmux pane authority refusals.
//!
//! This module owns identities and payload shape only. Launchers and CLI
//! presenters remain responsible for collecting facts and rendering them.

use std::path::PathBuf;

pub const REASON_FIELD: &str = "reason";
pub const AVAILABILITY_FIELD: &str = "availability";
pub const CAUSE_FIELD: &str = "cause";
pub const ACTION_REQUIRED_FIELD: &str = "action_required";
pub const HINT_ACTION_FIELD: &str = "hint_action";
pub const ACTION_FIELD: &str = "action";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaneAuthorityRefusalField {
    RequestedWorkspace,
    ObservedPaneId,
    ObservedPaneWorkspace,
    Endpoint,
    CallerControllingTty,
    ObservedPaneTty,
}

impl PaneAuthorityRefusalField {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RequestedWorkspace => "requested_workspace",
            Self::ObservedPaneId => "observed_pane_id",
            Self::ObservedPaneWorkspace => "observed_pane_workspace",
            Self::Endpoint => "endpoint",
            Self::CallerControllingTty => "caller_controlling_tty",
            Self::ObservedPaneTty => "observed_pane_tty",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaneAuthorityRefusalReason {
    AmbientTmuxEndpointUnavailable,
    AmbientPaneIdUnavailable,
    AmbientPaneWorkspaceUnavailable,
    CallerControllingTtyUnavailable,
    ObservedPaneTtyUnavailable,
    PaneTtyMismatch,
    PaneWorkspaceMismatch,
}

impl PaneAuthorityRefusalReason {
    pub const ALL: [Self; 7] = [
        Self::AmbientTmuxEndpointUnavailable,
        Self::AmbientPaneIdUnavailable,
        Self::AmbientPaneWorkspaceUnavailable,
        Self::CallerControllingTtyUnavailable,
        Self::ObservedPaneTtyUnavailable,
        Self::PaneTtyMismatch,
        Self::PaneWorkspaceMismatch,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AmbientTmuxEndpointUnavailable => "AmbientTmuxEndpointUnavailable",
            Self::AmbientPaneIdUnavailable => "AmbientPaneIdUnavailable",
            Self::AmbientPaneWorkspaceUnavailable => "AmbientPaneWorkspaceUnavailable",
            Self::CallerControllingTtyUnavailable => "CallerControllingTtyUnavailable",
            Self::ObservedPaneTtyUnavailable => "ObservedPaneTtyUnavailable",
            Self::PaneTtyMismatch => "PaneTtyMismatch",
            Self::PaneWorkspaceMismatch => "PaneWorkspaceMismatch",
        }
    }

    pub const fn required_fact_fields(self) -> &'static [PaneAuthorityRefusalField] {
        use PaneAuthorityRefusalField::{
            CallerControllingTty, Endpoint, ObservedPaneId, ObservedPaneTty, ObservedPaneWorkspace,
            RequestedWorkspace,
        };

        match self {
            Self::AmbientTmuxEndpointUnavailable => &[RequestedWorkspace, Endpoint],
            Self::AmbientPaneIdUnavailable => &[RequestedWorkspace, ObservedPaneId, Endpoint],
            Self::AmbientPaneWorkspaceUnavailable => &[
                RequestedWorkspace,
                ObservedPaneId,
                ObservedPaneWorkspace,
                Endpoint,
            ],
            Self::CallerControllingTtyUnavailable => &[
                RequestedWorkspace,
                ObservedPaneId,
                Endpoint,
                CallerControllingTty,
            ],
            Self::ObservedPaneTtyUnavailable => &[
                RequestedWorkspace,
                ObservedPaneId,
                Endpoint,
                ObservedPaneTty,
            ],
            Self::PaneTtyMismatch => &[
                RequestedWorkspace,
                ObservedPaneId,
                Endpoint,
                CallerControllingTty,
                ObservedPaneTty,
            ],
            Self::PaneWorkspaceMismatch => &[
                RequestedWorkspace,
                ObservedPaneId,
                ObservedPaneWorkspace,
                Endpoint,
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaneAuthorityFactAvailability {
    Unavailable,
}

impl PaneAuthorityFactAvailability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnavailablePaneAuthorityFact<C> {
    pub cause: C,
}

impl<C> UnavailablePaneAuthorityFact<C> {
    pub const fn new(cause: C) -> Self {
        Self { cause }
    }

    pub const fn availability(&self) -> PaneAuthorityFactAvailability {
        PaneAuthorityFactAvailability::Unavailable
    }
}

macro_rules! unavailable_causes {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }
        }
    };
}

unavailable_causes!(AmbientTmuxEndpointUnavailableCause {
    TmuxValueNotUnicode => "tmux_value_not_unicode",
    TmuxTupleMalformed => "tmux_tuple_malformed",
    EndpointMissing => "endpoint_missing",
});

unavailable_causes!(AmbientPaneIdUnavailableCause {
    EnvironmentValueMissing => "environment_value_missing",
    EnvironmentValueEmpty => "environment_value_empty",
});

unavailable_causes!(AmbientPaneWorkspaceUnavailableCause {
    PaneQueryFailed => "pane_query_failed",
    CurrentPathMissing => "current_path_missing",
});

unavailable_causes!(CallerControllingTtyUnavailableCause {
    NoControllingTty => "no_controlling_tty",
    DeviceIdentityUnresolvable => "device_identity_unresolvable",
    PlatformUnsupported => "platform_unsupported",
});

unavailable_causes!(ObservedPaneTtyUnavailableCause {
    PaneTtyMissing => "pane_tty_missing",
    PaneTtyUnresolvable => "pane_tty_unresolvable",
    PaneTtyNotCharacterDevice => "pane_tty_not_character_device",
});

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AmbientTmuxEndpointUnavailableFacts {
    pub requested_workspace: PathBuf,
    pub endpoint: UnavailablePaneAuthorityFact<AmbientTmuxEndpointUnavailableCause>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AmbientPaneIdUnavailableFacts {
    pub requested_workspace: PathBuf,
    pub observed_pane_id: UnavailablePaneAuthorityFact<AmbientPaneIdUnavailableCause>,
    pub endpoint: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AmbientPaneWorkspaceUnavailableFacts {
    pub requested_workspace: PathBuf,
    pub observed_pane_id: String,
    pub observed_pane_workspace: UnavailablePaneAuthorityFact<AmbientPaneWorkspaceUnavailableCause>,
    pub endpoint: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallerControllingTtyUnavailableFacts {
    pub requested_workspace: PathBuf,
    pub observed_pane_id: String,
    pub endpoint: String,
    pub caller_controlling_tty: UnavailablePaneAuthorityFact<CallerControllingTtyUnavailableCause>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedPaneTtyUnavailableFacts {
    pub requested_workspace: PathBuf,
    pub observed_pane_id: String,
    pub endpoint: String,
    pub observed_pane_tty: UnavailablePaneAuthorityFact<ObservedPaneTtyUnavailableCause>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneTtyMismatchFacts {
    pub requested_workspace: PathBuf,
    pub observed_pane_id: String,
    pub endpoint: String,
    pub caller_controlling_tty: u64,
    pub observed_pane_tty: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneWorkspaceMismatchFacts {
    pub requested_workspace: PathBuf,
    pub observed_pane_id: String,
    pub observed_pane_workspace: PathBuf,
    pub endpoint: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PaneAuthorityRefusalFacts {
    AmbientTmuxEndpointUnavailable(AmbientTmuxEndpointUnavailableFacts),
    AmbientPaneIdUnavailable(AmbientPaneIdUnavailableFacts),
    AmbientPaneWorkspaceUnavailable(AmbientPaneWorkspaceUnavailableFacts),
    CallerControllingTtyUnavailable(CallerControllingTtyUnavailableFacts),
    ObservedPaneTtyUnavailable(ObservedPaneTtyUnavailableFacts),
    PaneTtyMismatch(PaneTtyMismatchFacts),
    PaneWorkspaceMismatch(PaneWorkspaceMismatchFacts),
}

impl PaneAuthorityRefusalFacts {
    pub const fn reason(&self) -> PaneAuthorityRefusalReason {
        match self {
            Self::AmbientTmuxEndpointUnavailable(_) => {
                PaneAuthorityRefusalReason::AmbientTmuxEndpointUnavailable
            }
            Self::AmbientPaneIdUnavailable(_) => {
                PaneAuthorityRefusalReason::AmbientPaneIdUnavailable
            }
            Self::AmbientPaneWorkspaceUnavailable(_) => {
                PaneAuthorityRefusalReason::AmbientPaneWorkspaceUnavailable
            }
            Self::CallerControllingTtyUnavailable(_) => {
                PaneAuthorityRefusalReason::CallerControllingTtyUnavailable
            }
            Self::ObservedPaneTtyUnavailable(_) => {
                PaneAuthorityRefusalReason::ObservedPaneTtyUnavailable
            }
            Self::PaneTtyMismatch(_) => PaneAuthorityRefusalReason::PaneTtyMismatch,
            Self::PaneWorkspaceMismatch(_) => PaneAuthorityRefusalReason::PaneWorkspaceMismatch,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaneAuthorityRecoveryHint {
    AttachLeader,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaneAuthorityRecoveryAction {
    OpenTerminalOutsideCurrentTmuxPaneOrAttachFromMatchingPane,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneAuthorityRecovery {
    pub action_required: bool,
    pub hint_action: PaneAuthorityRecoveryHint,
    pub action: PaneAuthorityRecoveryAction,
}

impl PaneAuthorityRecovery {
    pub const REQUIRED: Self = Self {
        action_required: true,
        hint_action: PaneAuthorityRecoveryHint::AttachLeader,
        action:
            PaneAuthorityRecoveryAction::OpenTerminalOutsideCurrentTmuxPaneOrAttachFromMatchingPane,
    };
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneAuthorityRefusal {
    pub facts: PaneAuthorityRefusalFacts,
    pub recovery: PaneAuthorityRecovery,
}

impl PaneAuthorityRefusal {
    pub const fn new(facts: PaneAuthorityRefusalFacts) -> Self {
        Self {
            facts,
            recovery: PaneAuthorityRecovery::REQUIRED,
        }
    }

    pub const fn reason(&self) -> PaneAuthorityRefusalReason {
        self.facts.reason()
    }
}

#[cfg(test)]
mod tests {
    use super::{PaneAuthorityRefusalField as Field, PaneAuthorityRefusalReason as Reason};

    #[test]
    fn reason_catalog_is_closed_and_mismatch_requires_all_four_workspace_facts() {
        assert_eq!(Reason::ALL.len(), 7);
        assert_eq!(
            Reason::PaneWorkspaceMismatch.required_fact_fields(),
            &[
                Field::RequestedWorkspace,
                Field::ObservedPaneId,
                Field::ObservedPaneWorkspace,
                Field::Endpoint,
            ]
        );
    }

    #[test]
    fn unavailable_reasons_require_the_fact_they_could_not_observe() {
        assert!(Reason::AmbientTmuxEndpointUnavailable
            .required_fact_fields()
            .contains(&Field::Endpoint));
        assert!(Reason::AmbientPaneIdUnavailable
            .required_fact_fields()
            .contains(&Field::ObservedPaneId));
        assert!(Reason::AmbientPaneWorkspaceUnavailable
            .required_fact_fields()
            .contains(&Field::ObservedPaneWorkspace));
    }
}
