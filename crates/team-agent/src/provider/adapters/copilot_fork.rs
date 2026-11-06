//! Resolve the native Copilot home directory.
use std::path::PathBuf;

use crate::provider::ProviderError;





pub(crate) fn copilot_home() -> Result<PathBuf, ProviderError> {
    if let Some(home) = std::env::var_os("COPILOT_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home));
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".copilot"))
        .ok_or_else(|| ProviderError::Io("HOME is unset; cannot resolve COPILOT_HOME".to_string()))
}
