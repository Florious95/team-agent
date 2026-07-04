//! 0.5.x Windows-native transport Phase 1b: `windows-shim` binary.
//!
//! Long-lived per-workspace/team helper that owns ConPTY handles for
//! all workers under its team. The coordinator/CLI opens a named pipe
//! `\\.\pipe\team-agent-<workspace-hash>-<team-key>` and speaks the
//! 15-op protocol defined in `team_agent::conpty::protocol`. The shim
//! routes each request through `team_agent::conpty::shim::Shim` which
//! implements the protocol semantics against a swappable
//! `PaneRuntime` factory — here supplied by
//! `WindowsPaneRuntime` (ConPTY + child process + scrollback ring).
//!
//! On non-Windows hosts this file compiles to a no-op stub so the
//! workspace still builds on the developer Mac.

#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "windows-shim: this binary is Windows-only. On non-Windows hosts \
         it is a no-op stub so the workspace still builds."
    );
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    windows_shim::run()
}

#[cfg(windows)]
mod windows_shim {
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::mem;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use anyhow::{anyhow, Context, Result};
    use conpty_transport::protocol::{read_frame, write_frame, Request};
    use conpty_transport::shim::{PaneRuntime, Shim};
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Security::SECURITY_ATTRIBUTES;
    use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
    use windows::Win32::System::Console::{ClosePseudoConsole, CreatePseudoConsole, COORD, HPCON};
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_ACCESS_DUPLEX, PIPE_READMODE_BYTE,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
        InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
        WaitForSingleObject, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
        PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES,
        STARTUPINFOEXW, STARTUPINFOW,
    };

    /// CLI args (positional: --workspace-hash <hex> --team <key>
    /// --pipe-name <name>). The pipe token is generated in-memory here
    /// and printed to stderr so the launching coordinator can seed the
    /// `PipeClient` (CR C-1: never written to disk).
    struct Args {
        workspace_hash: String,
        team_key: String,
        pipe_name: String,
        pipe_token: String,
    }

    fn parse_args() -> Result<Args> {
        let mut argv = std::env::args().skip(1);
        let mut workspace_hash = None;
        let mut team_key = None;
        let mut pipe_name = None;
        let mut pipe_token = None;
        while let Some(a) = argv.next() {
            match a.as_str() {
                "--workspace-hash" => workspace_hash = argv.next(),
                "--team" => team_key = argv.next(),
                "--pipe-name" => pipe_name = argv.next(),
                "--pipe-token" => pipe_token = argv.next(),
                _ => {}
            }
        }
        Ok(Args {
            workspace_hash: workspace_hash
                .ok_or_else(|| anyhow!("--workspace-hash required"))?,
            team_key: team_key.ok_or_else(|| anyhow!("--team required"))?,
            pipe_name: pipe_name.ok_or_else(|| anyhow!("--pipe-name required"))?,
            pipe_token: pipe_token
                .unwrap_or_else(|| format!("tok-{:x}", std::process::id())),
        })
    }

    pub(super) fn run() -> Result<()> {
        let args = parse_args()?;
        eprintln!(
            "windows-shim: workspace_hash={} team={} pipe={} pid={} version={}",
            args.workspace_hash,
            args.team_key,
            args.pipe_name,
            std::process::id(),
            env!("CARGO_PKG_VERSION")
        );
        eprintln!("windows-shim: pipe_token={}", args.pipe_token);
        let shim = Arc::new(Shim::new(
            args.workspace_hash.clone(),
            args.team_key.clone(),
            std::process::id(),
            format!("windows-shim-{}", env!("CARGO_PKG_VERSION")),
            args.pipe_token.clone(),
            Box::new(|spawn| {
                Ok(Arc::new(WindowsPaneRuntime::new(spawn)?) as Arc<dyn PaneRuntime>)
            }),
        ));
        // Accept named-pipe connections and handle them. MVP: single
        // connection at a time (Phase 3 will multiplex).
        loop {
            let handle = create_named_pipe(&args.pipe_name)?;
            eprintln!("windows-shim: waiting for client on {}", args.pipe_name);
            unsafe {
                ConnectNamedPipe(handle, None).context("ConnectNamedPipe")?;
            }
            eprintln!("windows-shim: client connected");
            if let Err(e) = serve(handle, Arc::clone(&shim)) {
                eprintln!("windows-shim: connection error: {e:?}");
            }
            unsafe { CloseHandle(handle).ok() };
        }
    }

    fn create_named_pipe(name: &str) -> Result<HANDLE> {
        let mut wide: Vec<u16> = std::ffi::OsString::from(name)
            .encode_wide()
            .chain(Some(0))
            .collect();
        unsafe {
            let handle = CreateNamedPipeW(
                PWSTR(wide.as_mut_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                65536,
                65536,
                0,
                None,
            );
            if handle == INVALID_HANDLE_VALUE {
                return Err(anyhow!("CreateNamedPipeW failed for {name}"));
            }
            Ok(handle)
        }
    }

    fn serve(handle: HANDLE, shim: Arc<Shim>) -> Result<()> {
        let mut reader = PipeIo(handle);
        loop {
            let frame = match read_frame(&mut reader) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            };
            let req: Request = serde_json::from_slice(&frame)
                .context("parse request frame as JSON")?;
            let resp = shim.handle(&req);
            let resp_bytes = serde_json::to_vec(&resp).context("serialize response")?;
            let mut writer = PipeIo(handle);
            write_frame(&mut writer, &resp_bytes)?;
        }
        Ok(())
    }

    /// Thin std::io::{Read,Write} wrapper around a HANDLE so we can
    /// share the length-prefix framing code with the ConPtyBackend
    /// client side.
    struct PipeIo(HANDLE);

    impl std::io::Read for PipeIo {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let mut n_read: u32 = 0;
            unsafe {
                ReadFile(self.0, Some(buf), Some(&mut n_read), None)
                    .map_err(std::io::Error::other)?;
            }
            Ok(n_read as usize)
        }
    }

    impl std::io::Write for PipeIo {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut n_written: u32 = 0;
            unsafe {
                WriteFile(self.0, Some(buf), Some(&mut n_written), None)
                    .map_err(std::io::Error::other)?;
            }
            Ok(n_written as usize)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Real ConPTY-backed PaneRuntime (Windows only). Spawns the argv,
    /// keeps a background thread draining the child stdout into a
    /// bounded scrollback ring.
    struct WindowsPaneRuntime {
        hpcon: Mutex<Option<HPCON>>,
        input_write: Mutex<HANDLE>,
        proc_info: Mutex<Option<PROCESS_INFORMATION>>,
        scrollback: Arc<Mutex<VecDeque<u8>>>,
        alive: Arc<AtomicBool>,
    }

    // SAFETY: HANDLE is *mut c_void which is !Send by default, but we
    // guard access via Mutex and never dereference from multiple threads.
    unsafe impl Send for WindowsPaneRuntime {}
    unsafe impl Sync for WindowsPaneRuntime {}

    const SCROLLBACK_CAP: usize = 64 * 1024;

    impl WindowsPaneRuntime {
        fn new(spawn: &conpty_transport::protocol::SpawnRequest) -> Result<Self> {
            unsafe {
                // Two pipes.
                let mut sa = SECURITY_ATTRIBUTES {
                    nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: ptr::null_mut(),
                    bInheritHandle: true.into(),
                };
                let mut input_read: HANDLE = INVALID_HANDLE_VALUE;
                let mut input_write: HANDLE = INVALID_HANDLE_VALUE;
                windows::Win32::System::Pipes::CreatePipe(
                    &mut input_read,
                    &mut input_write,
                    Some(&mut sa),
                    0,
                )
                .context("CreatePipe(input)")?;
                let mut output_read: HANDLE = INVALID_HANDLE_VALUE;
                let mut output_write: HANDLE = INVALID_HANDLE_VALUE;
                windows::Win32::System::Pipes::CreatePipe(
                    &mut output_read,
                    &mut output_write,
                    Some(&mut sa),
                    0,
                )
                .context("CreatePipe(output)")?;
                let size = COORD {
                    X: spawn.cols as i16,
                    Y: spawn.rows as i16,
                };
                let hpcon =
                    CreatePseudoConsole(size, input_read, output_write, 0).context("ConPTY")?;
                CloseHandle(input_read).ok();
                CloseHandle(output_write).ok();
                // Startup attribute list.
                let mut attr_size: usize = 0;
                let _ = InitializeProcThreadAttributeList(None, 1, Some(0), &mut attr_size);
                let mut attr_buf = vec![0u8; attr_size];
                let attr_list =
                    LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut _);
                InitializeProcThreadAttributeList(Some(attr_list), 1, Some(0), &mut attr_size)
                    .context("InitializeProcThreadAttributeList")?;
                UpdateProcThreadAttribute(
                    attr_list,
                    0,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                    Some(hpcon.0 as *const _),
                    mem::size_of::<HPCON>(),
                    None,
                    None,
                )
                .context("UpdateProcThreadAttribute")?;
                let mut startup = STARTUPINFOEXW {
                    StartupInfo: STARTUPINFOW {
                        cb: mem::size_of::<STARTUPINFOEXW>() as u32,
                        dwFlags: STARTF_USESTDHANDLES,
                        ..Default::default()
                    },
                    lpAttributeList: attr_list,
                };
                // Build the command line. For MVP: quote+join argv[0]
                // as the exe name; keep argv[1..] appended.
                let cmd = if spawn.argv.is_empty() {
                    "cmd.exe".to_string()
                } else {
                    spawn.argv.join(" ")
                };
                let mut cmd_w: Vec<u16> = std::ffi::OsString::from(&cmd)
                    .encode_wide()
                    .chain(Some(0))
                    .collect();
                let mut proc_info = PROCESS_INFORMATION::default();
                CreateProcessW(
                    None,
                    Some(PWSTR(cmd_w.as_mut_ptr())),
                    None,
                    None,
                    false,
                    EXTENDED_STARTUPINFO_PRESENT,
                    None,
                    None,
                    &mut startup.StartupInfo,
                    &mut proc_info,
                )
                .context("CreateProcessW")?;
                mem::drop(attr_buf); // attr list no longer needed after CreateProcessW
                let scrollback = Arc::new(Mutex::new(VecDeque::with_capacity(SCROLLBACK_CAP)));
                let alive = Arc::new(AtomicBool::new(true));
                // Background reader thread. Drains child stdout into
                // the ring buffer until the pipe closes.
                let sb = Arc::clone(&scrollback);
                let al = Arc::clone(&alive);
                let output_read_raw = output_read.0 as isize;
                thread::spawn(move || {
                    let output_read = HANDLE(output_read_raw as *mut _);
                    let mut buf = [0u8; 4096];
                    loop {
                        let mut n_read: u32 = 0;
                        let ok = unsafe {
                            ReadFile(output_read, Some(&mut buf), Some(&mut n_read), None).is_ok()
                        };
                        if !ok || n_read == 0 {
                            al.store(false, Ordering::Relaxed);
                            break;
                        }
                        let mut sb_g = sb.lock().unwrap();
                        for &b in &buf[..n_read as usize] {
                            if sb_g.len() >= SCROLLBACK_CAP {
                                sb_g.pop_front();
                            }
                            sb_g.push_back(b);
                        }
                    }
                });
                Ok(Self {
                    hpcon: Mutex::new(Some(hpcon)),
                    input_write: Mutex::new(input_write),
                    proc_info: Mutex::new(Some(proc_info)),
                    scrollback,
                    alive,
                })
            }
        }
    }

    impl PaneRuntime for WindowsPaneRuntime {
        fn write_input(&self, bytes: &[u8]) -> Result<usize, String> {
            let handle = *self.input_write.lock().unwrap();
            let mut n_written: u32 = 0;
            unsafe {
                WriteFile(handle, Some(bytes), Some(&mut n_written), None)
                    .map_err(|e| e.to_string())?;
            }
            Ok(n_written as usize)
        }
        fn capture(&self, _range: &str) -> Result<String, String> {
            let sb = self.scrollback.lock().unwrap();
            let bytes: Vec<u8> = sb.iter().copied().collect();
            Ok(String::from_utf8_lossy(&bytes).to_string())
        }
        fn child_pid(&self) -> Option<u32> {
            self.proc_info.lock().unwrap().as_ref().map(|p| p.dwProcessId)
        }
        fn is_alive(&self) -> bool {
            self.alive.load(Ordering::Relaxed)
        }
        fn kill(&self) {
            if let Some(pi) = self.proc_info.lock().unwrap().take() {
                unsafe {
                    let _ = TerminateProcess(pi.hProcess, 1);
                    let _ = WaitForSingleObject(pi.hProcess, 2000);
                    CloseHandle(pi.hProcess).ok();
                    CloseHandle(pi.hThread).ok();
                }
            }
            if let Some(hpcon) = self.hpcon.lock().unwrap().take() {
                unsafe {
                    ClosePseudoConsole(hpcon);
                }
            }
            self.alive.store(false, Ordering::Relaxed);
        }
    }
}
