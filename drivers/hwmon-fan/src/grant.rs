//! The fan's attributes, handed to the daemon's user before the drop.
//!
//! The kernel makes a hwmon fan's `pwm1` and `pwm1_enable` root's (0644), and the daemon runs as its
//! own user without `CAP_DAC_OVERRIDE`, so its duty and mode writes would be refused. The root phase
//! calls [`grant_before_drop`] once per fan the board names, before the drop: it gives the fan's
//! attributes to the owner the daemon will run as (`root:<gid>`, mode 0660), the same grant the PWM
//! channels get (`pwm::grant`). The write the daemon makes after the drop is limited by the kernel's
//! permission check on those two attributes; the daemon keeps no capability for it.
//!
//! The directory is the one the driver itself binds ([`fan_dir`](crate::fan_dir)), so the attributes
//! granted are the ones it writes. `pwm1` must exist (the directory is found by it); `pwm1_enable` is
//! granted when the kernel has it.

use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt, chown};
use std::path::{Path, PathBuf};

use lemnos_core::Config;

/// The attributes the driver writes: the duty, and the mode that makes the duty take effect.
pub const GRANTED: [&str; 2] = ["pwm1", "pwm1_enable"];

/// Gives the fan a board file names to `owner` (`uid`, `gid`), mode 0660, and returns its directory.
///
/// # Errors
///
/// The fan's directory is not there (no `pwm1`), or an attribute can't be changed. The fan is then
/// refused by name at start; nothing else is changed.
pub fn grant_before_drop(cfg: &Config<'_>, owner: (u32, u32)) -> io::Result<PathBuf> {
    let dir = crate::fan_dir(cfg).map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no hwmon fan here: its directory has no pwm1",
        )
    })?;
    grant_dir(&dir, owner)?;
    Ok(dir)
}

/// Gives the attributes of the fan directory `dir` to `owner`. A missing `pwm1` is an error; a missing
/// `pwm1_enable` is left as it is.
///
/// # Errors
///
/// `pwm1` is missing, or an attribute can't be changed (by the process: root is needed to hand it over).
pub fn grant_dir(dir: &Path, owner: (u32, u32)) -> io::Result<()> {
    for name in GRANTED {
        let path = dir.join(name);
        match fs::metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound && name != "pwm1" => continue,
            Err(error) => return Err(error),
        }
        chown(&path, Some(owner.0), Some(owner.1))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o660))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    /// A scratch directory of this test, removed when it ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("hwmon-grant-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The owner this process can hand a file to without privilege: its own ids.
    fn own(dir: &Path) -> (u32, u32) {
        let meta = fs::metadata(dir).unwrap();
        (meta.uid(), meta.gid())
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().mode() & 0o777
    }

    #[test]
    fn pwm1_and_pwm1_enable_go_to_the_owner_in_mode_0660() {
        let scratch = Scratch::new("both");
        let dir = scratch.0.clone();
        fs::write(dir.join("pwm1"), "77\n").unwrap();
        fs::write(dir.join("pwm1_enable"), "2\n").unwrap();
        fs::set_permissions(dir.join("pwm1"), fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(dir.join("pwm1_enable"), fs::Permissions::from_mode(0o644)).unwrap();
        grant_dir(&dir, own(&dir)).unwrap();
        assert_eq!(mode(&dir.join("pwm1")), 0o660);
        assert_eq!(mode(&dir.join("pwm1_enable")), 0o660);
    }

    #[test]
    fn a_fan_without_pwm1_enable_is_granted_by_its_duty_alone() {
        let scratch = Scratch::new("duty-only");
        let dir = scratch.0.clone();
        fs::write(dir.join("pwm1"), "77\n").unwrap();
        fs::set_permissions(dir.join("pwm1"), fs::Permissions::from_mode(0o644)).unwrap();
        grant_dir(&dir, own(&dir)).unwrap();
        assert_eq!(mode(&dir.join("pwm1")), 0o660);
        assert!(!dir.join("pwm1_enable").exists());
    }

    #[test]
    fn a_directory_without_pwm1_is_refused_and_nothing_is_changed() {
        let scratch = Scratch::new("no-duty");
        let dir = scratch.0.clone();
        fs::write(dir.join("pwm1_enable"), "2\n").unwrap();
        fs::set_permissions(dir.join("pwm1_enable"), fs::Permissions::from_mode(0o644)).unwrap();
        let error = grant_dir(&dir, own(&dir)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(mode(&dir.join("pwm1_enable")), 0o644);
    }
}
