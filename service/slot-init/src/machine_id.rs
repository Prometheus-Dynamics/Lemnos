//! The machine id: derived at boot from the board serial, and bound read-only over the slot's own
//! `/etc/machine-id` before `switch_root` (`docs/slots.md`, "The machine id").
//!
//! The root is read-only, so systemd on it would take a random id on every boot, and the persistent
//! journal on `/data` would scatter over those ids. The id is the first 16 bytes of SHA-256 over a
//! fixed domain and the serial, as 32 lowercase hex digits and a newline. A refusal never blocks the
//! boot: its named reason goes to the kernel log, and the root's own (empty) file stays.

use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Where the derived id is staged, under the initramfs's root. The bind mount keeps the staged file
/// alive after `switch_root`, which is why it is staged here and not in the slot's `/run`.
pub const STAGED: &str = "run/lemnos/machine-id";
/// Where the id is bound, under the new root.
pub const TARGET: &str = "etc/machine-id";

pub use lemnos_identity::machine_id::derive;

/// The board serial as the command line's `<prefix>.serial.*` words name its source, read and
/// normalised by the same code the driver uses.
///
/// # Errors
///
/// When the command line names no serial source, names one wrongly, or the source cannot be read
/// or does not fit the serial format. The text is what the log says.
pub fn serial_from_cmdline(cmdline: &str, prefix: &str) -> Result<String, String> {
    let settings = lemnos_identity::settings_from_words(cmdline, prefix)?;
    if settings.kind.is_none() {
        return Err("the command line names no serial source: the board has no system-identity device with a serial".into());
    }
    lemnos_identity::read_serial(&settings)
}

/// A refusal by name. The boot goes on without a machine id in each case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The serial could not be had: no source on the command line, or the source failed.
    SerialAbsent(String),
    /// The root has no `/etc/machine-id` to bind over (or it is not a plain file). Nothing is created.
    TargetMissing(String),
    /// The id could not be staged, or the bind or its read-only remount failed.
    BindFailed(String),
}

impl Refusal {
    /// The reason's name, as the log line and the docs give it.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::SerialAbsent(_) => "machine-id-serial-absent",
            Self::TargetMissing(_) => "machine-id-target-missing",
            Self::BindFailed(_) => "machine-id-bind-failed",
        }
    }

    fn detail(&self) -> &str {
        match self {
            Self::SerialAbsent(detail) | Self::TargetMissing(detail) | Self::BindFailed(detail) => {
                detail
            }
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}): {}", self.reason(), self.detail())
    }
}

/// Derives the id of `serial`, stages it at `staged` (0444) and binds it read-only over `target`
/// with `bind`. Refusals are checked in the order the docs give: the serial, then the target (which
/// is never created), then the staging and the bind. A bind that fails leaves no staged file.
///
/// # Errors
///
/// The refusal, by name.
pub fn bind(
    staged: &Path,
    target: &Path,
    serial: &Result<String, String>,
    bind: impl FnOnce(&Path, &Path) -> Result<(), String>,
) -> Result<(), Refusal> {
    let serial = serial
        .as_deref()
        .map_err(|why| Refusal::SerialAbsent(why.to_string()))?;
    match fs::symlink_metadata(target) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => {
            return Err(Refusal::TargetMissing(format!(
                "{} is not a plain file in the new root",
                target.display()
            )));
        }
        Err(error) => {
            return Err(Refusal::TargetMissing(format!(
                "{}: {error}",
                target.display()
            )));
        }
    }
    stage(staged, &derive(serial)).map_err(|error| {
        Refusal::BindFailed(format!(
            "cannot stage the id at {}: {error}",
            staged.display()
        ))
    })?;
    if let Err(error) = bind(staged, target) {
        let _ = fs::remove_file(staged);
        return Err(Refusal::BindFailed(format!(
            "{}: {error}",
            target.display()
        )));
    }
    Ok(())
}

/// Writes `text` to `path` (0444), replacing any copy an earlier attempt left.
fn stage(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let _ = fs::remove_file(path);
    fs::write(path, text)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o444))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    #[test]
    fn the_serial_is_read_from_the_words_by_the_shared_reader() {
        let dir = lemnos_scratch::Scratch::adopt(
            std::env::temp_dir().join(format!("machine-id-serial-{}", std::process::id())),
        );
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let cmdline_file = dir.join("cmdline");
        fs::write(&cmdline_file, "console=tty1 serialno=0123456789ABCDEF\n").unwrap();
        let path = cmdline_file.display();
        let line = format!(
            "os.slot=a os.serial.kind=cmdline os.serial.key=serialno os.serial.path={path}"
        );
        assert_eq!(
            serial_from_cmdline(&line, "os"),
            Ok("0123456789abcdef".to_string())
        );
        let none = serial_from_cmdline("os.slot=a", "os").unwrap_err();
        assert!(none.contains("names no serial source"), "{none}");
        let no_key = serial_from_cmdline("os.serial.kind=cmdline", "os").unwrap_err();
        assert!(no_key.contains("serial_key"), "{no_key}");
    }

    /// A directory for one test's staged id and target, under a name of its own.
    /// A directory under TMPDIR, removed when the guard drops, pass or panic.
    struct Dir(PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn staging(name: &str) -> (Dir, PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("machine-id-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("new/etc")).unwrap();
        (
            Dir(dir.clone()),
            dir.join("run/lemnos/machine-id"),
            dir.join("new/etc/machine-id"),
        )
    }

    fn ok_serial() -> Result<String, String> {
        Ok("0123456789abcdef".into())
    }

    #[test]
    fn a_bound_id_is_staged_read_only_and_bound_over_the_target() {
        let (_dir, staged, target) = staging("bound");
        fs::write(&target, "").unwrap();
        let calls = RefCell::new(Vec::new());
        let result = bind(&staged, &target, &ok_serial(), |s, t| {
            calls.borrow_mut().push((s.to_path_buf(), t.to_path_buf()));
            Ok(())
        });
        assert_eq!(result, Ok(()));
        assert_eq!(
            calls.borrow().as_slice(),
            [(staged.clone(), target.clone())]
        );
        assert_eq!(
            fs::read_to_string(&staged).unwrap(),
            derive("0123456789abcdef")
        );
        assert_eq!(
            fs::metadata(&staged).unwrap().permissions().mode() & 0o777,
            0o444
        );
    }

    #[test]
    fn no_serial_is_refused_by_name_and_nothing_is_staged() {
        let (_dir, staged, target) = staging("no_serial");
        fs::write(&target, "").unwrap();
        let result = bind(&staged, &target, &Err("no source".into()), |_, _| {
            panic!("no bind")
        });
        let refusal = result.unwrap_err();
        assert_eq!(refusal.reason(), "machine-id-serial-absent");
        assert_eq!(refusal.to_string(), "(machine-id-serial-absent): no source");
        assert!(!staged.exists());
    }

    /// The target is checked before anything is written: a missing target creates nothing.
    #[test]
    fn a_missing_target_is_refused_by_name_and_nothing_is_created() {
        let (_dir, staged, target) = staging("missing");
        let result = bind(&staged, &target, &ok_serial(), |_, _| panic!("no bind"));
        assert_eq!(result.unwrap_err().reason(), "machine-id-target-missing");
        assert!(!target.exists(), "the target is not created");
        assert!(!staged.exists(), "nothing is staged");
    }

    #[test]
    fn a_target_that_is_a_symlink_is_refused_as_missing() {
        let (_dir, staged, target) = staging("symlink");
        let elsewhere = target.with_file_name("elsewhere");
        fs::write(&elsewhere, "").unwrap();
        std::os::unix::fs::symlink(&elsewhere, &target).unwrap();
        let result = bind(&staged, &target, &ok_serial(), |_, _| panic!("no bind"));
        assert_eq!(result.unwrap_err().reason(), "machine-id-target-missing");
    }

    /// A bind that fails is refused by name, and the staged copy is taken back.
    #[test]
    fn a_bind_that_fails_is_refused_by_name_and_leaves_no_staged_file() {
        let (_dir, staged, target) = staging("bind_fails");
        fs::write(&target, "").unwrap();
        let result = bind(&staged, &target, &ok_serial(), |_, _| Err("EPERM".into()));
        let refusal = result.unwrap_err();
        assert_eq!(refusal.reason(), "machine-id-bind-failed");
        assert!(refusal.to_string().contains("EPERM"));
        assert!(!staged.exists(), "a failed bind leaves no staged id");
    }
}
