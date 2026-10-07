//! The device table: every device of the board definition, built and
//! retried on a schedule, read on a schedule, with its status.

use lemnos_board::{Buses, DeviceSpec, DriverRegistry};
use lemnos_device::{BoxedDevice, DeviceInfo, DeviceStatus, MAX_CHANNELS, NO_VALUE};
use lemnos_hal::{ErrorKind, HalError};
use lemnos_ipc::{ChannelDesc, ControlDesc, DeviceDesc};
use std::path::PathBuf;

/// The shortest and longest wait before rebuilding a device that failed.
const RETRY_MIN_MS: u64 = 1_000;
const RETRY_MAX_MS: u64 = 30_000;
/// How often a sensor is read when nothing asks for it more often.
const DEFAULT_POLL_MS: u32 = 1_000;

/// A client's subscription to a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Subscription {
    pub client: u32,
    pub period_ms: u32,
    pub next_ms: u64,
}

/// One device.
pub(crate) struct Slot {
    pub spec: DeviceSpec,
    pub device: Option<BoxedDevice>,
    pub info: Option<&'static DeviceInfo>,
    pub status: DeviceStatus,
    pub error: Option<ErrorKind>,
    pub values: [i32; MAX_CHANNELS],
    pub read_us: u64,
    pub fresh: bool,
    pub next_read_ms: u64,
    pub next_build_ms: u64,
    retry_ms: u64,
    pub subscriptions: Vec<Subscription>,
    /// For fans: the `pwm_mode` that hands the fan back to the kernel.
    pub restore_mode: Option<i32>,
    /// For fans: where that mode is written, for the panic hook.
    pub restore_path: Option<PathBuf>,
}

impl Slot {
    pub fn new(spec: DeviceSpec) -> Self {
        let restore_mode = spec
            .config
            .get("restore_mode")
            .and_then(lemnos_board::ConfigValue::as_i64)
            .and_then(|v| i32::try_from(v).ok());
        Self {
            spec,
            device: None,
            info: None,
            status: DeviceStatus::Missing,
            error: None,
            values: [NO_VALUE; MAX_CHANNELS],
            read_us: 0,
            fresh: false,
            next_read_ms: 0,
            next_build_ms: 0,
            retry_ms: RETRY_MIN_MS,
            subscriptions: Vec::new(),
            restore_mode,
            restore_path: None,
        }
    }

    pub fn id(&self) -> &str {
        &self.spec.id
    }

    /// How often to read: the fastest subscription, else the board's
    /// `poll_ms`.
    pub fn period_ms(&self) -> u32 {
        let board = self.spec.poll_ms.unwrap_or(DEFAULT_POLL_MS).max(1);
        self.subscriptions
            .iter()
            .map(|s| s.period_ms)
            .min()
            .map_or(board, |fastest| fastest.min(board))
            .max(1)
    }

    pub fn is_sensor(&self) -> bool {
        self.device.as_ref().is_some_and(BoxedDevice::is_sensor)
    }

    pub fn is_light(&self) -> bool {
        self.device.as_ref().is_some_and(BoxedDevice::is_light)
    }

    /// Records a status; returns the new status when it changed.
    pub fn set_status(
        &mut self,
        status: DeviceStatus,
        error: Option<ErrorKind>,
    ) -> Option<DeviceStatus> {
        self.error = error.or(if status == DeviceStatus::Available {
            None
        } else {
            self.error
        });
        (self.status != status).then(|| {
            self.status = status;
            status
        })
    }

    /// Builds and initializes the device if it is due; returns a status
    /// change.
    pub fn build(
        &mut self,
        registry: &DriverRegistry,
        buses: &mut dyn Buses,
        delay: &mut dyn embedded_hal::delay::DelayNs,
        now_ms: u64,
    ) -> Option<DeviceStatus> {
        if self.device.is_some() || now_ms < self.next_build_ms {
            return None;
        }
        let result = registry
            .build(&self.spec, buses)
            .map_err(|e| e.kind())
            .and_then(|mut device| {
                device.init(delay)?;
                Ok(device)
            });
        match result {
            Ok(device) => {
                self.info = Some(device.info());
                self.device = Some(device);
                self.retry_ms = RETRY_MIN_MS;
                self.next_read_ms = now_ms;
                if self.restore_mode.is_some() {
                    self.restore_path = fan_mode_path(&self.spec, buses);
                }
                self.set_status(DeviceStatus::Available, None)
            }
            Err(kind) => {
                self.next_build_ms = now_ms + self.retry_ms;
                self.retry_ms = (self.retry_ms * 2).min(RETRY_MAX_MS);
                let status = match DeviceStatus::after_error(kind) {
                    DeviceStatus::Degraded => DeviceStatus::Missing,
                    other => other,
                };
                self.set_status(status, Some(kind))
            }
        }
    }

    /// Reads the device if it is due; returns a status change.
    pub fn poll(&mut self, now_ms: u64, now_us: u64) -> Option<DeviceStatus> {
        if now_ms < self.next_read_ms || !self.is_sensor() {
            return None;
        }
        self.next_read_ms = now_ms + u64::from(self.period_ms());
        self.read(now_us)
    }

    /// Reads the device now; returns a status change.
    pub fn read(&mut self, now_us: u64) -> Option<DeviceStatus> {
        let device = self.device.as_mut()?;
        match device.read(&mut self.values) {
            Ok(()) => {
                self.read_us = now_us;
                self.fresh = true;
                self.set_status(DeviceStatus::Available, None)
            }
            Err(kind) => {
                let status = DeviceStatus::after_error(kind);
                if status == DeviceStatus::Missing {
                    // Gone: rebuild it on the retry schedule.
                    self.device = None;
                    self.fresh = false;
                    self.next_build_ms = now_us / 1000 + self.retry_ms;
                }
                self.set_status(status, Some(kind))
            }
        }
    }

    /// The device as clients see it.
    pub fn describe(&self) -> DeviceDesc {
        let info = self.info;
        DeviceDesc {
            id: self.spec.id.clone(),
            label: self
                .spec
                .label
                .clone()
                .unwrap_or_else(|| self.spec.id.clone()),
            class: info.map_or(lemnos_device::DeviceClass::Other, |i| i.class),
            model: info.map_or_else(|| self.spec.driver.clone(), |i| i.model.to_string()),
            status: self.status,
            channels: info.map_or_else(Vec::new, |i| {
                i.channels
                    .iter()
                    .map(|c| ChannelDesc {
                        name: c.name.into(),
                        quantity: c.quantity,
                        axis: c.axis,
                        exponent: c.exponent,
                    })
                    .collect()
            }),
            controls: info.map_or_else(Vec::new, |i| {
                i.controls
                    .iter()
                    .map(|c| ControlDesc {
                        name: c.name.into(),
                        quantity: c.quantity,
                        exponent: c.exponent,
                        min: c.min,
                        max: c.max,
                    })
                    .collect()
            }),
            pixels: self
                .device
                .as_ref()
                .map_or(0, |d| u16::try_from(d.pixel_count()).unwrap_or(u16::MAX)),
        }
    }

    /// The channel values read last.
    pub fn channel_values(&self) -> Vec<i32> {
        let n = self.info.map_or(0, |i| i.channels.len());
        self.values[..n].to_vec()
    }

    /// Whether `client` may write this device's controls.
    pub fn allows(&self, client: &str) -> bool {
        self.spec.writers.is_empty() || self.spec.writers.iter().any(|w| w == client)
    }

    /// Hands a fan back to the kernel (`restore_mode`), if configured.
    pub fn restore(&mut self) {
        if let (Some(mode), Some(device)) = (self.restore_mode, self.device.as_mut())
            && let Some(index) = device.info().control_index("pwm_mode")
        {
            let _ = device.set(index, mode);
        }
    }
}

/// The `pwm1_enable` file of a hwmon fan, for the panic hook.
fn fan_mode_path(spec: &DeviceSpec, buses: &mut dyn Buses) -> Option<PathBuf> {
    if spec.driver != "hwmon-fan" {
        return None;
    }
    let root = match &spec.path {
        Some(path) => PathBuf::from(path),
        None => lemnos_drivers_linux::HwmonFan::find(
            &buses.sys().hwmon(),
            spec.matches.get("name").map(String::as_str),
        )
        .ok()
        .flatten()?
        .root()
        .to_path_buf(),
    };
    Some(root.join("pwm1_enable"))
}
