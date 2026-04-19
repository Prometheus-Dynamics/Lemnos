use lemnos::linux::LinuxPaths;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_TEST_ROOT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct ExampleLinuxTestRoot {
    root: PathBuf,
}

impl ExampleLinuxTestRoot {
    pub fn new(prefix: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos();
        let id = NEXT_TEST_ROOT_ID.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("{prefix}-{}-{nonce}-{id}", std::process::id()));
        fs::create_dir_all(&root).expect("create temp root");
        Self { root }
    }

    pub fn paths(&self) -> LinuxPaths {
        LinuxPaths::new()
            .with_sys_class_root(self.root.join("sys/class"))
            .with_sys_bus_root(self.root.join("sys/bus"))
            .with_dev_root(self.root.join("dev"))
    }

    pub fn root_path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root.join(relative)
    }

    pub fn read(&self, relative: impl AsRef<Path>) -> String {
        fs::read_to_string(self.root.join(relative)).expect("read test file")
    }

    pub fn create_dir(&self, relative: impl AsRef<Path>) {
        fs::create_dir_all(self.root.join(relative)).expect("create test directory");
    }

    pub fn write(&self, relative: impl AsRef<Path>, contents: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write test file");
    }
}

impl Drop for ExampleLinuxTestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
