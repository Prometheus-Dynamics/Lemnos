//! A PWM channel through sysfs.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use lemnos_hal::ErrorKind;
use lemnos_hal::raw::{Polarity, PwmConfig, RawPwm};

use crate::{Claim, kind_of};

/// How long a freshly exported channel gets to appear, by the clock: the kernel makes it during the
/// export write, so the bound is met at once on a board, and a count of looks would run out early on a
/// loaded machine where whatever makes the directory is slower (a test's stand-in thread).
const EXPORT_BOUND: Duration = Duration::from_secs(1);
const EXPORT_WAIT: Duration = Duration::from_millis(2);

/// Waits for `root` to appear after an export, up to [`EXPORT_BOUND`].
fn wait_exported(root: &Path) -> io::Result<()> {
    let until = std::time::Instant::now() + EXPORT_BOUND;
    while !root.exists() {
        if std::time::Instant::now() >= until {
            return Err(io::ErrorKind::NotFound.into());
        }
        std::thread::sleep(EXPORT_WAIT);
    }
    Ok(())
}

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

/// Whether this process may write the attribute `path`, as a write would be checked: it is opened for
/// writing, and nothing is written. Any refusal is the kernel's `EACCES`, which the caller sees as
/// `PermissionDenied`.
fn may_write(path: &Path) -> io::Result<()> {
    fs::OpenOptions::new().write(true).open(path).map(|_| ())
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
            wait_exported(&root)?;
        }
        let pwm = Self {
            period: root.join("period"),
            duty: root.join("duty_cycle"),
            polarity: root.join("polarity"),
            enable: root.join("enable"),
            unexport,
            _claim: None,
        };
        // The daemon writes the four attributes after its drop. A channel whose attributes it may not
        // write yet is refused here, and the host binds it again under the failure rule, so it binds
        // once the grant has been made (the root phase's, or the hotplug rule's). Dropping this undoes
        // an export this call made.
        for attribute in [&pwm.period, &pwm.duty, &pwm.polarity, &pwm.enable] {
            may_write(attribute)?;
        }
        Ok(pwm)
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

/// The attributes of a channel the daemon writes after its drop, and so the ones the root phase
/// hands to the daemon's group. `export` and `unexport` stay root's: the daemon never exports.
const GRANTED: [&str; 4] = ["period", "duty_cycle", "polarity", "enable"];

/// The root phase's grant of one channel (`pwmchipN:CHANNEL`), before the drop. Exports it if the
/// kernel has not, waits for `pwmM` to appear (bounded), then gives the four attributes the daemon
/// writes to `owner` with mode 0660. Nothing else on the chip is touched. Returns the channel's real
/// directory, which is where the unit's `ReadWritePaths=` must reach.
///
/// # Errors
///
/// A chip directory that is not there, an export the kernel refused, a channel that does not
/// appear within the bound, or an attribute that cannot be handed over. The caller refuses the device
/// by name; the daemon keeps running.
pub(crate) fn grant(chip: &Path, channel: u32, owner: (u32, u32)) -> io::Result<PathBuf> {
    use std::os::unix::fs::{PermissionsExt, chown};
    if !chip.is_dir() {
        return Err(io::ErrorKind::NotFound.into());
    }
    let root = chip.join(format!("pwm{channel}"));
    if !root.exists() {
        fs::write(chip.join("export"), channel.to_string())?;
        wait_exported(&root)?;
    }
    for name in GRANTED {
        let path = root.join(name);
        chown(&path, Some(owner.0), Some(owner.1))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o660))?;
    }
    fs::canonicalize(&root)
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
    fn a_channel_the_daemon_may_not_write_is_refused_until_the_grant_is_made() {
        use std::os::unix::fs::PermissionsExt;
        let chip = fake_chip("ungranted", true);
        let root = chip.join("pwm1");
        let names = ["period", "duty_cycle", "polarity", "enable"];
        // The stand-in fails as the kernel's root-owned 0644 does: a write open of a 0444 attribute is
        // EACCES for a user. (Root writes past the mode bits, so this test must run as a user.)
        for name in names {
            std::fs::set_permissions(root.join(name), std::fs::Permissions::from_mode(0o444))
                .unwrap();
        }
        let refused = std::fs::OpenOptions::new()
            .write(true)
            .open(root.join("enable"))
            .unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            SysfsPwm::open(&chip, 1).err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
        for name in names {
            std::fs::set_permissions(root.join(name), std::fs::Permissions::from_mode(0o660))
                .unwrap();
        }
        let mut pwm = SysfsPwm::open(&chip, 1).unwrap();
        pwm.configure(&config(2000, 500, true)).unwrap();
        drop(pwm);
        std::fs::remove_dir_all(chip).unwrap();
    }

    #[test]
    fn a_real_root_owned_file_refuses_a_channel_the_same_way() {
        // /etc/passwd is root's and 0644: refused by the kernel for a user, and by the same check here.
        assert_eq!(
            may_write(Path::new("/etc/passwd")).err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
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

#[cfg(test)]
mod grant_tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, symlink};

    /// A scratch directory, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("pwm-grant-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn channel(chip: &Path, n: u32) {
        let root = chip.join(format!("pwm{n}"));
        fs::create_dir_all(&root).unwrap();
        for name in ["period", "duty_cycle", "polarity", "enable", "capture"] {
            fs::write(root.join(name), "0").unwrap();
        }
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().mode() & 0o777
    }

    /// The owner the test runs as: a file the test makes is the user's own, so handing it to that
    /// owner needs no privilege.
    fn me(dir: &Path) -> (u32, u32) {
        let meta = fs::metadata(dir).unwrap();
        (meta.uid(), meta.gid())
    }

    #[test]
    fn the_root_phase_grants_exactly_the_named_channel_and_nothing_else() {
        let scratch = Scratch::new("exact");
        let chip = scratch.0.join("pwmchip0");
        fs::create_dir_all(&chip).unwrap();
        fs::write(chip.join("export"), "").unwrap();
        fs::write(chip.join("unexport"), "").unwrap();
        channel(&chip, 0);
        channel(&chip, 1);
        let owner = me(&scratch.0);

        let real = grant(&chip, 1, owner).unwrap();
        assert_eq!(real, fs::canonicalize(chip.join("pwm1")).unwrap());
        for name in GRANTED {
            assert_eq!(mode(&chip.join("pwm1").join(name)), 0o660, "{name}");
        }
        // The channel it did not name, and the chip's own files, keep the mode they had.
        for name in ["period", "duty_cycle", "polarity", "enable", "capture"] {
            assert_ne!(mode(&chip.join("pwm0").join(name)), 0o660, "pwm0/{name}");
        }
        assert_ne!(mode(&chip.join("pwm1").join("capture")), 0o660);
        assert_ne!(mode(&chip.join("unexport")), 0o660);
        assert_ne!(mode(&chip.join("export")), 0o660);
    }

    #[test]
    fn an_absent_channel_is_exported_then_granted() {
        let scratch = Scratch::new("export");
        let chip = scratch.0.join("pwmchip0");
        fs::create_dir_all(&chip).unwrap();
        fs::write(chip.join("export"), "").unwrap();
        // The kernel's export makes pwm2; the test stands in for it after the write.
        let made = chip.clone();
        let writer = std::thread::spawn(move || {
            while fs::read_to_string(made.join("export"))
                .unwrap_or_default()
                .is_empty()
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            channel(&made, 2);
        });
        let owner = me(&scratch.0);
        let result = grant(&chip, 2, owner);
        writer.join().unwrap();
        result.unwrap();
        assert_eq!(fs::read_to_string(chip.join("export")).unwrap(), "2");
        assert_eq!(mode(&chip.join("pwm2/enable")), 0o660);
    }

    #[test]
    fn a_channel_the_kernel_never_makes_is_refused_within_the_bound() {
        let scratch = Scratch::new("never");
        let chip = scratch.0.join("pwmchip0");
        fs::create_dir_all(&chip).unwrap();
        fs::write(chip.join("export"), "").unwrap();
        let owner = me(&scratch.0);
        let error = grant(&chip, 5, owner).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn a_chip_that_is_not_there_is_refused_by_name() {
        let scratch = Scratch::new("nochip");
        let owner = me(&scratch.0);
        let error = grant(&scratch.0.join("pwmchip9"), 1, owner).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn the_grant_reports_the_real_directory_behind_a_chip_symlink() {
        let scratch = Scratch::new("link");
        let real = scratch.0.join("devices/platform/soc/pwm/pwm/pwmchip2");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("export"), "").unwrap();
        channel(&real, 1);
        let class = scratch.0.join("class");
        fs::create_dir_all(&class).unwrap();
        symlink(&real, class.join("pwmchip2")).unwrap();
        let owner = me(&scratch.0);
        let found = grant(&class.join("pwmchip2"), 1, owner).unwrap();
        assert_eq!(found, fs::canonicalize(real.join("pwm1")).unwrap());
    }
}
