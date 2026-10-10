//! The orientation fusion device (`driver = "fusion"`): lemnosd hosts it,
//! because it needs the board's IMU and magnetometer. Its filter
//! (`lemnos-fusion`) takes the IMU's calibrated samples and the
//! magnetometer's, and its output is the device's channels
//! (`docs/imu-calibration-fusion.md`).
//!
//! The device is on demand: while a client subscribes to it (or `always` is
//! set), lemnosd adds an internal subscription to the IMU and, for 9-axis
//! mode, to the magnetometer, so they are read at the fusion's rate. When
//! the last subscriber leaves, those subscriptions go and the sensors idle.

use crate::devices::{Slot, Subscription};
use lemnos_board::{ConfigValue, DeviceSpec};
use lemnos_device::{
    Axis, CalibrationStatus, Channel, DeviceClass, DeviceInfo, DeviceStatus, MAX_CHANNELS,
    NO_VALUE, PART_ACCEL, PART_GYRO, PART_MAG, Quantity,
};
use lemnos_fusion::{Algorithm, Mode, Orientation, OrientationConfig, Quat};

/// The driver name in `board.toml`.
pub const DRIVER: &str = "fusion";
/// `poll_ms` when the board leaves it out: the fastest the fusion reports.
pub const DEFAULT_POLL_MS: u32 = 10;
/// The fastest the IMU is read for the fusion (the subscriber's period is
/// never below this).
pub const MIN_PERIOD_MS: u32 = 10;
/// How often the magnetometer is read for the fusion.
pub const MAG_PERIOD_MS: u32 = 100;
/// The subscriber id of the fusion's internal subscriptions: no socket client
/// has it, so deliveries to clients skip them.
pub(crate) const INTERNAL: u32 = u32::MAX - 1;

/// The fusion device's channels, in the order its values are filled.
pub static FUSION_INFO: DeviceInfo = DeviceInfo::new(
    DeviceClass::Orientation,
    "fusion",
    &[
        Channel::new("quaternion.w", Quantity::Ratio, -6),
        Channel::new("quaternion.x", Quantity::Ratio, -6),
        Channel::new("quaternion.y", Quantity::Ratio, -6),
        Channel::new("quaternion.z", Quantity::Ratio, -6),
        Channel::new("roll", Quantity::Angle, -6),
        Channel::new("pitch", Quantity::Angle, -6),
        Channel::new("yaw", Quantity::Angle, -6),
        Channel::new("gravity.x", Quantity::Acceleration, -3).on(Axis::X),
        Channel::new("gravity.y", Quantity::Acceleration, -3).on(Axis::Y),
        Channel::new("gravity.z", Quantity::Acceleration, -3).on(Axis::Z),
        Channel::new("linear_acceleration.x", Quantity::Acceleration, -3).on(Axis::X),
        Channel::new("linear_acceleration.y", Quantity::Acceleration, -3).on(Axis::Y),
        Channel::new("linear_acceleration.z", Quantity::Acceleration, -3).on(Axis::Z),
        Channel::new("magnetic_disturbance", Quantity::Level, 0),
        Channel::new("confidence.imu", Quantity::Ratio, -3),
        Channel::new("confidence.magnetometer", Quantity::Ratio, -3),
    ],
    &[],
);

const ROLL: usize = 4;
const GRAVITY: usize = 7;
const LINEAR: usize = 10;
const DISTURBANCE: usize = 13;
const CONFIDENCE_IMU: usize = 14;
const CONFIDENCE_MAG: usize = 15;

/// The fusion's settings, from its `config` (see the doc's table).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Settings {
    pub imu: String,
    pub mag: Option<String>,
    pub always: bool,
    pub config: OrientationConfig,
}

fn number(spec: &DeviceSpec, key: &str, default: f32) -> Result<f32, String> {
    match spec.config.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_f64()
            .map(|v| v as f32)
            .ok_or_else(|| format!("{key} must be a number")),
    }
}

/// Parses a fusion device's settings. The registry has checked the keys and
/// their types; this reads them.
pub(crate) fn settings(spec: &DeviceSpec) -> Result<Settings, String> {
    let text = |key: &str| spec.config.get(key).and_then(ConfigValue::as_str);
    let imu = text("imu")
        .ok_or("fusion needs `imu` (the IMU's device id)")?
        .to_string();
    let nine_axis = match text("mode").unwrap_or("6axis") {
        "6axis" => false,
        "9axis" => true,
        other => return Err(format!("mode {other:?} is not 6axis or 9axis")),
    };
    let algorithm = match text("algorithm").unwrap_or("mahony") {
        "mahony" => Algorithm::Mahony,
        "madgwick" => Algorithm::Madgwick,
        other => return Err(format!("algorithm {other:?} is not mahony or madgwick")),
    };
    let mag = text("mag").map(str::to_string);
    if nine_axis && mag.is_none() {
        return Err("mode 9axis needs `mag` (the magnetometer's device id)".into());
    }
    let always = match spec.config.get("always") {
        None => false,
        Some(value) => value.as_bool().ok_or("always must be true or false")?,
    };
    let dip_rad = match spec.config.get("dip_deg") {
        None => None,
        Some(_) => Some(number(spec, "dip_deg", 0.0)?.to_radians()),
    };
    let config = OrientationConfig {
        algorithm,
        mode: if nine_axis {
            Mode::NineAxis
        } else {
            Mode::SixAxis
        },
        kp: number(spec, "kp", 1.0)?,
        ki: number(spec, "ki", 0.05)?,
        beta: number(spec, "beta", 0.1)?,
        mount: Quat::from_euler_deg(
            number(spec, "mount_roll_deg", 0.0)?,
            number(spec, "mount_pitch_deg", 0.0)?,
            number(spec, "mount_yaw_deg", 0.0)?,
        ),
        declination_rad: number(spec, "declination_deg", 0.0)?.to_radians(),
        dip_rad,
    };
    Ok(Settings {
        imu,
        mag: if nine_axis { mag } else { None },
        always,
        config,
    })
}

/// The index of channel `prefix.x`, `prefix.y`, `prefix.z` in `info`.
fn axes(info: &DeviceInfo, prefix: &str) -> Option<[usize; 3]> {
    Some([
        info.channel_index(&format!("{prefix}.x"))?,
        info.channel_index(&format!("{prefix}.y"))?,
        info.channel_index(&format!("{prefix}.z"))?,
    ])
}

fn bits(indices: impl IntoIterator<Item = usize>) -> u64 {
    indices
        .into_iter()
        .filter(|i| *i < 64)
        .fold(0, |mask, i| mask | (1 << i))
}

/// A row's three values as SI units (`scale` is added to each channel's
/// exponent): `None` when one is no value.
fn vector(
    info: &DeviceInfo,
    row: &[i32; MAX_CHANNELS],
    idx: &[usize; 3],
    scale: i32,
) -> Option<[f32; 3]> {
    let mut out = [0.0f32; 3];
    for (axis, &i) in idx.iter().enumerate() {
        let raw = *row.get(i)?;
        if raw == NO_VALUE {
            return None;
        }
        let exponent = i32::from(info.channels.get(i)?.exponent) + scale;
        out[axis] = crate::scaled(raw, i8::try_from(exponent).ok()?) as f32;
    }
    Some(out)
}

/// The counts of `value` at `exponent` (the channel's convention).
fn counts(value: f32, exponent: i32) -> i32 {
    if !value.is_finite() {
        return NO_VALUE;
    }
    (f64::from(value) * 10f64.powi(-exponent)).round() as i32
}

/// A fusion device whose inputs are resolved: the filter and the channels it
/// reads from the IMU and magnetometer.
pub(crate) struct Fusion {
    /// The fusion's slot.
    pub slot: usize,
    pub settings: Settings,
    /// The IMU's slot, and its calibrated channels (accelerometer, gyro).
    pub imu: usize,
    pub accel: [usize; 3],
    pub gyro: [usize; 3],
    pub imu_mask: u64,
    /// The magnetometer's slot and its calibrated field channels (9-axis).
    pub mag: Option<usize>,
    pub field: [usize; 3],
    pub mag_mask: u64,
    pub filter: Orientation,
    /// The last calibration confidences seen, permille (kept while a device
    /// is on its worker).
    imu_confidence: i32,
    mag_confidence: i32,
    /// The time of the last sample fed (boot clock, microseconds).
    last_us: u64,
}

impl Fusion {
    /// Feeds IMU samples, oldest first, `period_us` apart with the last at
    /// `read_us`. A sample with a channel that has no value is skipped.
    pub(crate) fn feed_imu(
        &mut self,
        info: &DeviceInfo,
        samples: &[[i32; MAX_CHANNELS]],
        read_us: u64,
        period_us: u64,
    ) -> bool {
        let Some(last) = samples.len().checked_sub(1) else {
            return false;
        };
        let mut fed = false;
        for (i, row) in samples.iter().enumerate() {
            let at = read_us.saturating_sub((last - i) as u64 * period_us);
            let (Some(accel), Some(gyro)) = (
                vector(info, row, &self.accel, 0),
                vector(info, row, &self.gyro, 0),
            ) else {
                continue;
            };
            self.filter.update_imu(at, gyro, accel);
            self.last_us = self.last_us.max(at);
            fed = true;
        }
        fed
    }

    /// Feeds magnetometer samples (microteslas: the field's exponent plus 6),
    /// `trusted` as the magnetometer's calibration says.
    pub(crate) fn feed_mag(
        &mut self,
        info: &DeviceInfo,
        samples: &[[i32; MAX_CHANNELS]],
        read_us: u64,
        period_us: u64,
        trusted: bool,
    ) -> bool {
        let Some(last) = samples.len().checked_sub(1) else {
            return false;
        };
        let mut fed = false;
        for (i, row) in samples.iter().enumerate() {
            let at = read_us.saturating_sub((last - i) as u64 * period_us);
            let Some(field) = vector(info, row, &self.field, 6) else {
                continue;
            };
            self.filter.update_mag(at, field, trusted);
            self.last_us = self.last_us.max(at);
            fed = true;
        }
        fed
    }

    /// Records the calibration confidences: the IMU's is the lower of its
    /// accelerometer's and gyro's, the magnetometer's its own.
    pub(crate) fn note_confidence(
        &mut self,
        imu: Option<&CalibrationStatus>,
        mag: Option<&CalibrationStatus>,
    ) {
        if let Some(status) = imu {
            self.imu_confidence = i32::from(
                status.parts[PART_ACCEL]
                    .confidence
                    .min(status.parts[PART_GYRO].confidence),
            );
        }
        if let Some(status) = mag {
            self.mag_confidence = i32::from(status.parts[PART_MAG].confidence);
        }
    }

    /// Writes the filter's output into the fusion's slot, when it has one.
    /// Returns the status change, if any. Before the first valid output the
    /// values stay `NO_VALUE` (and the status is degraded).
    pub(crate) fn publish(&self, slots: &mut [Slot]) -> Option<DeviceStatus> {
        let slot = &mut slots[self.slot];
        let out = self.filter.output();
        if !out.valid {
            return slot.set_status(DeviceStatus::Degraded, None);
        }
        let mut values = [NO_VALUE; MAX_CHANNELS];
        let q = out.quaternion;
        for (i, v) in [q.w, q.x, q.y, q.z].into_iter().enumerate() {
            values[i] = counts(v, -6);
        }
        for (i, v) in [out.roll, out.pitch, out.yaw].into_iter().enumerate() {
            values[ROLL + i] = counts(v, -6);
        }
        for axis in 0..3 {
            values[GRAVITY + axis] = counts(out.gravity[axis], -3);
            values[LINEAR + axis] = counts(out.linear_acceleration[axis], -3);
        }
        values[DISTURBANCE] = i32::from(out.magnetic_disturbance);
        values[CONFIDENCE_IMU] = self.imu_confidence;
        values[CONFIDENCE_MAG] = if self.mag.is_some() {
            self.mag_confidence
        } else {
            NO_VALUE
        };
        slot.values = values;
        slot.values_mask = u64::MAX;
        slot.read_us = self.last_us;
        slot.fresh = true;
        slot.set_status(DeviceStatus::Available, None)
    }

    /// Starts the filter again (`calibration reset orientation`). The values
    /// go back to `NO_VALUE` until it is valid.
    pub(crate) fn reset(&mut self, slots: &mut [Slot]) -> Option<DeviceStatus> {
        self.filter.reset();
        self.last_us = 0;
        let slot = &mut slots[self.slot];
        slot.values = [NO_VALUE; MAX_CHANNELS];
        slot.fresh = false;
        slot.set_status(DeviceStatus::Degraded, None)
    }

    /// The magnetometer's calibration is applied (its samples are trusted).
    pub(crate) fn mag_trusted(status: Option<&CalibrationStatus>) -> bool {
        status.is_some_and(|s| s.parts[PART_MAG].active)
    }
}

/// The index of the device called `id`.
fn slot_index(slots: &[Slot], id: &str) -> Option<usize> {
    slots.iter().position(|s| s.id() == id)
}

/// Resolves the fusion in slot `slot`: its settings, and the IMU's and
/// magnetometer's channels by name. The reason it cannot be built, when it
/// cannot (the devices may still come up: the caller retries).
pub(crate) fn resolve(slot: usize, slots: &[Slot]) -> Result<Fusion, String> {
    let settings = settings(&slots[slot].spec)?;
    let imu = slot_index(slots, &settings.imu)
        .ok_or_else(|| format!("no device {:?} (the fusion's imu)", settings.imu))?;
    let imu_info = slots[imu]
        .info
        .ok_or_else(|| format!("imu {:?} is not available yet", settings.imu))?;
    let accel = axes(imu_info, "acceleration_cal")
        .ok_or_else(|| format!("imu {:?} has no acceleration_cal.* channels", settings.imu))?;
    let gyro = axes(imu_info, "angular_rate_cal")
        .ok_or_else(|| format!("imu {:?} has no angular_rate_cal.* channels", settings.imu))?;
    let (mag, field, mag_mask) = match &settings.mag {
        None => (None, [0; 3], 0),
        Some(id) => {
            let index = slot_index(slots, id)
                .ok_or_else(|| format!("no device {id:?} (the fusion's mag)"))?;
            let info = slots[index]
                .info
                .ok_or_else(|| format!("magnetometer {id:?} is not available yet"))?;
            let field = axes(info, "magnetic_field_cal").ok_or_else(|| {
                format!("magnetometer {id:?} has no magnetic_field_cal.* channels")
            })?;
            (Some(index), field, bits(field))
        }
    };
    Ok(Fusion {
        slot,
        imu,
        accel,
        gyro,
        imu_mask: bits(accel.into_iter().chain(gyro)),
        mag,
        field,
        mag_mask,
        filter: Orientation::new(settings.config),
        imu_confidence: 0,
        mag_confidence: 0,
        last_us: 0,
        settings,
    })
}

/// Keeps the IMU's and magnetometer's internal subscriptions in step with
/// the fusions: present while the fusion has a subscriber (or `always`),
/// removed otherwise. Run after every change to the subscriptions, so none
/// outlives its subscriber.
pub(crate) fn sync(fusions: &[Fusion], slots: &mut [Slot], now_us: u64) {
    for f in fusions {
        let fusion = &slots[f.slot];
        let demand = f.settings.always || !fusion.subscriptions.is_empty();
        let period = fusion
            .period_ms()
            .unwrap_or(DEFAULT_POLL_MS)
            .max(MIN_PERIOD_MS);
        set_internal(&mut slots[f.imu], demand, period, f.imu_mask, now_us);
        if let Some(mag) = f.mag {
            set_internal(&mut slots[mag], demand, MAG_PERIOD_MS, f.mag_mask, now_us);
        }
    }
}

/// Adds, updates or removes `slot`'s internal subscription for the fusion.
fn set_internal(slot: &mut Slot, wanted: bool, period_ms: u32, mask: u64, now_us: u64) {
    let at = slot.subscriptions.iter().position(|s| s.client == INTERNAL);
    match (wanted, at) {
        (true, Some(i)) => {
            let sub = &mut slot.subscriptions[i];
            sub.period_ms = period_ms;
            sub.mask = mask;
        }
        (true, None) => {
            slot.subscriptions.push(Subscription {
                client: INTERNAL,
                period_ms,
                next_us: now_us,
                mask,
            });
            // Read it on the new schedule from now.
            slot.next_read_us = slot.next_read_us.min(now_us);
        }
        (false, Some(i)) => {
            slot.subscriptions.remove(i);
        }
        (false, None) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fusion_info_has_the_documented_channels() {
        let names: Vec<&str> = FUSION_INFO.channels.iter().map(|c| c.name).collect();
        assert_eq!(names.len(), 16);
        assert_eq!(names[0], "quaternion.w");
        assert_eq!(names[ROLL], "roll");
        assert_eq!(names[GRAVITY], "gravity.x");
        assert_eq!(names[LINEAR], "linear_acceleration.x");
        assert_eq!(names[DISTURBANCE], "magnetic_disturbance");
        assert_eq!(names[CONFIDENCE_IMU], "confidence.imu");
        assert_eq!(names[CONFIDENCE_MAG], "confidence.magnetometer");
    }

    #[test]
    fn settings_read_the_documented_keys_and_refuse_bad_values() {
        let spec = DeviceSpec::new("orientation", DRIVER)
            .with("imu", ConfigValue::String("imu".into()))
            .with("mag", ConfigValue::String("magnetometer".into()))
            .with("mode", ConfigValue::String("9axis".into()))
            .with("mount_yaw_deg", ConfigValue::Integer(90));
        let s = settings(&spec).unwrap();
        assert_eq!(s.imu, "imu");
        assert_eq!(s.mag.as_deref(), Some("magnetometer"));
        assert_eq!(s.config.mode, Mode::NineAxis);
        assert!(!s.always);
        assert!(s.config.dip_rad.is_none());

        let six = DeviceSpec::new("orientation", DRIVER)
            .with("imu", ConfigValue::String("imu".into()))
            .with("mag", ConfigValue::String("magnetometer".into()));
        // 6-axis ignores the magnetometer.
        let s = settings(&six).unwrap();
        assert_eq!(s.config.mode, Mode::SixAxis);
        assert!(s.mag.is_none());

        let no_mag = DeviceSpec::new("orientation", DRIVER)
            .with("imu", ConfigValue::String("imu".into()))
            .with("mode", ConfigValue::String("9axis".into()));
        assert!(settings(&no_mag).unwrap_err().contains("needs `mag`"));
    }

    #[test]
    fn a_row_with_no_value_is_not_a_sample() {
        let idx = [0usize, 1, 2];
        let mut row = [NO_VALUE; MAX_CHANNELS];
        row[0] = 1000;
        row[1] = 2000;
        assert!(vector(&FUSION_INFO, &row, &idx, 0).is_none());
        row[2] = 3000;
        // The fusion's own channels are quaternion counts (exponent -6).
        let v = vector(&FUSION_INFO, &row, &idx, 0).unwrap();
        assert!((v[0] - 0.001).abs() < 1e-6);
        assert!((v[2] - 0.003).abs() < 1e-6);
    }
}
