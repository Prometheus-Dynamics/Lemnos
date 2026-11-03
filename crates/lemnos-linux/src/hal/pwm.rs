use super::IoError;
use embedded_hal::pwm::{ErrorType, SetDutyCycle};
use lemnos_uevent::LinuxPaths;
use std::fs;
use std::io;
use std::path::PathBuf;

/// One exported sysfs PWM channel (`/sys/class/pwm/pwmchipN/pwmM`) as an
/// embedded-hal [`SetDutyCycle`] channel.
///
/// Duty is written straight to `duty_cycle`, with the period read from
/// `period` on each call (the kernel rejects a duty above the period, so the
/// period is the scale). The channel must be exported and enabled through
/// the `export` and `enable` attributes, which this type does not touch.
/// Written, untested on board (the BCM2712 PWM channels).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysfsPwm {
    channel_root: PathBuf,
}

impl SysfsPwm {
    /// The channel `channel` of `chip_name` under `paths`, without checking
    /// that it is exported.
    pub fn new(paths: &LinuxPaths, chip_name: &str, channel: u32) -> Self {
        Self {
            channel_root: paths.pwm_channel_root(chip_name, channel),
        }
    }

    /// Like [`SysfsPwm::new`], but fails with `NotFound` when the channel
    /// directory is absent (not exported yet).
    pub fn open(paths: &LinuxPaths, chip_name: &str, channel: u32) -> io::Result<Self> {
        let pwm = Self::new(paths, chip_name, channel);
        if !pwm.channel_root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "PWM channel '{}' is not exported",
                    pwm.channel_root.display()
                ),
            ));
        }
        Ok(pwm)
    }

    /// The channel directory, for example `/sys/class/pwm/pwmchip0/pwm0`.
    pub fn channel_root(&self) -> &std::path::Path {
        &self.channel_root
    }

    /// The period in nanoseconds (the `period` attribute).
    pub fn period_ns(&self) -> io::Result<u64> {
        read_u64(&self.channel_root.join("period"))
    }

    /// Sets the duty cycle in nanoseconds (the `duty_cycle` attribute).
    pub fn set_duty_cycle_ns(&mut self, duty_cycle_ns: u64) -> io::Result<()> {
        fs::write(
            self.channel_root.join("duty_cycle"),
            duty_cycle_ns.to_string(),
        )
    }
}

fn read_u64(path: &std::path::Path) -> io::Result<u64> {
    let raw = fs::read_to_string(path)?;
    raw.trim().parse::<u64>().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("'{}' is not a period: {error}", path.display()),
        )
    })
}

impl ErrorType for SysfsPwm {
    type Error = IoError;
}

impl SetDutyCycle for SysfsPwm {
    fn max_duty_cycle(&self) -> u16 {
        u16::MAX
    }

    fn set_duty_cycle(&mut self, duty: u16) -> Result<(), Self::Error> {
        let period = self.period_ns()?;
        let duty_ns = (u128::from(period) * u128::from(duty) / u128::from(u16::MAX)) as u64;
        self.set_duty_cycle_ns(duty_ns).map_err(IoError::from)
    }
}
