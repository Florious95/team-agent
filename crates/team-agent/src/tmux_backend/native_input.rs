//! Guarded legacy native-input effects. Shared contract parsers remain observation-only.

use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::*;
use crate::lifecycle::pane_input_lock::{acquire_required, PaneInputLockRequest};
use crate::model::enums::Provider;
use crate::transport::native_input::{inspect_input, InputFrame, NativeInputBinding};

#[cfg(test)]
#[path = "native_input_tests.rs"]
mod tests;


fn refused(reason: &str) -> TransportError {
    TransportError::NativeInputNotReady {
        reason: reason.to_string(),
    }
}

impl TmuxBackend {
    pub(super) fn observe_current_native_input(
        &self,
        target: &Target,
        provider: Provider,
    ) -> Result<Option<InputFrame>, TransportError> {
        if !crate::transport::native_input::supports_input_observation(provider) {
            return Ok(None);
        }
        // Pure observation: no nonce initialization, paste, or key.
        let binding = self.query_native_binding(target, true)?;
        let capture = self.capture(target, CaptureRange::Tail(200))?;
        let frame = inspect_input(provider, &capture.text, binding.pane_height)
            .ok_or_else(|| refused("native_surface_unknown"))?;
        if self.query_native_binding(target, true)? != binding {
            return Err(refused("binding_changed"));
        }
        Ok(Some(frame))
    }

    fn query_native_binding(
        &self,
        target: &Target,
        allow_unset_nonce: bool,
    ) -> Result<NativeInputBinding, TransportError> {
        let pane = pane_from_target(target);
        let argv = self.tmux_argv(&[
            "tmux".into(), "display-message".into(), "-p".into(), "-t".into(), pane.as_str().into(),
            "#{socket_path}|#{pane_id}|#{pane_pid}|#{session_name}|#{window_name}|#{@team_agent_pane_binding_nonce}|#{pane_in_mode}|#{pane_height}".into(),
        ]);
        let output = self.runner.run(&argv)?;
        if !output.success || output.stdout.lines().count() != 1 {
            return Err(refused("binding_query"));
        }
        let fields: Vec<_> = output.stdout.trim_end().split('|').collect();
        if fields.len() != 8 {
            return Err(refused("binding_invalid:field_count"));
        }
        for (invalid, field) in [
            (!std::path::Path::new(fields[0]).is_absolute(), "endpoint"),
            (fields[1] != pane.as_str(), "pane"),
            (fields[3].is_empty(), "session_empty"),
            (fields[4].is_empty(), "window_empty"),
            (fields[5].is_empty() && !allow_unset_nonce, "nonce_empty"),
            (fields[6] != "0", "pane_in_mode"),
        ] {
            if invalid {
                // Field class only: no argv, environment, business text or raw
                // native metadata is copied into the existing refusal event.
                return Err(refused(&format!("binding_invalid:{field}")));
            }
        }
        let pane_height = fields[7]
            .parse::<u32>()
            .map_err(|_| refused("binding_height"))?;
        if pane_height < 10 {
            return Err(refused("binding_height"));
        }
        let pid = fields[2]
            .parse::<u32>()
            .map_err(|_| refused("binding_pid"))?;
        let birth = team_agent_contract::host::process::process_birth_identity(pid)
            .map_err(|_| refused("carrier_birth"))?;
        Ok(NativeInputBinding {
            endpoint: fields[0].into(),
            pane_id: fields[1].into(),
            pane_pid: pid,
            pane_height,
            carrier_birth: birth,
            session: fields[3].into(),
            window: fields[4].into(),
            binding_nonce: fields[5].into(),
        })
    }

    // Called only under the required input lock. A newly launched legacy worker
    // has no Leader lease nonce producer: initialize its exact current carrier,
    // never overwrite an existing nonce or waive the subsequent strict reads.
}
