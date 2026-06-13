use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_WORKSPACE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct TempWorkspace {
    path: PathBuf,
}

impl TempWorkspace {
    pub fn new(prefix: &str, tag: &str) -> Self {
        let tmp_root = if Path::new("/private/tmp").is_dir() {
            PathBuf::from("/private/tmp")
        } else {
            std::env::temp_dir()
        };
        let path = tmp_root.join(format!(
            "{prefix}-{tag}-{}-{}",
            std::process::id(),
            TEMP_WORKSPACE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        let path = std::fs::canonicalize(path).unwrap();
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn to_path_buf(&self) -> PathBuf {
        self.path.clone()
    }

    pub fn keep_enabled() -> bool {
        std::env::var("TEAM_AGENT_KEEP_TEST_TMP").as_deref() == Ok("1")
    }
}

impl AsRef<Path> for TempWorkspace {
    fn as_ref(&self) -> &Path {
        self.path()
    }
}

impl Deref for TempWorkspace {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.path()
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        if !Self::keep_enabled() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
