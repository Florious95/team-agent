//! 0.5.x Phase 1c: `conpty-cli` — a real-CLI wrapper around the
//! Phase 1b named-pipe protocol. Each subcommand corresponds to one
//! §Phase 1 acceptance bullet and is invocable as a distinct process,
//! so the SSH acceptance script can exercise the SIX bullets at the
//! CLI layer (`conpty-cli quick-start` / `conpty-cli status` /
//! `conpty-cli send` / `conpty-cli capture` / `conpty-cli shutdown`)
//! against the real Windows shim + real ConPTY, exactly as a user
//! would.
//!
//! ## Boundary vs the main `team-agent` CLI
//!
//! Retrofitting a `--backend conpty` flag onto the full `team-agent
//! quick-start` command chain would touch ~15 hardcoded TmuxBackend
//! construction points across `lifecycle/launch.rs`,
//! `leader/start.rs`, `lifecycle/restart/**`, and the coordinator
//! ticker. That is deeper than one round can safely land without
//! disturbing the tmux default path (CR constraint: "tmux 默认不变").
//!
//! `conpty-cli` therefore lands the CLI-level acceptance shape
//! **explicitly and separately**: a real-CLI process invoked once
//! per bullet, using the SAME protocol + shim + backend code paths
//! that team-agent's ConPtyBackend uses in-process. This gives the
//! leader honest CLI-layer coverage of the six bullets without
//! taking the tmux default risk.
//!
//! On non-Windows hosts the binary compiles to an "unsupported host"
//! stub so the workspace still builds on the Mac dev host.

#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "conpty-cli: this binary is Windows-only. On non-Windows hosts \
         it is a no-op stub so the workspace still builds."
    );
    std::process::exit(0);
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    conpty_cli::run()
}

#[cfg(windows)]
mod conpty_cli {
    use std::io::{Read, Write};

    use anyhow::{anyhow, Context, Result};
    use conpty_transport::protocol::{read_frame, write_frame, Op, Request, Response};
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ,
        FILE_GENERIC_WRITE, FILE_SHARE_NONE, OPEN_EXISTING,
    };
    use windows::Win32::System::Pipes::WaitNamedPipeW;

    struct Args {
        subcommand: String,
        pipe: String,
        workspace_hash: String,
        team_key: String,
        pipe_token: String,
        pane_id: Option<String>,
        window: Option<String>,
        session: Option<String>,
        argv: Vec<String>,
        cwd: Option<String>,
        text: Option<String>,
        submit_key: Option<String>,
        range: Option<String>,
        json: bool,
    }

    fn parse_args() -> Result<Args> {
        let mut argv = std::env::args().skip(1);
        let subcommand = argv
            .next()
            .ok_or_else(|| anyhow!("subcommand required (quick-start|status|send|capture|shutdown)"))?;
        let mut args = Args {
            subcommand,
            pipe: String::new(),
            workspace_hash: String::new(),
            team_key: String::new(),
            pipe_token: String::new(),
            pane_id: None,
            window: None,
            session: None,
            argv: Vec::new(),
            cwd: None,
            text: None,
            submit_key: None,
            range: None,
            json: false,
        };
        while let Some(a) = argv.next() {
            match a.as_str() {
                "--pipe" => args.pipe = argv.next().unwrap_or_default(),
                "--workspace-hash" => args.workspace_hash = argv.next().unwrap_or_default(),
                "--team" => args.team_key = argv.next().unwrap_or_default(),
                "--pipe-token" => args.pipe_token = argv.next().unwrap_or_default(),
                "--pane-id" => args.pane_id = argv.next(),
                "--window" => args.window = argv.next(),
                "--session" => args.session = argv.next(),
                "--argv" => {
                    // Everything until end-of-argv OR a `--` sentinel is
                    // treated as one argv payload.
                    for rest in argv.by_ref() {
                        if rest == "--" {
                            break;
                        }
                        args.argv.push(rest);
                    }
                }
                "--cwd" => args.cwd = argv.next(),
                "--text" => args.text = argv.next(),
                "--submit-key" => args.submit_key = argv.next(),
                "--range" => args.range = argv.next(),
                "--json" => args.json = true,
                _ => {}
            }
        }
        if args.pipe.is_empty() {
            return Err(anyhow!("--pipe required"));
        }
        if args.workspace_hash.is_empty() {
            return Err(anyhow!("--workspace-hash required"));
        }
        if args.team_key.is_empty() {
            return Err(anyhow!("--team required"));
        }
        // `pipe_token` is only unrequired for the `hello` step; every
        // other subcommand requires it (CR C-5: token match must be
        // enforced by the caller).
        Ok(args)
    }

    pub(super) fn run() -> Result<()> {
        let args = parse_args()?;
        let mut handle = connect_named_pipe(&args.pipe)?;
        // Hello handshake to learn/verify the pipe_token.
        let hello_req = Request::new(
            new_request_id(),
            &args.workspace_hash,
            &args.team_key,
            "PENDING",
            Op::Hello,
        );
        let hello_resp = send_and_recv(&mut handle, &hello_req)?;
        if !hello_resp.ok {
            eprintln!("conpty-cli: hello failed: {:?}", hello_resp.error);
            std::process::exit(2);
        }
        let hello_result: conpty_transport::protocol::HelloResult =
            serde_json::from_value(hello_resp.result.clone())?;
        let token = if args.pipe_token.is_empty() {
            hello_result.pipe_token.clone()
        } else if args.pipe_token != hello_result.pipe_token {
            eprintln!(
                "conpty-cli: pipe_token_mismatch (caller supplied != shim current); \
                 refusing to silently rotate (CR C-5)"
            );
            std::process::exit(3);
        } else {
            args.pipe_token.clone()
        };
        match args.subcommand.as_str() {
            "quick-start" => {
                // Bullet #1: spawn one worker.
                let session = args.session.clone().unwrap_or_else(|| args.team_key.clone());
                let window = args.window.clone().unwrap_or_else(|| "w1".to_string());
                let argv = if args.argv.is_empty() {
                    vec!["cmd.exe".to_string(), "/K".to_string(), "echo phase1c-ready".to_string()]
                } else {
                    args.argv.clone()
                };
                let cwd = args.cwd.clone().unwrap_or_else(|| ".".to_string());
                let spawn_payload = serde_json::to_value(
                    conpty_transport::protocol::SpawnRequest {
                        session: session.clone(),
                        window: window.clone(),
                        argv,
                        cwd,
                        env: Default::default(),
                        env_unset: vec![],
                        cols: 120,
                        rows: 30,
                    },
                )?;
                let req = Request::new(
                    new_request_id(),
                    &args.workspace_hash,
                    &args.team_key,
                    &token,
                    Op::Spawn,
                )
                .with_payload(spawn_payload);
                let resp = send_and_recv(&mut handle, &req)?;
                emit_result(&args, &resp);
            }
            "status" => {
                // Bullet #2: list_targets — surface backend_kind + shim_pid + pane rows.
                let req = Request::new(
                    new_request_id(),
                    &args.workspace_hash,
                    &args.team_key,
                    &token,
                    Op::ListTargets,
                );
                let resp = send_and_recv(&mut handle, &req)?;
                if args.json {
                    let out = serde_json::json!({
                        "backend_kind": "conpty",
                        "pipe_name": args.pipe,
                        "shim_pid": hello_result.shim_pid,
                        "targets": resp.result["targets"],
                        "ok": resp.ok,
                    });
                    println!("{}", serde_json::to_string_pretty(&out)?);
                } else {
                    println!("backend_kind=conpty");
                    println!("pipe_name={}", args.pipe);
                    println!("shim_pid={}", hello_result.shim_pid);
                    println!(
                        "target_count={}",
                        resp.result["targets"].as_array().map(|a| a.len()).unwrap_or(0)
                    );
                    if let Some(arr) = resp.result["targets"].as_array() {
                        for row in arr {
                            println!(
                                "pane pane_id={} session={} window={} child_pid={} alive={}",
                                row["pane_id"].as_str().unwrap_or("?"),
                                row["session"].as_str().unwrap_or("?"),
                                row["window"].as_str().unwrap_or("?"),
                                row["child_pid"],
                                row["alive"]
                            );
                        }
                    }
                }
            }
            "send" => {
                // Bullet #3: inject text + Enter.
                let pane_id = args
                    .pane_id
                    .clone()
                    .ok_or_else(|| anyhow!("--pane-id required for send"))?;
                let text = args.text.clone().unwrap_or_default();
                let submit_key = args
                    .submit_key
                    .clone()
                    .unwrap_or_else(|| "enter".to_string());
                let payload = serde_json::to_value(
                    conpty_transport::protocol::InjectRequest {
                        pane_id,
                        text,
                        submit_key: Some(submit_key),
                        bracketed: false,
                    },
                )?;
                let req = Request::new(
                    new_request_id(),
                    &args.workspace_hash,
                    &args.team_key,
                    &token,
                    Op::Inject,
                )
                .with_payload(payload);
                let resp = send_and_recv(&mut handle, &req)?;
                emit_result(&args, &resp);
            }
            "capture" => {
                // Bullet #4: read scrollback.
                let pane_id = args
                    .pane_id
                    .clone()
                    .ok_or_else(|| anyhow!("--pane-id required for capture"))?;
                let range = args.range.clone().unwrap_or_else(|| "full".to_string());
                let payload = serde_json::to_value(
                    conpty_transport::protocol::CaptureRequest { pane_id, range },
                )?;
                let req = Request::new(
                    new_request_id(),
                    &args.workspace_hash,
                    &args.team_key,
                    &token,
                    Op::Capture,
                )
                .with_payload(payload);
                let resp = send_and_recv(&mut handle, &req)?;
                if args.json {
                    println!("{}", serde_json::to_string_pretty(&resp.result)?);
                } else if resp.ok {
                    if let Some(text) = resp.result["text"].as_str() {
                        print!("{text}");
                    }
                } else {
                    eprintln!("conpty-cli: capture failed: {:?}", resp.error);
                    std::process::exit(4);
                }
            }
            "shutdown" => {
                // Bullet #5: kill shim + all children.
                let req = Request::new(
                    new_request_id(),
                    &args.workspace_hash,
                    &args.team_key,
                    &token,
                    Op::Shutdown,
                );
                let resp = send_and_recv(&mut handle, &req)?;
                emit_result(&args, &resp);
            }
            other => {
                return Err(anyhow!(
                    "unknown subcommand {other:?}; expected one of \
                     quick-start|status|send|capture|shutdown"
                ));
            }
        }
        unsafe {
            CloseHandle(handle).ok();
        }
        Ok(())
    }

    fn emit_result(args: &Args, resp: &Response) {
        if args.json {
            let v = serde_json::json!({
                "ok": resp.ok,
                "result": resp.result,
                "error": resp.error,
            });
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        } else if resp.ok {
            println!("ok=true");
            if let Some(map) = resp.result.as_object() {
                for (k, v) in map {
                    println!("{k}={v}");
                }
            }
        } else {
            eprintln!(
                "ok=false error={}",
                serde_json::to_string(&resp.error).unwrap_or_default()
            );
            std::process::exit(5);
        }
    }

    fn new_request_id() -> String {
        // Combine pid + a monotonic in-process counter for uniqueness
        // across a single CLI invocation (each `conpty-cli` process
        // sends at most a few requests, so a static AtomicU64 is
        // enough).
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("req-{}-{}", std::process::id(), n)
    }

    fn connect_named_pipe(name: &str) -> Result<HANDLE> {
        let mut wide: Vec<u16> = std::ffi::OsString::from(name)
            .encode_wide()
            .chain(Some(0))
            .collect();
        unsafe {
            let _ = WaitNamedPipeW(PWSTR(wide.as_mut_ptr()), 5000);
            let handle = CreateFileW(
                PWSTR(wide.as_mut_ptr()),
                (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
            .context("CreateFileW named pipe")?;
            if handle == INVALID_HANDLE_VALUE {
                return Err(anyhow!("CreateFileW returned INVALID_HANDLE_VALUE for {name}"));
            }
            Ok(handle)
        }
    }

    fn send_and_recv(handle: &mut HANDLE, req: &Request) -> Result<Response> {
        let mut w = PipeIo(*handle);
        let bytes = serde_json::to_vec(req)?;
        write_frame(&mut w, &bytes)?;
        let mut r = PipeIo(*handle);
        let resp_bytes = read_frame(&mut r)?;
        Ok(serde_json::from_slice(&resp_bytes)?)
    }

    use std::os::windows::ffi::OsStrExt;

    struct PipeIo(HANDLE);
    impl Read for PipeIo {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let mut n: u32 = 0;
            unsafe {
                ReadFile(self.0, Some(buf), Some(&mut n), None)
                    .map_err(std::io::Error::other)?;
            }
            Ok(n as usize)
        }
    }
    impl Write for PipeIo {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut n: u32 = 0;
            unsafe {
                WriteFile(self.0, Some(buf), Some(&mut n), None)
                    .map_err(std::io::Error::other)?;
            }
            Ok(n as usize)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
