//! Expected fact identities for the public pane-authority contracts.

use team_agent::model::pane_authority_refusal::{
    PaneAuthorityRefusalField, PaneAuthorityRefusalReason as Reason,
};

pub const fn every_reason() -> [Reason; 7] {
    [
        Reason::AmbientTmuxEndpointUnavailable,
        Reason::AmbientPaneIdUnavailable,
        Reason::AmbientPaneWorkspaceUnavailable,
        Reason::CallerControllingTtyUnavailable,
        Reason::ObservedPaneTtyUnavailable,
        Reason::PaneTtyMismatch,
        Reason::PaneWorkspaceMismatch,
    ]
}

pub const fn expected_fact_fields(reason: Reason) -> &'static [PaneAuthorityRefusalField] {
    use PaneAuthorityRefusalField::{
        CallerControllingTty, Endpoint, ObservedPaneId, ObservedPaneTty, ObservedPaneWorkspace,
        RequestedWorkspace,
    };

    match reason {
        Reason::AmbientTmuxEndpointUnavailable => &[RequestedWorkspace, Endpoint],
        Reason::AmbientPaneIdUnavailable => &[RequestedWorkspace, ObservedPaneId, Endpoint],
        Reason::AmbientPaneWorkspaceUnavailable => &[
            RequestedWorkspace,
            ObservedPaneId,
            ObservedPaneWorkspace,
            Endpoint,
        ],
        Reason::CallerControllingTtyUnavailable => &[
            RequestedWorkspace,
            ObservedPaneId,
            Endpoint,
            CallerControllingTty,
        ],
        Reason::ObservedPaneTtyUnavailable => &[
            RequestedWorkspace,
            ObservedPaneId,
            Endpoint,
            ObservedPaneTty,
        ],
        Reason::PaneTtyMismatch => &[
            RequestedWorkspace,
            ObservedPaneId,
            Endpoint,
            CallerControllingTty,
            ObservedPaneTty,
        ],
        Reason::PaneWorkspaceMismatch => &[
            RequestedWorkspace,
            ObservedPaneId,
            ObservedPaneWorkspace,
            Endpoint,
        ],
    }
}
