use super::super::support::{PTY_TEST_LOCK, TestRoot, usb_vendor_request};
use crate::{Runtime, RuntimeFailureCategory, RuntimeFailureOperation};
use lemnos_bus::BusError;
use lemnos_core::{
    DeviceRequest, GpioLevel, GpioRequest, I2cRequest, InteractionRequest, PwmConfiguration,
    PwmPolarity, PwmRequest, SpiRequest, StandardRequest, StandardResponse, UartConfiguration,
    UartDataBits, UartFlowControl, UartParity, UartRequest, UartStopBits, UsbRequest,
};
use lemnos_discovery::{DiscoveryContext, DiscoveryProbe};
use lemnos_driver_sdk::DriverError;
use lemnos_drivers_gpio::GpioDriver;
use lemnos_drivers_i2c::I2cDriver;
use lemnos_drivers_pwm::PwmDriver;
use lemnos_drivers_spi::SpiDriver;
use lemnos_drivers_uart::UartDriver;
use lemnos_drivers_usb::UsbDriver;
use lemnos_linux::LinuxBackend;
use serialport::{SerialPort, TTYPort};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::symlink;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

mod failures;
mod gpio_pwm;
mod uart;

struct SysfsGpioExportHarness {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl SysfsGpioExportHarness {
    fn new(
        root: &TestRoot,
        global_line: u32,
        direction: &'static str,
        value: &'static str,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let root_path = root.root.root().to_path_buf();
        let worker = thread::spawn(move || {
            let gpio_root = root_path.join("sys/class/gpio");
            let export_path = gpio_root.join("export");
            let unexport_path = gpio_root.join("unexport");
            let line_root = gpio_root.join(format!("gpio{global_line}"));

            while !worker_stop.load(Ordering::Relaxed) {
                if read_trimmed(&export_path).as_deref() == Some(&global_line.to_string())
                    && !line_root.exists()
                {
                    fs::create_dir_all(&line_root).expect("create exported gpio line");
                    fs::write(line_root.join("direction"), format!("{direction}\n"))
                        .expect("write gpio direction");
                    fs::write(line_root.join("value"), format!("{value}\n"))
                        .expect("write gpio value");
                    fs::write(line_root.join("active_low"), "0\n").expect("write gpio active_low");
                    fs::write(line_root.join("edge"), "none\n").expect("write gpio edge");
                    fs::write(&export_path, "").expect("clear gpio export file");
                }

                if read_trimmed(&unexport_path).as_deref() == Some(&global_line.to_string())
                    && line_root.exists()
                {
                    fs::remove_dir_all(&line_root).expect("remove exported gpio line");
                    fs::write(&unexport_path, "").expect("clear gpio unexport file");
                }

                thread::sleep(Duration::from_millis(5));
            }
        });

        Self {
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for SysfsGpioExportHarness {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join gpio sysfs harness");
        }
    }
}

struct SysfsPwmExportHarness {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl SysfsPwmExportHarness {
    fn new(
        root: &TestRoot,
        chip_name: &'static str,
        channel: u32,
        period_ns: u64,
        duty_cycle_ns: u64,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let root_path = root.root.root().to_path_buf();
        let worker = thread::spawn(move || {
            let chip_root = root_path.join("sys/class/pwm").join(chip_name);
            let export_path = chip_root.join("export");
            let unexport_path = chip_root.join("unexport");
            let channel_root = chip_root.join(format!("pwm{channel}"));

            while !worker_stop.load(Ordering::Relaxed) {
                if read_trimmed(&export_path).as_deref() == Some(&channel.to_string())
                    && !channel_root.exists()
                {
                    fs::create_dir_all(&channel_root).expect("create exported pwm channel");
                    fs::write(channel_root.join("period"), format!("{period_ns}\n"))
                        .expect("write pwm period");
                    fs::write(
                        channel_root.join("duty_cycle"),
                        format!("{duty_cycle_ns}\n"),
                    )
                    .expect("write pwm duty_cycle");
                    fs::write(channel_root.join("enable"), "0\n").expect("write pwm enable");
                    fs::write(channel_root.join("polarity"), "normal\n")
                        .expect("write pwm polarity");
                    fs::write(&export_path, "").expect("clear pwm export file");
                }

                if read_trimmed(&unexport_path).as_deref() == Some(&channel.to_string())
                    && channel_root.exists()
                {
                    fs::remove_dir_all(&channel_root).expect("remove exported pwm channel");
                    fs::write(&unexport_path, "").expect("clear pwm unexport file");
                }

                thread::sleep(Duration::from_millis(5));
            }
        });

        Self {
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for SysfsPwmExportHarness {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join pwm sysfs harness");
        }
    }
}

fn read_trimmed(path: &std::path::Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
}

fn wait_for_path_state(path: &std::path::Path, exists: bool, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if path.exists() == exists {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }

    panic!(
        "{label} did not become {} at '{}'",
        if exists { "present" } else { "absent" },
        path.display()
    );
}
