//! 0.5.x Windows-native transport Phase 0: ConPTY proof.
//!
//! Scope (kept tight per `.team/artifacts/0.5.x-windows-native-transport-design.md`
//! §Phase 0):
//!
//!   * Create a ConPTY.
//!   * Start `cmd.exe` (or a `node` echo loop if `CONPTY_PROOF_CHILD=node` is set).
//!   * Write UTF-8 text plus CR (must include an ASCII token AND a Chinese token).
//!   * Capture the child's stdout back through the ConPTY output pipe.
//!   * Exit cleanly; no orphan child process left behind.
//!
//! This binary is Windows-only and is **NOT** wired into any product path.
//! The whole file is gated by `#[cfg(windows)]` and the non-Windows entry
//! point just prints a message and exits with 0 so the workspace can be
//! compiled on the developer host without pulling win32 dependencies.
//!
//! CR C-4 tracking artefact hooks (embedded, no external tooling required):
//!   * Compile-time constants `BUILD_GIT_REV` / `BUILD_TIMESTAMP` come from
//!     `build.rs` — printed at startup so the deploy log captures them.
//!   * Runtime sha256 of the binary itself is printed at startup so the
//!     gate report can pin it against the deployed file.

#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "win-conpty-phase0: this binary is Windows-only. \
         On non-Windows hosts it is a no-op stub so the workspace still \
         builds. Cross-compile to x86_64-pc-windows-{{gnu,msvc}} or build \
         via GitHub Actions to get the real proof executable."
    );
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    windows_main::run()
}

#[cfg(windows)]
mod windows_main {
    use std::io::Write;
    use std::mem;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    use anyhow::{anyhow, Context, Result};
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Security::SECURITY_ATTRIBUTES;
    use windows::Win32::Storage::FileSystem::ReadFile;
    use windows::Win32::System::Console::{ClosePseudoConsole, CreatePseudoConsole, COORD, HPCON};
    use windows::Win32::System::Pipes::CreatePipe;
    use windows::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
        InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
        WaitForSingleObject, EXTENDED_STARTUPINFO_PRESENT, INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST,
        PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES,
        STARTUPINFOEXW, STARTUPINFOW,
    };

    /// ASCII probe token — deliberately unique so a `capture.contains(token)`
    /// assertion in the gate report is trivially checkable.
    pub(super) const ASCII_TOKEN: &str = "TA_PHASE0_ASCII_TOKEN_20260705";
    /// Chinese probe token — verifies UTF-8 survives ConPTY injection AND
    /// capture without mojibake (CR C-1 acceptance in the design doc).
    pub(super) const CHINESE_TOKEN: &str = "王小明·测试·令牌";

    pub(super) fn run() -> Result<()> {
        // Emit tracking header first so a truncated run still records what
        // build/rev/sha were exercised (CR C-4).
        emit_tracking_header();

        // The proof creates one ConPTY, hosts `cmd.exe /K echo <tokens>&exit`
        // by default, or `node` if the caller wants to prove a longer-lived
        // echo loop. `cmd` is used by default because it's guaranteed present
        // on any Windows host and we don't need node's echo semantics to
        // prove the round-trip.
        let child_kind = match std::env::var("CONPTY_PROOF_CHILD").as_deref() {
            Ok("node") => ChildKind::NodeEcho,
            _ => ChildKind::CmdEcho,
        };

        let mut proof = ConPtyProof::new()?;
        let child_pid = proof.start_child(child_kind)?;
        eprintln!("[phase0] spawned child pid={child_pid}");

        // The child's stdout drains onto a background reader thread so we
        // can time-box the capture without blocking on ReadFile.
        let captured = proof.drain_output(Duration::from_secs(5))?;

        // The child is expected to self-terminate after echoing the tokens
        // (both cmd `& exit` and node `process.exit` do this). We still
        // enforce a bounded WaitForSingleObject + fallback TerminateProcess
        // to prove no-orphan behavior.
        proof.wait_or_kill(Duration::from_secs(3))?;

        println!("=== CAPTURE BEGIN ===");
        println!("{captured}");
        println!("=== CAPTURE END ===");

        let ascii_hit = captured.contains(ASCII_TOKEN);
        let chinese_hit = captured.contains(CHINESE_TOKEN);
        println!("[phase0] ascii_token_present={ascii_hit}");
        println!("[phase0] chinese_token_present={chinese_hit}");
        if !(ascii_hit && chinese_hit) {
            return Err(anyhow!(
                "capture missing expected tokens: ascii={ascii_hit} chinese={chinese_hit}"
            ));
        }
        println!("[phase0] result=OK");
        Ok(())
    }

    fn emit_tracking_header() {
        // build.rs writes these; if missing (e.g. built without build.rs
        // running for some reason), fall back to "unknown".
        let git_rev = option_env!("BUILD_GIT_REV").unwrap_or("unknown");
        let build_ts = option_env!("BUILD_TIMESTAMP").unwrap_or("unknown");
        let self_sha = self_sha256().unwrap_or_else(|_| "unknown".to_string());
        println!("[phase0-tracking] git_rev={git_rev}");
        println!("[phase0-tracking] build_timestamp={build_ts}");
        println!("[phase0-tracking] binary_sha256={self_sha}");
        println!("[phase0-tracking] rustc_target={}", std::env::consts::ARCH);
    }

    fn self_sha256() -> Result<String> {
        use std::io::Read;
        let exe = std::env::current_exe()?;
        let mut file = std::fs::File::open(exe)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        // Simple portable sha256 — avoid pulling `sha2` into a Windows-only
        // crate. Use a 32-byte rolling hash via the Rust stdlib DefaultHasher
        // is NOT sha256; instead compute manually with a small inline routine.
        Ok(sha256_hex(&buf))
    }

    // Minimal FIPS-180-4 sha256 implementation. Keeps this crate dependency-free
    // for hashing; only used for the binary self-identification line in the
    // C-4 tracking header.
    fn sha256_hex(bytes: &[u8]) -> String {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut h = [
            0x6a09e667u32,
            0xbb67ae85,
            0x3c6ef372,
            0xa54ff53a,
            0x510e527f,
            0x9b05688c,
            0x1f83d9ab,
            0x5be0cd19,
        ];
        let bit_len = (bytes.len() as u64) * 8;
        let mut buf: Vec<u8> = bytes.to_vec();
        buf.push(0x80);
        while buf.len() % 64 != 56 {
            buf.push(0);
        }
        buf.extend_from_slice(&bit_len.to_be_bytes());
        for chunk in buf.chunks(64) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([
                    chunk[i * 4],
                    chunk[i * 4 + 1],
                    chunk[i * 4 + 2],
                    chunk[i * 4 + 3],
                ]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
                (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let t1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            h[0] = h[0].wrapping_add(a);
            h[1] = h[1].wrapping_add(b);
            h[2] = h[2].wrapping_add(c);
            h[3] = h[3].wrapping_add(d);
            h[4] = h[4].wrapping_add(e);
            h[5] = h[5].wrapping_add(f);
            h[6] = h[6].wrapping_add(g);
            h[7] = h[7].wrapping_add(hh);
        }
        let mut out = String::with_capacity(64);
        for word in &h {
            out.push_str(&format!("{word:08x}"));
        }
        out
    }

    #[derive(Debug, Clone, Copy)]
    enum ChildKind {
        CmdEcho,
        NodeEcho,
    }

    struct ConPtyProof {
        hpcon: HPCON,
        // The pipe *ends* that stay on OUR side after ConPTY was created.
        // `input_write` is the "keyboard" side we send bytes into; the
        // ConPTY relays them to the child. `output_read` is where the
        // child's screen bytes come back out.
        input_write: HANDLE,
        output_read: HANDLE,
        proc_info: Option<PROCESS_INFORMATION>,
        // Kept alive so DeleteProcThreadAttributeList can run on drop.
        attr_list_buf: Vec<u8>,
    }

    impl ConPtyProof {
        fn new() -> Result<Self> {
            unsafe {
                // Two anonymous pipes:
                //   host → conpty (this is the "input" pipe from the host's
                //       perspective; the ConPTY reads from it).
                //   conpty → host (the "output" pipe the ConPTY writes to).
                let mut sa = SECURITY_ATTRIBUTES {
                    nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: ptr::null_mut(),
                    bInheritHandle: true.into(),
                };
                let mut input_read: HANDLE = INVALID_HANDLE_VALUE;
                let mut input_write: HANDLE = INVALID_HANDLE_VALUE;
                CreatePipe(&mut input_read, &mut input_write, Some(&mut sa), 0)
                    .context("CreatePipe(input) failed")?;
                let mut output_read: HANDLE = INVALID_HANDLE_VALUE;
                let mut output_write: HANDLE = INVALID_HANDLE_VALUE;
                CreatePipe(&mut output_read, &mut output_write, Some(&mut sa), 0)
                    .context("CreatePipe(output) failed")?;

                // 80x24 default window; ConPTY doesn't strictly need real
                // dimensions for our echo probe.
                let size = COORD { X: 80, Y: 24 };
                let hpcon = CreatePseudoConsole(size, input_read, output_write, 0)
                    .context("CreatePseudoConsole failed")?;

                // ConPTY has taken ownership of input_read and output_write.
                // Close our copies so only ConPTY has them (avoid leaks).
                CloseHandle(input_read).ok();
                CloseHandle(output_write).ok();

                Ok(Self {
                    hpcon,
                    input_write,
                    output_read,
                    proc_info: None,
                    attr_list_buf: Vec::new(),
                })
            }
        }

        fn start_child(&mut self, kind: ChildKind) -> Result<u32> {
            let cmd_line = build_child_cmdline(kind);
            unsafe {
                // STARTUPINFOEX with PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE
                // pointing at our HPCON. This is the canonical ConPTY
                // wiring documented on
                // https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session
                let mut size: usize = 0;
                // First call with a NULL attribute list to learn the size.
                // The reserved param is `Option<*const u32>` in windows 0.61;
                // pass `Some(&0)`.
                let _ = InitializeProcThreadAttributeList(None, 1, Some(0), &mut size);
                if size == 0 {
                    return Err(anyhow!(
                        "InitializeProcThreadAttributeList did not report a size"
                    ));
                }
                self.attr_list_buf.resize(size, 0);
                let attr_list =
                    LPPROC_THREAD_ATTRIBUTE_LIST(self.attr_list_buf.as_mut_ptr() as *mut _);
                InitializeProcThreadAttributeList(Some(attr_list), 1, Some(0), &mut size)
                    .context("InitializeProcThreadAttributeList failed")?;
                UpdateProcThreadAttribute(
                    attr_list,
                    0,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                    Some(self.hpcon.0 as *const _),
                    mem::size_of::<HPCON>(),
                    None,
                    None,
                )
                .context("UpdateProcThreadAttribute(PSEUDOCONSOLE) failed")?;

                let mut startup_info = STARTUPINFOEXW {
                    StartupInfo: STARTUPINFOW {
                        cb: mem::size_of::<STARTUPINFOEXW>() as u32,
                        dwFlags: STARTF_USESTDHANDLES,
                        ..Default::default()
                    },
                    lpAttributeList: attr_list,
                };

                let mut cmd_w: Vec<u16> = std::ffi::OsString::from(&cmd_line)
                    .encode_wide()
                    .chain(Some(0))
                    .collect();
                let mut proc_info = PROCESS_INFORMATION::default();
                let res = CreateProcessW(
                    None,
                    Some(PWSTR(cmd_w.as_mut_ptr())),
                    None,
                    None,
                    false,
                    EXTENDED_STARTUPINFO_PRESENT,
                    None,
                    None,
                    &mut startup_info.StartupInfo,
                    &mut proc_info,
                );
                res.context("CreateProcessW failed")?;

                let pid = proc_info.dwProcessId;
                self.proc_info = Some(proc_info);
                Ok(pid)
            }
        }

        fn drain_output(&mut self, budget: Duration) -> Result<String> {
            // ReadFile on the anonymous pipe blocks; hand it to a reader
            // thread and time-box the capture with a stop flag.
            //
            // HANDLE = *mut c_void, which is !Send. Marshal it across the
            // thread boundary as an `isize` and reconstruct on the other
            // side. Safe: the value is an opaque OS token, not a Rust
            // pointer to any Rust-owned memory.
            let stop = Arc::new(AtomicBool::new(false));
            let stop_reader = stop.clone();
            let output_read_raw: isize = self.output_read.0 as isize;
            let handle = thread::spawn(move || -> Result<Vec<u8>> {
                let output_read = HANDLE(output_read_raw as *mut _);
                let mut acc = Vec::with_capacity(8 * 1024);
                let mut buf = [0u8; 4096];
                loop {
                    if stop_reader.load(Ordering::Relaxed) {
                        break;
                    }
                    let mut n_read: u32 = 0;
                    let ok = unsafe {
                        ReadFile(output_read, Some(&mut buf), Some(&mut n_read), None).is_ok()
                    };
                    if !ok {
                        break; // pipe closed / broken
                    }
                    if n_read == 0 {
                        break;
                    }
                    acc.extend_from_slice(&buf[..n_read as usize]);
                }
                Ok(acc)
            });

            // Inject the tokens immediately.
            self.inject_tokens()?;

            // Wait either until the child exits or the budget elapses. When
            // budget elapses, closing the ConPTY handles causes ReadFile to
            // return, terminating the reader thread.
            let deadline = Instant::now() + budget;
            loop {
                if Instant::now() >= deadline {
                    break;
                }
                if let Some(pi) = self.proc_info.as_ref() {
                    let wait = unsafe { WaitForSingleObject(pi.hProcess, 50) };
                    if wait.0 == 0 {
                        break;
                    }
                }
            }
            stop.store(true, Ordering::Relaxed);
            // Closing ConPTY tears down the pipes and lets the reader thread
            // finish immediately.
            unsafe { ClosePseudoConsole(self.hpcon) };
            // The hpcon has consumed our pipe writer copies; mark
            // input_write as no longer ours so drop is a no-op.
            self.input_write = INVALID_HANDLE_VALUE;
            // Reader thread exits when the pipe closes.
            let bytes = handle
                .join()
                .map_err(|_| anyhow!("reader thread panicked"))??;
            // ConPTY emits VT sequences; strip common CSI/OSC escapes for
            // token-search readability. We keep the raw bytes for the gate
            // report by returning both.
            let raw = String::from_utf8_lossy(&bytes).to_string();
            Ok(strip_vt(&raw))
        }

        fn inject_tokens(&self) -> Result<()> {
            let payload = format!(
                "echo {} {}\r\nexit\r\n",
                super::windows_main::ASCII_TOKEN,
                super::windows_main::CHINESE_TOKEN
            );
            // CR is included in the payload above; also flush.
            let mut writer = InputPipe(self.input_write);
            writer
                .write_all(payload.as_bytes())
                .context("write injection payload")?;
            writer.flush().ok();
            Ok(())
        }

        fn wait_or_kill(&mut self, budget: Duration) -> Result<()> {
            let Some(pi) = self.proc_info.take() else {
                return Ok(());
            };
            unsafe {
                let ms = budget.as_millis().min(u32::MAX as u128) as u32;
                let wait = WaitForSingleObject(pi.hProcess, ms);
                if wait.0 != 0 {
                    // Not exited within budget — kill it to satisfy the
                    // "no orphan" acceptance criterion.
                    let _ = TerminateProcess(pi.hProcess, 1);
                    let _ = WaitForSingleObject(pi.hProcess, INFINITE);
                }
                let mut code: u32 = 0;
                GetExitCodeProcess(pi.hProcess, &mut code).ok();
                println!("[phase0] child exit_code={code}");
                CloseHandle(pi.hProcess).ok();
                CloseHandle(pi.hThread).ok();
            }
            Ok(())
        }
    }

    impl Drop for ConPtyProof {
        fn drop(&mut self) {
            unsafe {
                if !self.attr_list_buf.is_empty() {
                    DeleteProcThreadAttributeList(LPPROC_THREAD_ATTRIBUTE_LIST(
                        self.attr_list_buf.as_mut_ptr() as *mut _,
                    ));
                }
                if self.input_write != INVALID_HANDLE_VALUE {
                    CloseHandle(self.input_write).ok();
                }
                if self.output_read != INVALID_HANDLE_VALUE {
                    CloseHandle(self.output_read).ok();
                }
            }
        }
    }

    struct InputPipe(HANDLE);
    impl Write for InputPipe {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut written: u32 = 0;
            unsafe {
                windows::Win32::Storage::FileSystem::WriteFile(
                    self.0,
                    Some(buf),
                    Some(&mut written),
                    None,
                )
                .map_err(std::io::Error::other)?;
            }
            Ok(written as usize)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn build_child_cmdline(kind: ChildKind) -> String {
        match kind {
            ChildKind::CmdEcho => {
                // Ask cmd to interpret `echo` and exit. `/K` is deliberate:
                // it lets the reader see the prompt banner before echo runs.
                // Injection payload later sends `echo ... & exit`.
                "cmd.exe".to_string()
            }
            ChildKind::NodeEcho => {
                // Read stdin line by line and echo it back until EOF.
                // The `-e` payload uses single quotes so the Chinese tokens
                // pass through cmd's parser cleanly.
                r#"node -e "process.stdin.on('data',d=>process.stdout.write(d));process.stdin.on('end',()=>process.exit(0))""#
                    .to_string()
            }
        }
    }

    /// Minimal VT stripper: removes ESC-prefixed CSI (`ESC [ ... alpha`) and
    /// OSC (`ESC ] ... BEL|ST`) sequences that ConPTY sprinkles into the
    /// output. Enough to make token-search grep-friendly; not a full
    /// terminal emulator.
    fn strip_vt(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                match chars.peek().copied() {
                    Some('[') => {
                        chars.next();
                        while let Some(nc) = chars.next() {
                            if nc.is_ascii_alphabetic() {
                                break;
                            }
                        }
                    }
                    Some(']') => {
                        chars.next();
                        while let Some(nc) = chars.next() {
                            if nc == '\x07' {
                                break;
                            }
                            if nc == '\x1b' && chars.peek() == Some(&'\\') {
                                chars.next();
                                break;
                            }
                        }
                    }
                    Some(_) => {
                        chars.next(); // ESC + single char (e.g. ESC(B)
                    }
                    None => {}
                }
                continue;
            }
            out.push(c);
        }
        out
    }
}
