use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

pub struct TempLinuxTestRoot {
    root: PathBuf,
}

impl TempLinuxTestRoot {
    pub fn new(prefix: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos();
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("{prefix}-{}-{nonce}-{id}", std::process::id()));
        fs::create_dir_all(&root).expect("create temp test root");
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn join(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root.join(relative)
    }

    pub fn create_dir(&self, relative: impl AsRef<Path>) {
        fs::create_dir_all(self.root.join(relative)).expect("create test directory");
    }

    pub fn write(&self, relative: impl AsRef<Path>, contents: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directory");
        }
        fs::write(path, contents).expect("write test file");
    }

    pub fn touch(&self, relative: impl AsRef<Path>) {
        self.write(relative, "");
    }
}

impl Drop for TempLinuxTestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
