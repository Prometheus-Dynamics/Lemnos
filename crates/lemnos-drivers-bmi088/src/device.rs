//! The BMI088 in the Lemnos device model (`lemnos-device`).

#[cfg(feature = "align")]
use crate::align;
use crate::{Bmi088, Error, MAX_SAMPLES, asynch};
use embedded_hal::delay::DelayNs;
use lemnos_device::kernel::{KernelBinding, KernelChannel, KernelPart, Subsystem};
use lemnos_device::{
    Axis, Channel, Device, DeviceClass, DeviceError, DeviceInfo, NO_VALUE, Quantity,
};

/// The channels: six raw (`acceleration`, `angular_rate`) and six calibrated
/// (`acceleration_cal`, `angular_rate_cal`), X, Y, Z each. The raw counts are
/// acceleration in mm/s² (exponent -3) and angular rate in µrad/s (exponent
/// -6); a channel's value (counts × 10^exponent) is in m/s² and rad/s, the
/// quantities' units. The calibrated channels are the same units, with the
/// accelerometer's offset and scale and the gyroscope's zero-rate offset
/// removed (see `calibration.rs`); `NO_VALUE` without the `float` feature.
pub static INFO: DeviceInfo = DeviceInfo::new(
    DeviceClass::Imu,
    "BMI088",
    &[
        Channel::new("acceleration.x", Quantity::Acceleration, -3).on(Axis::X),
        Channel::new("acceleration.y", Quantity::Acceleration, -3).on(Axis::Y),
        Channel::new("acceleration.z", Quantity::Acceleration, -3).on(Axis::Z),
        Channel::new("angular_rate.x", Quantity::AngularRate, -6).on(Axis::X),
        Channel::new("angular_rate.y", Quantity::AngularRate, -6).on(Axis::Y),
        Channel::new("angular_rate.z", Quantity::AngularRate, -6).on(Axis::Z),
        Channel::new("acceleration_cal.x", Quantity::Acceleration, -3).on(Axis::X),
        Channel::new("acceleration_cal.y", Quantity::Acceleration, -3).on(Axis::Y),
        Channel::new("acceleration_cal.z", Quantity::Acceleration, -3).on(Axis::Z),
        Channel::new("angular_rate_cal.x", Quantity::AngularRate, -6).on(Axis::X),
        Channel::new("angular_rate_cal.y", Quantity::AngularRate, -6).on(Axis::Y),
        Channel::new("angular_rate_cal.z", Quantity::AngularRate, -6).on(Axis::Z),
    ],
    &[],
);

/// The raw channels come first; the calibrated ones follow.
const RAW: usize = 6;

/// A sensor time read and the host times around it (with a host clock).
type Ticks = Option<(u32, u64, u64)>;
const CHANNELS: usize = 12;

/// The mainline kernel drivers: `bmi088-accel` (IIO, m/s²) for the
/// accelerometer and `bmg160` (IIO, rad/s) for the gyroscope.
pub static KERNEL: KernelBinding = KernelBinding {
    parts: &[
        KernelPart {
            subsystem: Subsystem::Iio,
            names: &["bmi088-accel", "bmi088_accel", "bmi088a"],
        },
        KernelPart {
            subsystem: Subsystem::Iio,
            names: &["bmi088_gyro", "bmi088-gyro", "bmi088g"],
        },
    ],
    channels: &[
        Some(KernelChannel::new(0, "in_accel_x", 0)),
        Some(KernelChannel::new(0, "in_accel_y", 0)),
        Some(KernelChannel::new(0, "in_accel_z", 0)),
        Some(KernelChannel::new(1, "in_anglvel_x", 0)),
        Some(KernelChannel::new(1, "in_anglvel_y", 0)),
        Some(KernelChannel::new(1, "in_anglvel_z", 0)),
        // The calibrated channels are the driver's, not the kernel's.
        None,
        None,
        None,
        None,
        None,
        None,
    ],
};

/// Copies `src` into `dst` element by element (`memcpy` is a library call on
/// MCUs).
fn copy_into(dst: &mut [i32], src: &[i32]) {
    for (slot, value) in dst.iter_mut().zip(src) {
        *slot = *value;
    }
}

fn fill(out: &mut [i32], values: [i32; 6]) {
    // Element by element: `copy_from_slice` would link `memcpy`.
    for (slot, value) in out.iter_mut().zip(values) {
        *slot = value;
    }
}

/// Rounds half away from zero (no `libm`: `no_std`).
#[cfg(feature = "float")]
fn round(x: f32) -> i32 {
    if x >= 0.0 {
        (x + 0.5) as i32
    } else {
        (x - 0.5) as i32
    }
}

/// The calibrated channels in device units: mm/s² and µrad/s.
#[cfg(feature = "float")]
fn cal_counts(accel: [f32; 3], gyro: [f32; 3]) -> [i32; 6] {
    [
        round(accel[0] * 1_000.0),
        round(accel[1] * 1_000.0),
        round(accel[2] * 1_000.0),
        round(gyro[0] * 1_000_000.0),
        round(gyro[1] * 1_000_000.0),
        round(gyro[2] * 1_000_000.0),
    ]
}

impl<I2C: embedded_hal::i2c::I2c> Device for Bmi088<I2C> {
    type Error = Error<I2C::Error>;

    fn info(&self) -> &'static DeviceInfo {
        &INFO
    }

    /// Runs [`Bmi088::init`] with the configuration from
    /// [`with_config`](Bmi088::with_config) (or the last one applied).
    fn init(&mut self, delay: &mut dyn DelayNs) -> Result<(), DeviceError<Self::Error>> {
        let settings = self.settings;
        Bmi088::init(self, &mut &mut *delay, settings).map_err(DeviceError::Driver)
    }
}

impl<I2C: embedded_hal::i2c::I2c> lemnos_device::Sensor for Bmi088<I2C> {
    fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<Self::Error>> {
        lemnos_device::check_buffer(&INFO, out)?;
        let config = self
            .config
            .ok_or(DeviceError::Driver(Error::NotInitialized))?;
        let (accel, gyro) = self
            .read_axes(self.selection() & 0b111, self.selection() >> 3)
            .map_err(DeviceError::Driver)?;
        let mut values = [NO_VALUE; CHANNELS];
        let raw = config.channels(accel, gyro);
        copy_into(&mut values[..RAW], &raw);
        // A read of every axis of both dies is a full sample: it feeds the
        // calibration and gives the calibrated channels a value.
        #[cfg(feature = "float")]
        if self.selection() == crate::ALL_AXES {
            let si = config.sample(accel, gyro);
            let period = u64::from(config.accel_rate.period_us());
            let (a, g) = self.cal.sample(0, period, si.accel_mps2, si.gyro_radps);
            copy_into(&mut values[RAW..], &cal_counts(a, g));
        }
        let mask = self.channel_mask();
        for (i, slot) in out.iter_mut().enumerate().take(CHANNELS) {
            *slot = if mask & (1 << i) != 0 {
                values[i]
            } else {
                NO_VALUE
            };
        }
        Ok(())
    }

    /// With the FIFOs on, every sample since the last read, oldest first (up
    /// to [`MAX_SAMPLES`]). The accelerometer sets the stream: every one of its
    /// samples is returned. The two dies run on their own clocks (on board #1
    /// the accelerometer ran 1.1 % faster), so the gyroscope samples are paired
    /// from the newest end. Extra gyroscope samples (the oldest) are not used.
    /// When the gyroscope holds fewer, its oldest sample is repeated for the
    /// oldest accelerometer samples; they are at most a period or two apart.
    /// Every sample feeds the calibration. Without the FIFOs, one sample, as
    /// [`read`](Self::read).
    fn read_batch(
        &mut self,
        out: &mut [[i32; lemnos_device::MAX_CHANNELS]],
    ) -> Result<usize, DeviceError<Self::Error>> {
        if !self.fifo() {
            let Some(row) = out.first_mut() else {
                return Ok(0);
            };
            lemnos_device::Sensor::read(self, &mut row[..])?;
            return Ok(1);
        }
        let config = self
            .config
            .ok_or(DeviceError::Driver(Error::NotInitialized))?;
        let cap = out.len().min(MAX_SAMPLES);
        let mut accel = [[0i16; 3]; MAX_SAMPLES];
        let mut gyro = [[0i16; 3]; MAX_SAMPLES];
        let clock = self.clock();
        let time = || clock.map_or(0, |now| now());
        // Each die's count, timed, then its frames (the gyroscope's whole
        // FIFO, so the newest frames are the ones read).
        let t0 = time();
        let bytes = self.accel_fifo_bytes().map_err(DeviceError::Driver)?;
        let t1 = time();
        // The sensor time right after the count, to place the accelerometer's
        // samples (with a host clock only).
        let ticks = self.sensortime(clock).map_err(DeviceError::Driver)?;
        let (na, skipped) = self
            .read_accel_frames(bytes, &mut accel[..cap])
            .map_err(DeviceError::Driver)?;
        if na == 0 {
            return Ok(0);
        }
        let t2 = time();
        let status = self.gyro_fifo_frames().map_err(DeviceError::Driver)?;
        let t3 = time();
        let mut ng = self
            .read_gyro_frames(status, &mut gyro)
            .map_err(DeviceError::Driver)?;
        let aligned = self.track(
            &config,
            bytes,
            na,
            skipped,
            status,
            ng,
            [t0, t1, t2, t3],
            ticks,
        );
        if ng == 0 {
            // The gyroscope FIFO is empty: hold its latest value for these.
            gyro[0] = self.read_gyro_raw().map_err(DeviceError::Driver)?;
            ng = 1;
        }
        if aligned {
            ng = self.resample(&mut gyro, ng, na);
        }
        let n = na;
        #[cfg(feature = "float")]
        let period = u64::from(config.accel_rate.period_us());
        for i in 0..n {
            let (a, g) = (accel[i], gyro[(ng + i).saturating_sub(na)]);
            let mut row = [NO_VALUE; CHANNELS];
            copy_into(&mut row[..RAW], &config.channels(a, g));
            #[cfg(feature = "float")]
            {
                let si = config.sample(a, g);
                let back = (n - 1 - i) as u64 * period;
                let (ca, cg) = self.cal.sample(back, period, si.accel_mps2, si.gyro_radps);
                copy_into(&mut row[RAW..], &cal_counts(ca, cg));
            }
            for (slot, value) in out[i].iter_mut().zip(row) {
                *slot = value;
            }
        }
        Ok(n)
    }

    fn sample_period_us(&self) -> Option<u32> {
        if !self.fifo() {
            return None;
        }
        self.config().map(|config| config.accel_rate.period_us())
    }

    /// The channels (bit i: channel i of [`INFO`]). The FIFO batch reads
    /// return every channel (both FIFOs hold both dies); the selection applies
    /// to plain reads.
    #[cfg(feature = "align")]
    fn set_clock(&mut self, now_ns: fn() -> u64) {
        self.now = Some(now_ns);
    }

    fn set_clock_us(&mut self, t_us: u64) {
        #[cfg(feature = "float")]
        self.cal.set_read_us(t_us);
        #[cfg(not(feature = "float"))]
        let _ = t_us;
    }

    fn select_channels(&mut self, mask: u64) {
        self.select_channel_mask((mask & 0x0fff) as u16);
    }

    /// The calibration's status (feature `float`; `None` without it).
    #[cfg(feature = "float")]
    fn calibration_status(&self) -> Option<lemnos_device::CalibrationStatus> {
        Some(self.calibration_status())
    }

    #[cfg(feature = "float")]
    fn calibration_command(
        &mut self,
        command: lemnos_device::CalibrationCommand,
    ) -> Result<(), DeviceError<Self::Error>> {
        self.cal
            .command(command)
            .map_err(|_| DeviceError::Unsupported)
    }

    #[cfg(feature = "float")]
    fn calibration_words(&self, out: &mut [i32]) -> usize {
        self.cal.words(out)
    }

    #[cfg(feature = "float")]
    fn load_calibration(&mut self, words: &[i32]) -> Result<(), DeviceError<Self::Error>> {
        self.cal
            .load(words)
            .map_err(|()| DeviceError::Driver(Error::CalibrationWords))
    }
}

impl<I2C: embedded_hal_async::i2c::I2c> lemnos_device::asynch::Device for asynch::Bmi088<I2C> {
    type Error = Error<I2C::Error>;

    fn info(&self) -> &'static DeviceInfo {
        &INFO
    }

    async fn init(
        &mut self,
        delay: &mut impl embedded_hal_async::delay::DelayNs,
    ) -> Result<(), DeviceError<Self::Error>> {
        let settings = self.settings;
        asynch::Bmi088::init(self, delay, settings)
            .await
            .map_err(DeviceError::Driver)
    }
}

impl<I2C: embedded_hal_async::i2c::I2c> lemnos_device::asynch::Sensor for asynch::Bmi088<I2C> {
    async fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<Self::Error>> {
        lemnos_device::check_buffer(&INFO, out)?;
        let config = self
            .config
            .ok_or(DeviceError::Driver(Error::NotInitialized))?;
        let accel = self.read_accel_raw().await.map_err(DeviceError::Driver)?;
        let gyro = self.read_gyro_raw().await.map_err(DeviceError::Driver)?;
        fill(out, config.channels(accel, gyro));
        // The calibration runs on the blocking driver only.
        for slot in out.iter_mut().skip(RAW).take(CHANNELS - RAW) {
            *slot = NO_VALUE;
        }
        Ok(())
    }
}

impl<I2C: embedded_hal::i2c::I2c> Bmi088<I2C> {
    /// Feeds each die's clock with its timed count (`t`: before and after the
    /// accelerometer's count, then the gyroscope's) and the frames read.
    /// Returns whether the batch can be resampled: both clocks running and
    /// gyroscope frames read. An overflowed FIFO restarts both clocks.
    #[cfg(feature = "align")]
    #[allow(clippy::too_many_arguments)]
    fn track(
        &mut self,
        config: &crate::Config,
        accel_bytes: usize,
        accel_read: usize,
        skipped: u32,
        gyro_fill: usize,
        gyro_read: usize,
        t: [u64; 4],
        ticks: Ticks,
    ) -> bool {
        if self.now.is_none() {
            return false;
        }
        if skipped > 0 || gyro_fill >= crate::fifo::GYR_FIFO_FRAMES {
            self.align.reset();
            return false;
        }
        let accel_ns = config.accel_rate.period_us() * 1000;
        let gyro_ns = config.gyro_rate.period_us() * 1000;
        let a = &mut self.align;
        let fill = accel_bytes / crate::fifo::ACC_FRAME_LEN;
        a.accel.count(fill, t[1], accel_ns);
        if let Some((counter, before, after)) = ticks {
            let newest = (a.accel.first_unread() + fill as u64).checked_sub(1);
            a.ticks.read(
                counter,
                before,
                after,
                newest,
                config.accel_rate.period_us(),
            );
        }
        a.accel.took(accel_read);
        a.gyro.count(gyro_fill, t[3], gyro_ns);
        a.gyro.took(gyro_read);
        gyro_read > 0
    }
    /// Without the `align` feature nothing is tracked: the FIFOs are paired.
    #[cfg(not(feature = "align"))]
    #[allow(clippy::too_many_arguments, clippy::unused_self)]
    fn track(
        &mut self,
        _: &crate::Config,
        _: usize,
        _: usize,
        _: u32,
        _: usize,
        _: usize,
        _: [u64; 4],
        _: Ticks,
    ) -> bool {
        false
    }

    /// The accelerometer's sensor time and the host times around its read,
    /// with a host clock.
    #[cfg(feature = "align")]
    fn sensortime(&mut self, clock: Option<fn() -> u64>) -> Result<Ticks, Error<I2C::Error>> {
        let Some(now) = clock else {
            return Ok(None);
        };
        let before = now();
        let counter = self.read_sensortime()?;
        Ok(Some((counter, before, now())))
    }

    #[cfg(not(feature = "align"))]
    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn sensortime(&mut self, _: Option<fn() -> u64>) -> Result<Ticks, Error<I2C::Error>> {
        Ok(None)
    }

    /// The host's clock, if it gave one (never without `align`).
    fn clock(&self) -> Option<fn() -> u64> {
        #[cfg(feature = "align")]
        return self.now;
        #[cfg(not(feature = "align"))]
        None
    }

    /// Resamples the gyroscope (`gyro[..ng]`, just read) at each of the `na`
    /// accelerometer samples' times into `gyro[..na]`; returns `na`.
    #[cfg(feature = "align")]
    fn resample(&mut self, gyro: &mut [[i16; 3]; MAX_SAMPLES], ng: usize, na: usize) -> usize {
        let first_a = self.align.accel.first_unread() - na as u64;
        let first_g = self.align.gyro.first_unread() - ng as u64;
        let before = self.align.last_gyro;
        let mut resampled = [[0i16; 3]; MAX_SAMPLES];
        for (i, slot) in resampled[..na].iter_mut().enumerate() {
            let t = self.align.accel_time(first_a + i as u64);
            *slot = align::gyro_at(t, &self.align.gyro, first_g, &gyro[..ng], before);
        }
        self.align.last_gyro = Some((first_g + ng as u64 - 1, gyro[ng - 1]));
        gyro[..na].copy_from_slice(&resampled[..na]);
        na
    }

    #[cfg(not(feature = "align"))]
    #[allow(clippy::unused_self)]
    fn resample(&mut self, _: &mut [[i16; 3]; MAX_SAMPLES], ng: usize, _: usize) -> usize {
        ng
    }
}
