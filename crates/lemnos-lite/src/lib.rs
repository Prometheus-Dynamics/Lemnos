//! A static device table over the compact device model (`lemnos-device`),
//! for firmware and small Linux images that know their hardware at build
//! time. `#![no_std]`, no allocation.
//!
//! ```ignore
//! let mut imu = Bmi088::new(i2c_imu);
//! let mut power = Ina::new(i2c_power, 0x40, Model::Ina238, cfg)?;
//! let mut devices = lemnos_lite::Devices::new([
//!     lemnos_lite::sensor("imu", &mut imu).every(10),
//!     lemnos_lite::sensor("power", &mut power).every(100),
//! ]);
//! devices.init_all(&mut delay);
//! devices.read("power", &mut buf)?;            // or by index
//! let status = devices.status(1);              // the same DeviceStatus as the runtime
//! devices.poll(now_ms, &mut buf, |reading| { /* reading.name, .info, .values */ });
//! ```
//!
//! The table holds `&mut dyn` devices ([`DeviceRef`]), so code size stays
//! flat as devices are added; names are `&'static str`. Each entry tracks a
//! [`DeviceStatus`], the last [`ErrorKind`], and an optional polling period.
//! There is no discovery, driver matching, event log or allocation: the same
//! code runs on a microcontroller (buses from its HAL) and on Linux (buses
//! from `lemnos_linux::hal`).

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
mod tests;

use embedded_hal::delay::DelayNs;
pub use lemnos_device::{
    Control, DeviceInfo, DeviceRef, DeviceStatus, MAX_CHANNELS, NO_VALUE, Sensor,
};
pub use lemnos_hal::ErrorKind;

/// One device in a [`Devices`] table.
pub struct Entry<'a> {
    name: &'static str,
    device: DeviceRef<'a>,
    status: DeviceStatus,
    last_error: Option<ErrorKind>,
    ready: bool,
    period_ms: u32,
    due_ms: u64,
}

impl<'a> Entry<'a> {
    /// An entry for any [`DeviceRef`].
    pub fn new(name: &'static str, device: DeviceRef<'a>) -> Self {
        Self {
            name,
            device,
            status: DeviceStatus::Missing,
            last_error: None,
            ready: false,
            period_ms: 0,
            due_ms: 0,
        }
    }

    /// Polls this device every `period_ms` milliseconds in
    /// [`Devices::poll`] (0, the default, leaves it out of polling).
    pub fn every(mut self, period_ms: u32) -> Self {
        self.period_ms = period_ms;
        self
    }

    fn record<T>(&mut self, result: Result<T, ErrorKind>) -> Result<T, ErrorKind> {
        match &result {
            Ok(_) => {
                self.status = DeviceStatus::Available;
            }
            Err(kind) => {
                self.last_error = Some(*kind);
                self.status = DeviceStatus::after_error(*kind);
                // A device that disappeared is brought up again before use.
                if self.status == DeviceStatus::Missing {
                    self.ready = false;
                }
            }
        }
        result
    }

    fn init(&mut self, delay: &mut dyn DelayNs) -> Result<(), ErrorKind> {
        let result = self.device.init(delay);
        self.ready = result.is_ok();
        self.record(result)
    }
}

/// A sensor entry.
pub fn sensor<'a>(name: &'static str, device: &'a mut impl Sensor) -> Entry<'a> {
    Entry::new(name, DeviceRef::sensor(device))
}

/// A control entry.
pub fn control<'a>(name: &'static str, device: &'a mut impl Control) -> Entry<'a> {
    Entry::new(name, DeviceRef::control(device))
}

/// An entry for a device that is both a sensor and a control.
pub fn device<'a>(name: &'static str, device: &'a mut (impl Sensor + Control)) -> Entry<'a> {
    Entry::new(name, DeviceRef::both(device))
}

/// Addresses an entry by index or by name.
pub trait Key {
    fn index<const N: usize>(&self, devices: &Devices<'_, N>) -> Option<usize>;
}

impl Key for usize {
    fn index<const N: usize>(&self, _devices: &Devices<'_, N>) -> Option<usize> {
        (*self < N).then_some(*self)
    }
}

impl Key for &str {
    fn index<const N: usize>(&self, devices: &Devices<'_, N>) -> Option<usize> {
        devices.index_of(self)
    }
}

/// One polled reading, handed to the [`Devices::poll`] callback.
#[derive(Debug)]
pub struct Reading<'b> {
    pub index: usize,
    pub name: &'static str,
    pub info: &'static DeviceInfo,
    /// One value per channel of `info`.
    pub values: &'b [i32],
}

/// A fixed table of `N` devices.
pub struct Devices<'a, const N: usize> {
    entries: [Entry<'a>; N],
}

impl<'a, const N: usize> Devices<'a, N> {
    pub fn new(entries: [Entry<'a>; N]) -> Self {
        Self { entries }
    }

    pub const fn len(&self) -> usize {
        N
    }

    pub const fn is_empty(&self) -> bool {
        N == 0
    }

    /// The index of the device called `name`.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.name == name)
    }

    /// The name of entry `index`.
    pub fn name(&self, index: usize) -> Option<&'static str> {
        self.entries.get(index).map(|e| e.name)
    }

    /// The description of a device.
    pub fn info(&self, key: impl Key) -> Option<&'static DeviceInfo> {
        Some(self.entries[key.index(self)?].device.info())
    }

    /// A device's status: [`Missing`](DeviceStatus::Missing) until its first
    /// successful `init`, then updated by every operation.
    pub fn status(&self, key: impl Key) -> DeviceStatus {
        key.index(self)
            .map_or(DeviceStatus::Missing, |i| self.entries[i].status)
    }

    /// The kind of a device's most recent failure, if any.
    pub fn last_error(&self, key: impl Key) -> Option<ErrorKind> {
        self.entries[key.index(self)?].last_error
    }

    /// Initializes every device; returns how many came up.
    pub fn init_all(&mut self, delay: &mut impl DelayNs) -> usize {
        let delay: &mut dyn DelayNs = delay;
        self.entries
            .iter_mut()
            .map(|e| e.init(delay))
            .filter(Result::is_ok)
            .count()
    }

    /// Initializes one device.
    pub fn init(&mut self, key: impl Key, delay: &mut impl DelayNs) -> Result<(), ErrorKind> {
        let i = key.index(self).ok_or(ErrorKind::NotFound)?;
        self.entries[i].init(delay)
    }

    /// Reads a sensor into `out` (one value per channel).
    pub fn read(&mut self, key: impl Key, out: &mut [i32]) -> Result<(), ErrorKind> {
        let entry = self.entry(key)?;
        let result = entry.device.read(out);
        entry.record(result)
    }

    /// Sets a control; returns the value applied.
    pub fn set(&mut self, key: impl Key, control: usize, value: i32) -> Result<i32, ErrorKind> {
        let entry = self.entry(key)?;
        let result = entry.device.set(control, value);
        entry.record(result)
    }

    /// Reads a control's current value.
    pub fn get(&mut self, key: impl Key, control: usize) -> Result<i32, ErrorKind> {
        let entry = self.entry(key)?;
        let result = entry.device.get(control);
        entry.record(result)
    }

    /// Sets a control by name.
    pub fn set_named(
        &mut self,
        key: impl Key,
        control: &str,
        value: i32,
    ) -> Result<i32, ErrorKind> {
        let i = key.index(self).ok_or(ErrorKind::NotFound)?;
        let index = self.entries[i]
            .device
            .info()
            .control_index(control)
            .ok_or(ErrorKind::NotFound)?;
        self.set(i, index, value)
    }

    fn entry(&mut self, key: impl Key) -> Result<&mut Entry<'a>, ErrorKind> {
        let i = key.index(self).ok_or(ErrorKind::NotFound)?;
        let entry = &mut self.entries[i];
        if entry.ready {
            Ok(entry)
        } else {
            Err(ErrorKind::Unavailable)
        }
    }

    /// Reads every sensor whose period has elapsed at `now_ms` (any
    /// monotonic millisecond clock) and hands each successful reading to
    /// `on_reading`. Devices that are not up (never initialized, or gone
    /// missing) are re-initialized on their schedule instead, using `delay`.
    /// Returns how many devices were read.
    pub fn poll(
        &mut self,
        now_ms: u64,
        delay: &mut impl DelayNs,
        buf: &mut [i32],
        mut on_reading: impl FnMut(Reading<'_>),
    ) -> usize {
        let delay: &mut dyn DelayNs = delay;
        let mut read = 0;
        for (index, entry) in self.entries.iter_mut().enumerate() {
            if entry.period_ms == 0 || now_ms < entry.due_ms || !entry.device.is_sensor() {
                continue;
            }
            entry.due_ms = now_ms + u64::from(entry.period_ms);
            if !entry.ready && entry.init(delay).is_err() {
                continue;
            }
            let result = entry.device.read(buf);
            if entry.record(result).is_ok() {
                read += 1;
                let info = entry.device.info();
                on_reading(Reading {
                    index,
                    name: entry.name,
                    info,
                    values: &buf[..info.channels.len()],
                });
            }
        }
        read
    }
}
