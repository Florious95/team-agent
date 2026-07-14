struct CleanupFixture {
    child: ChildGuard,
    owned: TmuxServer,
    foreign: TmuxServer,
}

impl CleanupFixture {
    fn new(tag: &str) -> Self {
        Self {
            child: ChildGuard::spawn(),
            owned: TmuxServer::start(short_test_socket(&format!("owned-{tag}")), unique("owned")),
            foreign: TmuxServer::start(
                short_test_socket(&format!("foreign-{tag}")),
                unique("foreign"),
            ),
        }
    }

    fn assert_owned_gone_and_foreign_alive(&mut self) {
        assert!(
            !self.child.is_running(),
            "exact registered pid survived fixture Drop"
        );
        assert!(
            !self.owned.is_alive(),
            "exact registered tmux server survived fixture Drop"
        );
        assert!(
            !self.owned.socket().exists(),
            "owned tmux socket file survived Drop"
        );
        assert!(
            self.foreign.is_alive(),
            "foreign tmux server/session was touched"
        );
    }
}

struct ChildGuard {
    child: Child,
}

impl ChildGuard {
    fn spawn() -> Self {
        let child = Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn owned sleep");
        Self { child }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn is_running(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.child.try_wait().expect("poll owned child") {
                Some(_) => return false,
                None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                None => return true,
            }
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct TmuxServer {
    socket: PathBuf,
}

impl TmuxServer {
    fn start(socket: PathBuf, session: String) -> Self {
        let _ = fs::remove_file(&socket);
        let output = Command::new("tmux")
            .args([
                "-S",
                path_str(&socket),
                "new-session",
                "-d",
                "-s",
                &session,
                "sleep 60",
            ])
            .output()
            .expect("tmux new-session");
        assert_success(&output, "tmux new-session");
        Self { socket }
    }

    fn socket(&self) -> &Path {
        &self.socket
    }

    fn is_alive(&self) -> bool {
        Command::new("tmux")
            .args(["-S", path_str(&self.socket), "list-sessions"])
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn kill(&mut self) {
        let _ = Command::new("tmux")
            .args(["-S", path_str(&self.socket), "kill-server"])
            .output();
    }
}

impl Drop for TmuxServer {
    fn drop(&mut self) {
        self.kill();
        let _ = fs::remove_file(&self.socket);
    }
}

struct LineCountCase {
    root: PathBuf,
    sources: PathBuf,
}

impl LineCountCase {
    fn new(lines: usize) -> Self {
        let root = short_test_root("line-count");
        let sources = root.join("src");
        fs::create_dir_all(&sources).expect("create line-count fixture");
        fs::write(sources.join("fixture.rs"), "line\n".repeat(lines)).expect("write source");
        Self { root, sources }
    }

    fn write_allowlist(&self, json: &str) -> PathBuf {
        let path = self.root.join("allowlist.json");
        fs::write(&path, json).expect("write allowlist");
        path
    }

    fn run(&self, allowlist: Option<&Path>, require_empty: bool, hard: bool) -> Output {
        let mut command = Command::new("python3");
        command
            .arg(repo_root().join("tools/check_line_count_gate.py"))
            .args([
                "--root",
                path_str(&self.sources),
                "--glob",
                "*.rs",
                "--max-lines",
                "3",
            ]);
        if let Some(path) = allowlist {
            command.args(["--allowlist", path_str(path)]);
        }
        if require_empty {
            command.arg("--require-empty-temporary-debt");
        }
        if hard {
            command.arg("--hard");
        }
        command
            .output()
            .expect("run self-contained line-count gate")
    }
}

impl Drop for LineCountCase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

struct PathCleanup(PathBuf);

impl Drop for PathCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl EnvGuard {
    fn set(key: &'static str, value: &std::ffi::OsStr) -> Self {
        let previous = std::env::var_os(key);
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }

    fn unset(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        unsafe { std::env::remove_var(key) };
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = &self.previous {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

fn attributed_test_region<'a>(source: &'a str, needle: &str) -> &'a str {
    let function = source
        .find(needle)
        .unwrap_or_else(|| panic!("missing {needle}"));
    let start = source[..function].rfind("#[test]").expect("test attribute");
    let body = function_block_after(source, needle);
    let end = source[function..]
        .find(&body)
        .map(|offset| function + offset + body.len())
        .unwrap();
    &source[start..end]
}

fn function_block_after(source: &str, needle: &str) -> String {
    let start = source
        .find(needle)
        .unwrap_or_else(|| panic!("missing {needle}"));
    let open = source[start..]
        .find('{')
        .map(|n| start + n)
        .expect("opening brace");
    let mut depth = 0usize;
    for (offset, ch) in source[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return source[start..open + offset + 1].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("unterminated block after {needle}")
}

fn assert_short_non_default_socket(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        assert!(
            path.as_os_str().as_bytes().len() < 104,
            "socket too long: {}",
            path.display()
        );
    }
    assert!(
        path.is_absolute(),
        "test socket must be an explicit -S path"
    );
    assert_ne!(
        path.file_name().and_then(|name| name.to_str()),
        Some("default")
    );
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed: code={:?} stdout={} stderr={}",
        output.status.code(),
        text(&output.stdout),
        text(&output.stderr)
    );
}

fn assert_failure_contains(output: &Output, needle: &str, label: &str) {
    let combined = format!("{}\n{}", text(&output.stdout), text(&output.stderr));
    assert!(
        !output.status.success(),
        "{label} unexpectedly succeeded: {combined}"
    );
    assert!(
        combined.to_lowercase().contains(&needle.to_lowercase()),
        "{label}: missing {needle}; output={combined}"
    );
}

fn short_test_socket(tag: &str) -> PathBuf {
    short_test_base().join(format!("ta43-{tag}-{}-{}.sock", std::process::id(), next()))
}

fn short_test_root(tag: &str) -> PathBuf {
    short_test_base().join(format!("ta43-{tag}-{}-{}", std::process::id(), next()))
}

fn short_test_base() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/private/tmp")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        PathBuf::from("/tmp")
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir()
    }
}

fn unique(tag: &str) -> String {
    format!("ta43-{tag}-{}-{}", std::process::id(), next())
}

fn next() -> u64 {
    CASE_COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("UTF-8 test path")
}

fn normalize(value: &str) -> String {
    value
        .replace("::", "")
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == ':')
        .flat_map(char::to_lowercase)
        .collect()
}

fn read_repo(relative: &str) -> String {
    fs::read_to_string(repo_root().join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("team-agent crate under crates/")
        .to_path_buf()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}
