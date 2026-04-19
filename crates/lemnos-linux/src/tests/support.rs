#[path = "../../../../testing/support/linux_test_root.rs"]
mod linux_test_root;

use crate::LinuxPaths;
use linux_test_root::TempLinuxTestRoot;

pub(super) struct TestRoot {
    pub(super) root: TempLinuxTestRoot,
}

impl TestRoot {
    pub(super) fn new() -> Self {
        Self {
            root: TempLinuxTestRoot::new("lemnos-linux-tests"),
        }
    }

    pub(super) fn paths(&self) -> LinuxPaths {
        LinuxPaths::new()
            .with_sys_class_root(self.root.root().join("sys/class"))
            .with_sys_bus_root(self.root.root().join("sys/bus"))
            .with_dev_root(self.root.root().join("dev"))
    }

    pub(super) fn create_dir(&self, relative: impl AsRef<std::path::Path>) {
        self.root.create_dir(relative);
    }

    pub(super) fn write(&self, relative: impl AsRef<std::path::Path>, contents: &str) {
        self.root.write(relative, contents);
    }

    pub(super) fn touch(&self, relative: impl AsRef<std::path::Path>) {
        self.root.touch(relative);
    }
}

pub(super) fn create_watch_roots(root: &TestRoot) {
    root.create_dir("sys/class/gpio");
    root.create_dir("sys/class/leds");
    root.create_dir("sys/class/hwmon");
    root.create_dir("sys/class/pwm");
    root.create_dir("sys/class/i2c-dev");
    root.create_dir("sys/class/tty");
    root.create_dir("sys/bus/i2c/devices");
    root.create_dir("sys/bus/spi/devices");
    root.create_dir("sys/bus/usb/devices");
}
