use std::collections::BTreeSet;
use std::ffi::OsStr;

use crate::contract::plan::LaunchPlan;
use super::{HostError, HostErrorKind};

/// Byte-preserving POSIX single quoting. This is not JSON/YAML or a provider argv builder.
/// The legacy helper is private/provider-coupled; no old module is imported or changed.
pub fn quote(value: &OsStr) -> Result<Vec<u8>, HostError> {
    let bytes = value.as_encoded_bytes();
    if bytes.contains(&0) { return Err(HostError::new("shell NUL", HostErrorKind::Invalid)); }
    let mut output = Vec::with_capacity(bytes.len() + 2);
    output.push(b'\'');
    for byte in bytes {
        if *byte == b'\'' { output.extend_from_slice(b"'\\''"); } else { output.push(*byte); }
    }
    output.push(b'\'');
    Ok(output)
}

/// The owned tmux pane execs the native program: pane_pid becomes the real native PID.
/// remain-on-exit is set in the private server's config before spawn; the tmux server,
/// not screen text or a second shell, owns wait/exit facts. Dead panes never take input.
pub fn launch_script(plan: &LaunchPlan, inherited_identity_keys: &BTreeSet<String>) -> Result<Vec<u8>, HostError> {
    plan.environment.validate().map_err(|_| HostError::new("launch environment", HostErrorKind::Invalid))?;
    let mut script = b"#!/bin/sh\numask 077\ncd ".to_vec();
    script.extend(quote(plan.cwd.as_os_str())?);
    script.extend_from_slice(b" || exit 126\n");
    if inherited_identity_keys.iter().any(|key| !crate::contract::types::valid_env_key(key)) {
        return Err(HostError::new("inherited identity key", HostErrorKind::Invalid));
    }
    // Clear inherited framework identities first; only the explicit captured overlay wins.
    for key in plan.environment.remove.union(inherited_identity_keys) {
        script.extend_from_slice(b"unset "); script.extend_from_slice(key.as_bytes()); script.push(b'\n');
    }
    for (key, value) in &plan.environment.set {
        script.extend_from_slice(b"export "); script.extend_from_slice(key.as_bytes()); script.push(b'=');
        script.extend(quote(value)?); script.push(b'\n');
    }
    script.extend_from_slice(b"exec ");
    script.extend(quote(plan.executable.as_os_str())?);
    for argument in &plan.arguments { script.push(b' '); script.extend(quote(argument)?); }
    script.push(b'\n');
    Ok(script)
}
