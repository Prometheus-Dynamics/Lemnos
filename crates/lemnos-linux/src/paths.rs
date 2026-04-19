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

    pub fn gpio_class_root(&self) -> PathBuf {
        self.sys_class_root.join("gpio")
    }

    pub fn led_class_root(&self) -> PathBuf {
        self.sys_class_root.join("leds")
    }

    pub fn hwmon_class_root(&self) -> PathBuf {
        self.sys_class_root.join("hwmon")
    }

    pub fn gpio_line_root(&self, global_line: u32) -> PathBuf {
        self.gpio_class_root().join(format!("gpio{global_line}"))
    }

    pub fn gpio_export_path(&self) -> PathBuf {
        self.gpio_class_root().join("export")
    }

    pub fn gpio_unexport_path(&self) -> PathBuf {
        self.gpio_class_root().join("unexport")
    }

    pub fn i2c_class_root(&self) -> PathBuf {
        self.sys_class_root.join("i2c-dev")
    }

    pub fn tty_class_root(&self) -> PathBuf {
        self.sys_class_root.join("tty")
    }

    pub fn pwm_class_root(&self) -> PathBuf {
        self.sys_class_root.join("pwm")
    }

    pub fn pwm_chip_root(&self, chip_name: &str) -> PathBuf {
        self.pwm_class_root().join(chip_name)
    }

    pub fn pwm_channel_root(&self, chip_name: &str, channel: u32) -> PathBuf {
        self.pwm_chip_root(chip_name).join(format!("pwm{channel}"))
    }

    pub fn pwm_export_path(&self, chip_name: &str) -> PathBuf {
        self.pwm_chip_root(chip_name).join("export")
    }

    pub fn pwm_unexport_path(&self, chip_name: &str) -> PathBuf {
        self.pwm_chip_root(chip_name).join("unexport")
    }

    pub fn i2c_devices_root(&self) -> PathBuf {
        self.sys_bus_root.join("i2c").join("devices")
    }

    pub fn spi_devices_root(&self) -> PathBuf {
        self.sys_bus_root.join("spi").join("devices")
    }

    pub fn usb_devices_root(&self) -> PathBuf {
        self.sys_bus_root.join("usb").join("devices")
    }

    pub fn gpio_devnode(&self, chip_name: &str) -> PathBuf {
        self.dev_root.join(chip_name)
    }

    pub fn i2c_devnode(&self, bus: u32) -> PathBuf {
        self.dev_root.join(format!("i2c-{bus}"))
    }

    pub fn spi_devnode(&self, bus: u32, chip_select: u16) -> PathBuf {
        self.dev_root.join(format!("spidev{bus}.{chip_select}"))
    }

    pub fn tty_devnode(&self, port: &str) -> PathBuf {
        self.dev_root.join(port)
    }

    pub fn usb_bus_devnode(&self, bus: u16, device_number: u16) -> PathBuf {
        self.dev_root
            .join("bus")
            .join("usb")
            .join(format!("{bus:03}"))
            .join(format!("{device_number:03}"))
    }
}
