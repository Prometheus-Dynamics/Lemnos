//! A PWM channel through sysfs.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use lemnos_hal::ErrorKind;
use lemnos_hal::raw::{Polarity, PwmConfig, RawPwm};

use crate::{Claim, kind_of};

/// How long a freshly exported channel gets to appear.
const EXPORT_TRIES: u32 = 50;
const EXPORT_WAIT: Duration = Duration::from_millis(2);

/// What applying a configuration needs of a channel.
trait Channel {
    fn read(&self) -> io::Result<PwmConfig>;
    fn write_period(&self, ns: u64) -> io::Result<()>;
    fn write_duty(&self, ns: u64) -> io::Result<()>;
    fn write_polarity(&self, polarity: Polarity) -> io::Result<()>;
    fn write_enable(&self, enabled: bool) -> io::Result<()>;
}

/// Changes only what differs. The kernel refuses a duty above the period, and a polarity change
/// while enabled, so the order matters.
fn apply(channel: &impl Channel, target: &PwmConfig) -> io::Result<()> {
    if target.period_ns == 0 || target.duty_ns > target.period_ns {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let current = channel.read()?;
    let timing_changes = current.period_ns != target.period_ns || current.duty_ns != target.duty_ns;
    let stop_first = current.enabled
        && (timing_changes || current.polarity != target.polarity || !target.enabled);
    if stop_first {
        channel.write_enable(false)?;
    }
    let period_first = target.period_ns > current.period_ns || target.duty_ns > current.period_ns;
    if period_first && current.period_ns != target.period_ns {
        channel.write_period(target.period_ns)?;
    }
    if current.duty_ns != target.duty_ns {
        channel.write_duty(target.duty_ns)?;
    }
    if !period_first && current.period_ns != target.period_ns {
        channel.write_period(target.period_ns)?;
    }
    if current.polarity != target.polarity {
        channel.write_polarity(target.polarity)?;
    }
    if stop_first || current.enabled != target.enabled {
        channel.write_enable(target.enabled)?;
    }
    Ok(())
}

/// One `pwmchipN/pwmM`, exported if nobody had. An export this made is undone when dropped.
pub struct SysfsPwm {
    period: PathBuf,
    duty: PathBuf,
    polarity: PathBuf,
    enable: PathBuf,
    unexport: Option<(PathBuf, u32)>,
    _claim: Option<Claim>,
}

fn decimal(value: u64, buffer: &mut [u8; 20]) -> &[u8] {
    let mut at = buffer.len();
    let mut rest = value;
    loop {
        at -= 1;
        buffer[at] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            return &buffer[at..];
        }
    }
}

/// Reads a small attribute into `buffer`, without its trailing newline.
fn read_attribute<'b>(path: &Path, buffer: &'b mut [u8; 32]) -> io::Result<&'b str> {
    let n = File::open(path)?.read(buffer)?;
    core::str::from_utf8(&buffer[..n])
        .map(str::trim)
        .map_err(|_| io::ErrorKind::InvalidData.into())
}

fn read_number(path: &Path) -> io::Result<u64> {
    let mut buffer = [0; 32];
    read_attribute(path, &mut buffer)?
        .parse()
        .map_err(|_| io::ErrorKind::InvalidData.into())
}

impl SysfsPwm {
    /// `chip` is the `pwmchipN` directory.
    pub fn open(chip: impl AsRef<Path>, channel: u32) -> io::Result<Self> {
        let chip = chip.as_ref();
        let root = chip.join(format!("pwm{channel}"));
        let mut unexport = None;
        if !root.exists() {
            fs::write(chip.join("export"), channel.to_string())?;
            unexport = Some((chip.join("unexport"), channel));
            let mut tries = 0;
            while !root.exists() {
                tries += 1;
                if tries == EXPORT_TRIES {
                    return Err(io::ErrorKind::NotFound.into());
                }
                std::thread::sleep(EXPORT_WAIT);
            }
        }
        Ok(Self {
            period: root.join("period"),
            duty: root.join("duty_cycle"),
            polarity: root.join("polarity"),
            enable: root.join("enable"),
            unexport,
            _claim: None,
        })
    }

    pub(crate) fn holding(mut self, claim: Claim) -> Self {
        self._claim = Some(claim);
        self
    }
}

impl Drop for SysfsPwm {
    fn drop(&mut self) {
        if let Some((path, channel)) = &self.unexport {
            // Nothing can be done about a failure here: the channel stays exported.
            let _ = fs::write(path, channel.to_string());
        }
    }
}

impl Channel for SysfsPwm {
    fn read(&self) -> io::Result<PwmConfig> {
        let mut buffer = [0; 32];
        let polarity = match read_attribute(&self.polarity, &mut buffer)? {
            "normal" => Polarity::Normal,
            "inversed" | "inverse" | "inverted" => Polarity::Inversed,
            _ => return Err(io::ErrorKind::InvalidData.into()),
        };
        let mut buffer = [0; 32];
        let enabled = match read_attribute(&self.enable, &mut buffer)? {
            "0" => false,
            "1" => true,
            _ => return Err(io::ErrorKind::InvalidData.into()),
        };
        Ok(PwmConfig {
            period_ns: read_number(&self.period)?,
            duty_ns: read_number(&self.duty)?,
            polarity,
            enabled,
        })
    }

    fn write_period(&self, ns: u64) -> io::Result<()> {
        fs::write(&self.period, decimal(ns, &mut [0; 20]))
    }

    fn write_duty(&self, ns: u64) -> io::Result<()> {
        fs::write(&self.duty, decimal(ns, &mut [0; 20]))
    }

    fn write_polarity(&self, polarity: Polarity) -> io::Result<()> {
        let text = match polarity {
            Polarity::Normal => "normal",
            Polarity::Inversed => "inversed",
        };
        fs::write(&self.polarity, text)
    }

    fn write_enable(&self, enabled: bool) -> io::Result<()> {
        fs::write(&self.enable, if enabled { "1" } else { "0" })
    }
}

impl RawPwm for SysfsPwm {
    fn configure(&mut self, config: &PwmConfig) -> Result<(), ErrorKind> {
        apply(self, config).map_err(|e| kind_of(&e))
    }

    fn config(&mut self) -> Result<PwmConfig, ErrorKind> {
        self.read().map_err(|e| kind_of(&e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_measure::test_alloc::allocations;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Recorder {
        current: PwmConfig,
        writes: RefCell<Vec<String>>,
    }

    impl Channel for Recorder {
        fn read(&self) -> io::Result<PwmConfig> {
            Ok(self.current)
        }
        fn write_period(&self, ns: u64) -> io::Result<()> {
            self.writes.borrow_mut().push(format!("period {ns}"));
            Ok(())
        }
        fn write_duty(&self, ns: u64) -> io::Result<()> {
            self.writes.borrow_mut().push(format!("duty {ns}"));
            Ok(())
        }
        fn write_polarity(&self, polarity: Polarity) -> io::Result<()> {
            self.writes.borrow_mut().push(format!("{polarity:?}"));
            Ok(())
        }
        fn write_enable(&self, enabled: bool) -> io::Result<()> {
            self.writes.borrow_mut().push(format!("enable {enabled}"));
            Ok(())
        }
    }

    fn config(period_ns: u64, duty_ns: u64, enabled: bool) -> PwmConfig {
        PwmConfig {
            period_ns,
            duty_ns,
            polarity: Polarity::Normal,
            enabled,
        }
    }

    fn writes(current: PwmConfig, target: PwmConfig) -> Vec<String> {
        let recorder = Recorder {
            current,
            ..Recorder::default()
        };
        apply(&recorder, &target).unwrap();
        recorder.writes.into_inner()
    }

    #[test]
    fn a_longer_period_is_written_before_the_duty() {
        assert_eq!(
            writes(config(100, 50, false), config(200, 150, false)),
            ["period 200", "duty 150"]
        );
    }

    #[test]
    fn a_shorter_period_is_written_after_the_duty() {
        assert_eq!(
            writes(config(200, 150, false), config(100, 50, false)),
            ["duty 50", "period 100"]
        );
    }

    #[test]
    fn a_running_channel_stops_for_a_change_and_starts_again() {
        assert_eq!(
            writes(config(100, 50, true), config(100, 60, true)),
            ["enable false", "duty 60", "enable true"]
        );
    }

    #[test]
    fn the_same_configuration_writes_nothing() {
        assert!(writes(config(100, 50, true), config(100, 50, true)).is_empty());
    }

    #[test]
    fn polarity_changes_with_the_channel_stopped() {
        let mut target = config(100, 50, true);
        target.polarity = Polarity::Inversed;
        assert_eq!(
            writes(config(100, 50, true), target),
            ["enable false", "Inversed", "enable true"]
        );
    }

    #[test]
    fn impossible_configurations_are_refused() {
        let recorder = Recorder::default();
        assert!(apply(&recorder, &config(0, 0, false)).is_err());
        assert!(apply(&recorder, &config(10, 11, false)).is_err());
        assert!(recorder.writes.into_inner().is_empty());
    }

    fn fake_chip(name: &str, exported: bool) -> PathBuf {
        let chip = std::env::temp_dir().join(format!("lemnos-pwm-{}-{name}", std::process::id()));
        let root = chip.join("pwm1");
        std::fs::create_dir_all(if exported { &root } else { &chip }).unwrap();
        if exported {
            for (name, value) in [
                ("period", "1000\n"),
                ("duty_cycle", "250\n"),
                ("polarity", "normal\n"),
                ("enable", "0\n"),
            ] {
                std::fs::write(root.join(name), value).unwrap();
            }
        }
        chip
    }

    #[test]
    fn a_sysfs_channel_reads_and_writes_its_attributes() {
        let chip = fake_chip("rw", true);
        let mut pwm = SysfsPwm::open(&chip, 1).unwrap();
        assert_eq!(pwm.config(), Ok(config(1000, 250, false)));
        pwm.configure(&config(2000, 500, true)).unwrap();
        assert_eq!(pwm.config(), Ok(config(2000, 500, true)));
        drop(pwm);
        std::fs::remove_dir_all(chip).unwrap();
    }

    #[test]
    fn a_channel_nobody_exports_gives_up_cleanly() {
        let chip = fake_chip("export", false);
        // The fake tree has an export file but nothing that reacts to it.
        std::fs::write(chip.join("export"), "").unwrap();
        assert_eq!(
            SysfsPwm::open(&chip, 1).err().unwrap().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(std::fs::read_to_string(chip.join("export")).unwrap(), "1");
        std::fs::remove_dir_all(chip).unwrap();
    }

    #[test]
    fn a_channel_that_was_already_exported_stays_exported() {
        let chip = fake_chip("left", true);
        drop(SysfsPwm::open(&chip, 1).unwrap());
        assert!(!chip.join("unexport").exists());
        std::fs::remove_dir_all(chip).unwrap();
    }

    #[test]
    fn numbers_are_written_as_decimal() {
        assert_eq!(decimal(0, &mut [0; 20]), b"0");
        assert_eq!(decimal(1_000_000, &mut [0; 20]), b"1000000");
        assert_eq!(decimal(u64::MAX, &mut [0; 20]), b"18446744073709551615");
    }

    #[test]
    fn applying_a_configuration_allocates_nothing() {
        let chip = fake_chip("alloc", true);
        let mut pwm = SysfsPwm::open(&chip, 1).unwrap();
        let (result, n) = allocations(|| pwm.configure(&config(2000, 500, true)));
        assert!(result.is_ok());
        assert_eq!(n, 0);
        drop(pwm);
        std::fs::remove_dir_all(chip).unwrap();
    }
}
