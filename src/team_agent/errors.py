class TeamAgentError(Exception):
    """Base exception for user-facing Team Agent errors."""


class ValidationError(TeamAgentError):
    """Spec or result envelope validation failed."""


class RuntimeError(TeamAgentError):
    """Runtime operation failed."""
