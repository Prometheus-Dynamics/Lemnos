//! The device table: every device of the board definition, built and
//! retried on a schedule, read on a schedule, with its status.

use lemnos_board::{Buses, DeviceSpec, DriverRegistry};
use lemnos_device::{BoxedDevice, DeviceInfo, DeviceStatus, MAX_CHANNELS, NO_VALUE};
use lemnos_drivers_linux::{FanRestore, RestoreKind};
use lemnos_hal::{ErrorKind, HalError};
use lemnos_ipc::{ChannelDesc, ControlDesc, DeviceDesc};

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
    /// Why the device failed last (the driver's or the board's own words;
    /// empty while it works).
    pub reason: String,
    pub values: [i32; MAX_CHANNELS],
    pub read_us: u64,
    pub fresh: bool,
    pub next_read_ms: u64,
    pub next_build_ms: u64,
    retry_ms: u64,
    pub subscriptions: Vec<Subscription>,
    /// For fans: how to hand the fan back to the kernel, worked out at the
    /// first bind and refreshed just before the first client write.
    pub restore: Option<FanRestore>,
    /// A client has written to this fan: the governor states in `restore`
    /// are the ones from before that.
    overriding: bool,
    /// Controls clients changed, who changed them last, and the value from
    /// before the first change.
    pub overrides: Vec<Override>,
}

/// A control a client changed.
#[derive(Debug, Clone)]
pub(crate) struct Override {
    pub control: usize,
    /// The connection that wrote last.
    pub holder: u32,
    pub name: String,
    /// The writer keeps its intents: not undone when it disconnects.
    pub keep: bool,
    /// The value before the first write (`None`: unreadable control).
    pub before: Option<i32>,
}

impl Slot {
    pub fn new(spec: DeviceSpec) -> Self {
        Self {
            spec,
            device: None,
            info: None,
            status: DeviceStatus::Missing,
            error: None,
            reason: String::new(),
            values: [NO_VALUE; MAX_CHANNELS],
            read_us: 0,
            fresh: false,
            next_read_ms: 0,
            next_build_ms: 0,
            retry_ms: RETRY_MIN_MS,
            subscriptions: Vec::new(),
            restore: None,
            overriding: false,
            overrides: Vec::new(),
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
        if status == DeviceStatus::Available {
            self.reason.clear();
        }
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
        let mut why = String::new();
        let result = match registry.build(&self.spec, buses) {
            Ok(mut device) => match device.init_why(delay, &mut why) {
                Ok(()) => Ok(device),
                Err(kind) => {
                    why.insert_str(0, "init: ");
                    Err(kind)
                }
            },
            Err(error) => {
                why = error.to_string();
                Err(error.kind())
            }
        };
        if result.is_err() {
            self.reason = why;
        }
        match result {
            Ok(device) => {
                self.info = Some(device.info());
                self.device = Some(device);
                self.retry_ms = RETRY_MIN_MS;
                self.next_read_ms = now_ms;
                if self.restore.is_none() {
                    self.restore = fan_restore_plan(&self.spec, buses);
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
        let mut why = String::new();
        match device.read_why(&mut self.values, &mut why) {
            Ok(()) => {
                self.read_us = now_us;
                self.fresh = true;
                self.set_status(DeviceStatus::Available, None)
            }
            Err(kind) => {
                self.reason = format!("read: {why}");
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
            reason: self.reason.clone(),
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
        self.spec.writers.is_empty()
            || self
                .spec
                .writers
                .iter()
                .any(|w| lemnos_board::client_matches(w, client))
    }

    /// Before a client's write: on the first one, records the governor's
    /// cooling states the hand-back restores (writing `pwm1` moves them).
    /// Returns whether the plan changed.
    pub fn before_write(&mut self) -> bool {
        if self.overriding {
            return false;
        }
        self.overriding = true;
        self.restore
            .as_mut()
            .is_some_and(|plan| plan.record_states().is_ok())
    }

    /// The hand-back to apply now. While no client is overriding the fan
    /// (never written, or released), the recorded governor states are
    /// stale: the governor owns the cooling device, so they are left out and
    /// only `pwm1_enable` and the governor kick remain.
    pub fn hand_back_plan(&self) -> Option<FanRestore> {
        let mut plan = self.restore.clone()?;
        if !self.overriding
            && let RestoreKind::CoolingDevice { devices, .. } = &mut plan.kind
        {
            for record in devices {
                record.state = None;
            }
        }
        Some(plan)
    }

    /// Hands a fan back to the kernel, if this is a fan.
    pub fn restore(&mut self) {
        if let Some(plan) = self.hand_back_plan() {
            let _ = plan.apply();
        }
    }

    /// Records that `who` is about to write control `control`: the first
    /// writer's snapshot of the old value stays; the holder becomes `who`.
    pub fn note_write(&mut self, control: usize, who: &crate::clients::Requester<'_>) {
        if let Some(existing) = self.overrides.iter_mut().find(|o| o.control == control) {
            existing.holder = who.id;
            existing.name = who.name.to_string();
            existing.keep = who.keep;
            return;
        }
        let before = self.device.as_mut().and_then(|d| d.get(control).ok());
        self.overrides.push(Override {
            control,
            holder: who.id,
            name: who.name.to_string(),
            keep: who.keep,
            before,
        });
    }

    /// Undoes the overrides `matches` selects: a fan goes back to the kernel
    /// (all of its overrides end), other controls get their old value.
    /// Returns whether anything was undone.
    pub fn revert(&mut self, matches: impl Fn(&Override) -> bool) -> bool {
        if !self.overrides.iter().any(&matches) {
            return false;
        }
        if self.restore.is_some() {
            let _ = self.release();
            self.overrides.clear();
            return true;
        }
        let (undo, keep): (Vec<Override>, Vec<Override>) = std::mem::take(&mut self.overrides)
            .into_iter()
            .partition(|o| matches(o));
        self.overrides = keep;
        if let Some(device) = self.device.as_mut() {
            for o in undo {
                if let Some(value) = o.before {
                    let _ = device.set(o.control, value);
                }
            }
        }
        true
    }

    /// Hands a fan back to the kernel's governor while the service keeps
    /// running. The next client write takes it back (recording the
    /// governor's states again first).
    pub fn release(&mut self) -> Result<(), ErrorKind> {
        if self.device.is_none() {
            return Err(self.error.unwrap_or(ErrorKind::Unavailable));
        }
        let plan = self.hand_back_plan().ok_or(ErrorKind::Unsupported)?;
        plan.apply().map_err(|e| lemnos_hal::HalError::kind(&e))?;
        self.overriding = false;
        self.overrides.clear();
        Ok(())
    }
}

/// A hwmon fan's hand-back plan, read at bind.
fn fan_restore_plan(spec: &DeviceSpec, buses: &mut dyn Buses) -> Option<FanRestore> {
    let sys = buses.sys();
    let fan = crate::fans::find_fan(spec, &sys)?;
    fan.restore_plan(&sys.thermal(), crate::fans::automatic_mode(spec))
        .ok()
}
