//! Windows named-pipe `PipeClient` implementation.
//!
//! ## Batch 5 role
//!
//! Batch 5 wires this into `team-agent::transport_factory` on Windows
//! so `ConPtyBackend` can talk to a live shim binary instead of always
//! degrading to `MuxUnavailable`.
//!
//! ## Layer split
//!
//! - `LocalShimClient` (in `shim.rs`) — in-process client; the shim
//!   handler runs in the same address space. Used by portable tests
//!   and the Mac/Linux fake-worker path.
//! - `NamedPipeClient` (this module, Windows-only) — real Windows
//!   named-pipe client using `CreateFileW`/`WaitNamedPipeW`/
//!   `ReadFile`/`WriteFile` via the `windows = "0.61"` crate. Same
//!   technique already exercised by `win-conpty-phase0::conpty_cli`
//!   binary; extracted here so the factory can wire it.
//!
//! ## Non-Windows behavior
//!
//! `NamedPipeClient` is `#[cfg(windows)]`. `team-agent::transport_factory`
//! reaches for it only inside its own cfg(windows) block, so
//! non-Windows callers see the ConPTY factory return
//! `ConPtyBackend::new(...)` without a client — matching pre-Batch-5
//! behavior byte-for-byte on Unix.
//!
//! `pipe_name_for` is portable — needed by callers on any platform
//! to derive the pipe name the shim binary will listen on.

/// Derive the pipe name from `(workspace_hash, team_key)`. This is
/// the convention the shim binary is expected to be started with:
/// `\\.\pipe\team-agent-conpty-<workspace_hash>-<team_key>`.
///
/// Kept as a free fn so both the factory (caller side) and any
/// shim-spawning helper (server side) derive the same string from
/// the same inputs. Portable across platforms so tests can build the
/// same string without needing a Windows target.
pub fn pipe_name_for(workspace_hash: &str, team_key: &str) -> String {
    format!(
        r"\\.\pipe\team-agent-conpty-{workspace_hash}-{team_key}"
    )
}

#[cfg(windows)]
mod imp {
    use crate::protocol::{read_frame, write_frame, Request, Response};
    use crate::shim::PipeClient;
    use std::io::{self, Read, Write};
    use std::os::windows::ffi::OsStrExt;

    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ,
        FILE_GENERIC_WRITE, FILE_SHARE_NONE, OPEN_EXISTING,
    };
    use windows::Win32::System::Pipes::WaitNamedPipeW;

    /// Client for the Windows-shim's named pipe.
    ///
    /// Owns the pipe handle for the lifetime of the client. Requests
    /// are framed by the shared `protocol::write_frame`/`read_frame`
    /// helpers so the client stays wire-compatible with the shim
    /// server and the in-process `LocalShimClient`.
    pub struct NamedPipeClient {
        pipe_name: String,
        handle: std::sync::Mutex<HANDLE>,
    }

    // SAFETY: Windows HANDLE is Send-safe as long as we serialize
    // access (we do via the Mutex). Sync is provided by the Mutex.
    unsafe impl Send for NamedPipeClient {}
    unsafe impl Sync for NamedPipeClient {}

    impl NamedPipeClient {
        /// Connect to the shim's named pipe. Blocks up to `wait_ms`
        /// milliseconds for the pipe to become available
        /// (`WaitNamedPipeW` semantics), then opens it with
        /// `CreateFileW`.
        pub fn connect(pipe_name: impl Into<String>, wait_ms: u32) -> io::Result<Self> {
            let pipe_name = pipe_name.into();
            let mut wide: Vec<u16> = std::ffi::OsString::from(&pipe_name)
                .encode_wide()
                .chain(Some(0))
                .collect();
            let handle = unsafe {
                // Wait for the pipe (best-effort — proceed even on
                // timeout, then let CreateFileW surface the real
                // error).
                let _ = WaitNamedPipeW(PWSTR(wide.as_mut_ptr()), wait_ms);
                let handle = CreateFileW(
                    PWSTR(wide.as_mut_ptr()),
                    (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
                .map_err(|e| io::Error::from_raw_os_error(e.code().0 as i32))?;
                if handle == INVALID_HANDLE_VALUE {
                    return Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        format!(
                            "CreateFileW returned INVALID_HANDLE_VALUE for {pipe_name}"
                        ),
                    ));
                }
                handle
            };
            Ok(Self {
                pipe_name,
                handle: std::sync::Mutex::new(handle),
            })
        }

        /// The pipe name this client is connected to. Useful for
        /// diagnostic detail in error paths.
        pub fn pipe_name(&self) -> &str {
            &self.pipe_name
        }
    }

    impl Drop for NamedPipeClient {
        fn drop(&mut self) {
            if let Ok(handle) = self.handle.lock() {
                if *handle != INVALID_HANDLE_VALUE {
                    unsafe {
                        let _ = CloseHandle(*handle);
                    }
                }
            }
        }
    }

    impl PipeClient for NamedPipeClient {
        fn request(&self, req: &Request) -> Response {
            // Serialize concurrent callers on the shared handle so a
            // response never gets interleaved with another request's
            // framing.
            let mut handle_guard = self
                .handle
                .lock()
                .expect("named pipe handle mutex poisoned");
            let handle = *handle_guard;
            let mut io = PipeIo(handle);
            let bytes = match serde_json::to_vec(req) {
                Ok(b) => b,
                Err(e) => return protocol_error_response(&format!("encode: {e}")),
            };
            if let Err(e) = write_frame(&mut io, &bytes) {
                *handle_guard = INVALID_HANDLE_VALUE;
                return protocol_error_response(&format!("write_frame: {e}"));
            }
            let resp_bytes = match read_frame(&mut io) {
                Ok(b) => b,
                Err(e) => {
                    *handle_guard = INVALID_HANDLE_VALUE;
                    return protocol_error_response(&format!("read_frame: {e}"));
                }
            };
            match serde_json::from_slice::<Response>(&resp_bytes) {
                Ok(r) => r,
                Err(e) => protocol_error_response(&format!("decode: {e}")),
            }
        }
    }

    fn protocol_error_response(reason: &str) -> Response {
        // Use the `TargetNotFound` variant with a descriptive message
        // so `ConPtyBackend` maps the response to a typed
        // `TransportError::MuxUnavailable` per CR C-1. We avoid the
        // more specific variants (`PipeTokenMismatch`, `SchemaSkew`)
        // because those carry additional semantic meaning callers
        // key on. A generic transport-layer I/O failure is more
        // honest surfacing as "the named target is unreachable".
        Response::err(
            String::new(),
            crate::protocol::ProtocolError::TargetNotFound {
                message: format!("named_pipe_io: {reason}"),
            },
        )
    }

    struct PipeIo(HANDLE);

    impl Read for PipeIo {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut n: u32 = 0;
            unsafe {
                ReadFile(self.0, Some(buf), Some(&mut n), None)
                    .map_err(|e| io::Error::from_raw_os_error(e.code().0 as i32))?;
            }
            Ok(n as usize)
        }
    }

    impl Write for PipeIo {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut n: u32 = 0;
            unsafe {
                WriteFile(self.0, Some(buf), Some(&mut n), None)
                    .map_err(|e| io::Error::from_raw_os_error(e.code().0 as i32))?;
            }
            Ok(n as usize)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}

#[cfg(windows)]
pub use imp::NamedPipeClient;

#[cfg(test)]
mod tests {
    use super::pipe_name_for;

    #[test]
    fn pipe_name_convention_uses_named_pipe_prefix_and_composite_key() {
        let n = pipe_name_for("wshash", "team-a");
        assert!(n.starts_with(r"\\.\pipe\"));
        assert!(n.contains("wshash"));
        assert!(n.contains("team-a"));
    }
}
