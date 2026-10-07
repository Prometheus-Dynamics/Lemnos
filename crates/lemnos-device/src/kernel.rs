//! How a device's channels map onto a Linux kernel driver's sysfs interface
//! (IIO or hwmon), as plain data.
//!
//! A chip crate that has a mainline kernel driver publishes a
//! [`KernelBinding`] next to its [`DeviceInfo`](crate::DeviceInfo). A generic
//! host-side binding (`lemnos-drivers-linux`) then reads the kernel's
//! attributes and produces exactly the channels the userspace driver would,
//! so consumers cannot tell which backend served a device. No code per chip
//! is involved: the binding is a table.

/// The kernel subsystem a part is exposed through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Subsystem {
    /// `/sys/bus/iio/devices/iio:deviceN`: `<attr>_raw` × `<attr>_scale`
    /// (plus `_offset`), or `<attr>_input` when the driver processes values.
    Iio,
    /// `/sys/class/hwmon/hwmonN`: `<attr>_input` integers.
    Hwmon,
}

/// One kernel device that serves part of a chip. The BMI088, for example, is
/// two IIO devices: the accelerometer and the gyroscope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KernelPart {
    pub subsystem: Subsystem,
    /// Values of the device's `name` attribute that identify this part.
    pub names: &'static [&'static str],
}

/// Where one channel comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KernelChannel {
    /// The index into [`KernelBinding::parts`].
    pub part: u8,
    /// The attribute stem: `in_accel_x` (IIO), `in1` or `temp1` (hwmon).
    pub attribute: &'static str,
    /// The power of ten from the kernel's unit to the channel's canonical
    /// unit: 0 for IIO m/s², -4 for IIO gauss to tesla, -3 for hwmon mV or
    /// m°C, -6 for hwmon µW.
    pub unit_exponent: i8,
}

impl KernelChannel {
    pub const fn new(part: u8, attribute: &'static str, unit_exponent: i8) -> Self {
        Self {
            part,
            attribute,
            unit_exponent,
        }
    }
}

/// The kernel interface of a device: its parts and, for every channel of its
/// `DeviceInfo` in order, where the value comes from (`None`: the kernel
/// driver does not expose it, and the channel reads `NO_VALUE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KernelBinding {
    pub parts: &'static [KernelPart],
    pub channels: &'static [Option<KernelChannel>],
}
