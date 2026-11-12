use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxPaths {
    pub sys_class_root: PathBuf,
    pub sys_bus_root: PathBuf,
    pub dev_root: PathBuf,
}

impl Default for LinuxPaths {
    fn default() -> Self {
        Self {
            sys_class_root: PathBuf::from("/sys/class"),
            sys_bus_root: PathBuf::from("/sys/bus"),
            dev_root: PathBuf::from("/dev"),
        }
    }
}

impl LinuxPaths {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_sys_class_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.sys_class_root = root.into();
        self
    }

    pub fn with_sys_bus_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.sys_bus_root = root.into();
        self
    }

    pub fn with_dev_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.dev_root = root.into();
        self
    }

    /// The sysfs mount point the class and bus roots live under (`/sys`).
    pub fn sys_root(&self) -> PathBuf {
        self.sys_class_root
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/sys"))
    }
}
