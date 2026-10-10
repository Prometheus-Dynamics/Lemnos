//! The device table: every device of the board definition, built and
//! retried on a schedule, read on a schedule, with its status.

use lemnos_board::{Buses, DeviceSpec, DriverRegistry};
use lemnos_device::{BoxedDevice, DeviceClass, DeviceInfo, DeviceStatus, MAX_CHANNELS, NO_VALUE};
use lemnos_drivers_linux::{FanRestore, RestoreKind};
use lemnos_hal::{ErrorKind, HalError};
use lemnos_ipc::{ChannelDesc, ControlDesc, DeviceDesc};
use lemnos_linux_sys::time::boottime_us;

use crate::schedule::advance;

/// The shortest and longest wait before rebuilding a device that failed.
const RETRY_MIN_MS: u64 = 1_000;
const RETRY_MAX_MS: u64 = 30_000;
/// `poll_ms` when the board leaves it out.
const DEFAULT_POLL_MS: u32 = 1_000;
/// A sensor that buffers samples (a FIFO) is drained no more often than this:
/// each drain is a few bus transactions, whatever it returns.
const BATCH_MIN_MS: u32 = 20;
/// ... and no less often than this, so its buffer does not overflow (the
/// BMI088's FIFO holds about 0.37 s at 400 Hz; the batch holds 120 ms).
const BATCH_MAX_MS: u32 = 100;

/// A client's subscription to a device. Deadlines are microseconds on the
/// boot clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Subscription {
    pub client: u32,
    pub period_ms: u32,
    pub next_us: u64,
    /// The channels wanted (bit i: channel i). All of them for a whole-device
    /// subscription.
    pub mask: u64,
}

/// Every channel of a device (`u64::MAX` for a whole-device subscription).
pub(crate) const WHOLE_DEVICE: u64 = u64::MAX;

/// The channels of `info` named by `names`: a channel's name, `prefix.*` for
/// every channel under `prefix.`, or `*` for all. `None` when a name is none of
/// the device's (or there is no name).
pub(crate) fn channel_mask(info: Option<&DeviceInfo>, names: &[String]) -> Option<u64> {
    let info = info?;
    let mut mask = 0u64;
    for name in names {
        if name == "*" {
            mask |= all_bits(info);
            continue;
        }
        let mut hit = false;
        for (i, channel) in info.channels.iter().enumerate().take(64) {
            let matches = match name.strip_suffix(".*") {
                Some(prefix) => channel
                    .name
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.')),
                None => channel.name == name,
            };
            if matches {
                mask |= 1 << i;
                hit = true;
            }
        }
        if !hit {
            return None;
        }
    }
    (mask != 0).then_some(mask)
}

/// The bits of every channel of `info`.
fn all_bits(info: &DeviceInfo) -> u64 {
    match info.channels.len().min(64) {
        64 => u64::MAX,
        n => (1u64 << n) - 1,
    }
}

/// Where a device's reads run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placement {
    /// A sensor on its bus's worker thread (`workers.rs`).
    Worker,
    /// A sensor the service thread reads itself (sysfs, with controls).
    Inline,
    /// Anything else (lights, and devices that are not sensors).
    Other,
}

impl Placement {
    fn of(device: &BoxedDevice) -> Self {
        match device {
            BoxedDevice::Sensor(_) => Self::Worker,
            BoxedDevice::Both(_) => Self::Inline,
            BoxedDevice::Control(_) | BoxedDevice::Light(_) => Self::Other,
        }
    }
}

/// One device.
pub(crate) struct Slot {
    pub spec: DeviceSpec,
    /// The device, when it is here: `None` while its bus's worker reads it,
    /// and when it is missing.
    pub device: Option<BoxedDevice>,
    /// The device is built (it may be on a worker).
    pub present: bool,
    /// The device buffers samples: a read returns every sample since the last.
    pub batched: bool,
    pub placement: Placement,
    /// The bus worker (lane) of a worker sensor.
    pub lane: Option<usize>,
    /// A read is running on the worker.
    pub busy: bool,
    /// The deadline a scheduled read on the worker was due at (`None`: a read
    /// a client asked for, which leaves the schedule alone).
    pub dispatched_us: Option<u64>,
    /// Clients waiting for a one-shot read, answered when it completes.
    pub waiters: Vec<u32>,
    pub info: Option<&'static DeviceInfo>,
    pub status: DeviceStatus,
    pub error: Option<ErrorKind>,
    /// Why the device failed last (the driver's or the board's own words;
    /// empty while it works).
    pub reason: String,
    pub values: [i32; MAX_CHANNELS],
    /// The channels the last read was asked for (bit i: channel i). Channels
    /// outside it hold no value.
    pub values_mask: u64,
    /// When the last good read started (boot clock, microseconds).
    pub read_us: u64,
    pub fresh: bool,
    /// The sensor's next read deadline (boot clock, microseconds).
    pub next_read_us: u64,
    /// How long the last read took (microseconds; 0 before the first).
    pub read_cost_us: u64,
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
            present: false,
            batched: false,
            placement: Placement::Other,
            lane: None,
            busy: false,
            dispatched_us: None,
            waiters: Vec::new(),
            info: None,
            status: DeviceStatus::Missing,
            error: None,
            reason: String::new(),
            values: [NO_VALUE; MAX_CHANNELS],
            values_mask: 0,
            read_us: 0,
            fresh: false,
            next_read_us: 0,
            read_cost_us: 0,
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

    /// The fastest the sensor is read (`poll_ms`): the cap on any
    /// subscription, and the age beyond which a one-shot read is not served
    /// from the last reading.
    pub fn cap_ms(&self) -> u32 {
        self.spec.poll_ms.unwrap_or(DEFAULT_POLL_MS).max(1)
    }

    /// How often the sensor is read while nobody subscribes: `idle_poll_ms`
    /// when set (0 means never). Without it, the cap, except for IMU-class
    /// devices, which are not read until someone subscribes. `None`: never.
    pub fn idle_ms(&self) -> Option<u32> {
        match self.spec.idle_poll_ms {
            Some(0) => None,
            Some(ms) => Some(ms),
            None if self.info.is_some_and(|i| i.class == DeviceClass::Imu) => None,
            None => Some(self.cap_ms()),
        }
    }

    /// The read period now: the fastest subscription, never faster than the
    /// cap (a subscriber asking for 500 ms gets 500 ms, not the cap), else the
    /// idle rate. `None`: the sensor is not read.
    pub fn period_ms(&self) -> Option<u32> {
        match self.subscriptions.iter().map(|s| s.period_ms).min() {
            // A buffered sensor is drained at the subscription's rate, within
            // the batch bounds (its samples carry their own times).
            Some(fastest) if self.batched => {
                Some(fastest.max(self.cap_ms()).clamp(BATCH_MIN_MS, BATCH_MAX_MS))
            }
            Some(fastest) => Some(fastest.max(self.cap_ms())),
            None => self.idle_ms(),
        }
    }

    /// The read period in microseconds (see [`period_ms`](Self::period_ms)).
    pub fn period_us(&self) -> Option<u64> {
        self.period_ms().map(|ms| u64::from(ms) * 1000)
    }

    /// Whether the device is a sensor, read by a worker or inline.
    pub fn is_sensor(&self) -> bool {
        self.present && self.placement != Placement::Other
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
        if self.present || now_ms < self.next_build_ms {
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
                self.placement = Placement::of(&device);
                self.batched = device.sample_period_us().is_some();
                self.device = Some(device);
                self.present = true;
                self.retry_ms = RETRY_MIN_MS;
                self.next_read_us = now_ms * 1000;
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

    /// Reads the sensor at its deadline. The next deadline is the first grid
    /// point after the read ends, so a read longer than the period cannot
    /// leave the sensor due again at once (and starve the clients). Returns a
    /// status change.
    pub fn read_scheduled(&mut self) -> Option<DeviceStatus> {
        let status = self.read();
        self.next_read_us = match self.period_us() {
            Some(period_us) => advance(self.next_read_us, period_us, boottime_us()),
            None => u64::MAX,
        };
        status
    }

    /// Reads the device now and times the read; returns a status change. The
    /// reading is stamped with the time the read started.
    pub fn read(&mut self) -> Option<DeviceStatus> {
        let mask = self.read_mask(boottime_us());
        self.read_with(mask)
    }

    /// Reads every channel now (a one-shot reader's read; see [`read`](Self::read)).
    pub fn read_full(&mut self) -> Option<DeviceStatus> {
        self.read_with(self.all_channels())
    }

    /// Reads the channels in `mask` now.
    fn read_with(&mut self, mask: u64) -> Option<DeviceStatus> {
        let started_us = boottime_us();
        let device = self.device.as_mut()?;
        device.select_channels(mask);
        self.values_mask = mask;
        let mut why = String::new();
        let result = device.read_why(&mut self.values, &mut why);
        let cost_us = boottime_us().saturating_sub(started_us);
        self.finish_read(started_us, cost_us, result, &why)
    }

    /// Every channel of the device (bit i: channel i), or all bits when the
    /// device is not built yet.
    pub fn all_channels(&self) -> u64 {
        self.info.map_or(u64::MAX, all_bits)
    }

    /// The channels the read at `now_us` needs: those of the subscriptions due
    /// by then (within half a period, as a read's deadline slack), and all of
    /// them for a one-shot reader or with no subscriber (an idle read).
    pub fn read_mask(&self, now_us: u64) -> u64 {
        let all = self.all_channels();
        if !self.waiters.is_empty() || self.subscriptions.is_empty() {
            return all;
        }
        let slack = self.period_us().unwrap_or(0) / 2;
        let mask = self
            .subscriptions
            .iter()
            .filter(|s| s.next_us <= now_us + slack)
            .fold(0u64, |m, s| m | (s.mask & all));
        if mask == 0 { all } else { mask }
    }

    /// The channel values a reader wanting `mask` gets: the device's values
    /// for those channels, and no value for the rest.
    pub fn channel_values_for(&self, mask: u64) -> Vec<i32> {
        let n = self.info.map_or(0, |i| i.channels.len());
        (0..n)
            .map(|i| {
                if i < 64 && mask & (1 << i) != 0 {
                    self.values[i]
                } else {
                    NO_VALUE
                }
            })
            .collect()
    }

    /// Records a read that ran (inline, or on a worker with `values` already
    /// in place); returns a status change. A device that is gone is dropped
    /// and rebuilt on the retry schedule.
    pub fn finish_read(
        &mut self,
        started_us: u64,
        cost_us: u64,
        result: Result<(), ErrorKind>,
        why: &str,
    ) -> Option<DeviceStatus> {
        self.read_cost_us = cost_us;
        match result {
            Ok(()) => {
                self.read_us = started_us;
                self.fresh = true;
                self.set_status(DeviceStatus::Available, None)
            }
            Err(kind) => {
                self.reason = format!("read: {why}");
                let status = DeviceStatus::after_error(kind);
                if status == DeviceStatus::Missing {
                    // Gone: rebuild it on the retry schedule.
                    self.device = None;
                    self.present = false;
                    self.fresh = false;
                    self.next_build_ms = started_us / 1000 + self.retry_ms;
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
        if !self.present {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(poll_ms: Option<u32>, idle_poll_ms: Option<u32>) -> Slot {
        let mut spec = DeviceSpec::new("s", "bmi088");
        spec.poll_ms = poll_ms;
        spec.idle_poll_ms = idle_poll_ms;
        Slot::new(spec)
    }

    fn subscribe(slot: &mut Slot, client: u32, period_ms: u32) {
        slot.subscriptions.push(Subscription {
            client,
            period_ms,
            next_us: 0,
            mask: WHOLE_DEVICE,
        });
    }

    #[test]
    fn an_unbuilt_sensor_idles_at_its_cap() {
        // Before the device is built its class is unknown: the cap applies.
        let slot = slot(Some(100), None);
        assert_eq!(slot.idle_ms(), Some(100));
        assert_eq!(slot.period_ms(), Some(100));
    }

    #[test]
    fn idle_poll_zero_means_never() {
        let slot = slot(Some(10), Some(0));
        assert_eq!(slot.idle_ms(), None);
        assert_eq!(slot.period_ms(), None);
        assert_eq!(slot.period_us(), None);
    }

    #[test]
    fn a_subscription_sets_the_rate_but_not_above_the_cap_or_below_its_request() {
        let mut slot = slot(Some(10), Some(0));
        subscribe(&mut slot, 1, 100);
        // A 100 ms subscriber does not make the sensor read at the cap.
        assert_eq!(slot.period_ms(), Some(100));
        subscribe(&mut slot, 2, 2);
        // The fastest subscription wins, but never beyond the cap.
        assert_eq!(slot.period_ms(), Some(10));
    }

    #[test]
    fn the_idle_rate_resumes_when_the_subscriptions_end() {
        let mut slot = slot(Some(10), Some(1000));
        subscribe(&mut slot, 1, 10);
        assert_eq!(slot.period_ms(), Some(10));
        slot.subscriptions.clear();
        assert_eq!(slot.period_ms(), Some(1000));
    }
}
