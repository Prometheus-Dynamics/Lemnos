//! The boot records' file formats: what tells the boot loader which slot to start. The
//! `system-slots` driver writes them; `lemnos-slot-init` reads them and, to fall back, writes a
//! trial into them.
//!
//! A pending trial carries its attempts: how many more boots may start it before it has to fall
//! back. The loader takes one before each trial boot (the static script for `uboot`, `slot-init`
//! for `tryboot` and `file`), so a boot that never reaches the daemon counts against it, and the
//! daemon gives its attempt back. See `docs/slots.md`, "How a trial resolves".

/// Attempts a trial gets when the board says nothing, and the most the loader's counter can hold.
pub const MAX_ATTEMPTS: u8 = 3;

/// The attempts a counter's text names: a whole number up to [`MAX_ATTEMPTS`]. Anything else is
/// none, which is what a trial with no counter is: it boots as it always did.
pub fn attempts(text: &str) -> Option<u8> {
    text.trim()
        .parse::<u8>()
        .ok()
        .filter(|n| *n <= MAX_ATTEMPTS)
}

/// The boot partition's copy of a trial's reason: `trial-reason`, beside the boot record. The
/// daemon writes it when it gives a trial up (and reads it when the state has no reason of its
/// own); `lemnos-slot-init` writes it when a trial's boot finds a piece of the kernel missing, and
/// holds the trial back while it says so. The loader itself never reads it.
pub const REASON_FILE: &str = "trial-reason";

/// The prefix of the reason `lemnos-slot-init` writes when the kernel lacks a piece of the boot
/// chain. A reason with this prefix holds the trial back (no attempt is taken for it) until the
/// daemon resolves the trial.
pub const KERNEL_GAP: &str = "the kernel lacks CONFIG_";

/// The board firmware's one-shot boot: `default.txt` and `trial.txt` on the boot partition, each
/// one `os_prefix` line, and a restart with [`tryboot::RESTART_ARG`] reading `trial.txt` once. The
/// firmware has no counter: a trial's attempts are the file [`tryboot::ATTEMPTS_FILE`] beside it,
/// which `lemnos-slot-init` takes one from before each trial restart.
pub mod tryboot {
    /// The default slot's file.
    pub const DEFAULT_FILE: &str = "default.txt";
    /// The trial slot's file.
    pub const TRIAL_FILE: &str = "trial.txt";
    /// The trial's attempts left, as text (see [`super::attempts`]).
    pub const ATTEMPTS_FILE: &str = "trial-attempts";
    /// The restart argument that boots the trial once.
    pub const RESTART_ARG: &str = "0 tryboot";
    /// Where the firmware says a boot was a trial: reads `1` (as a big-endian u32) when it was.
    pub const TRIAL_FLAG: &str = "proc/device-tree/chosen/bootloader/tryboot";

    /// The line that points the firmware at slot `id`.
    pub fn entry(id: &str) -> String {
        format!("os_prefix=slots/{id}/\n")
    }

    /// The slot a file's line points at.
    pub fn slot(text: &str) -> Option<&str> {
        let line = text.lines().next()?;
        let id = line.strip_prefix("os_prefix=slots/")?.strip_suffix('/')?;
        crate::store::valid_id(id).then_some(id)
    }
}

/// One `boot-record` file, `default=`, `trial=` and `trial_attempts=` lines, for a loader with one
/// fixed kernel: the initramfs picks the slot, takes one attempt from a pending trial, and clears
/// the trial when it has none left.
pub mod file {
    /// The record's file on the boot partition.
    pub const FILE: &str = "boot-record";

    /// The record's default, trial (`""` for none) and the trial's attempts left (0 with none).
    pub fn parse(text: &str) -> (String, String, u8) {
        let value = |key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
                .filter(|id| crate::store::valid_id(id))
                .unwrap_or("")
                .to_string()
        };
        let attempts = text
            .lines()
            .find_map(|l| l.strip_prefix("trial_attempts="))
            .and_then(super::attempts)
            .unwrap_or(0);
        (value("default"), value("trial"), attempts)
    }

    /// The text form.
    pub fn text(default: &str, trial: &str, attempts: u8) -> String {
        let mut text = format!("default={default}\n");
        if !trial.is_empty() {
            text.push_str(&format!("trial={trial}\ntrial_attempts={attempts}\n"));
        }
        text
    }
}

/// U-Boot's standard boot flow: `boot-record.env`, read by `env import -t`.
pub mod uboot {
    /// The record's file on the boot partition.
    pub const RECORD_FILE: &str = "boot-record.env";
    /// The record's keys, as the script imports them.
    pub const KEYS: [&str; 3] = ["default", "trial", "trial_attempts"];

    /// The record's values. `attempts` is what the script counts down: the boots that may still
    /// start `trial` (0 with none, or when it has run out).
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct Record {
        pub default: String,
        pub trial: String,
        pub attempts: u8,
    }

    impl Record {
        /// Reads the text form. Unknown lines and `#` comments are skipped, as `env import -t`
        /// skips what it isn't asked for; a value that isn't a slot id reads as none.
        pub fn parse(text: &str) -> Self {
            let mut record = Self::default();
            for line in text.lines() {
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                let id = || {
                    if crate::store::valid_id(value) {
                        value.to_string()
                    } else {
                        String::new()
                    }
                };
                match key {
                    "default" => record.default = id(),
                    "trial" => record.trial = id(),
                    "trial_attempts" => record.attempts = super::attempts(value).unwrap_or(0),
                    _ => {}
                }
            }
            record
        }

        /// The text form: one `name=value` line per key, in [`KEYS`] order, nothing else.
        pub fn text(&self) -> String {
            format!(
                "default={}\ntrial={}\ntrial_attempts={}\n",
                self.default, self.trial, self.attempts
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_record_reads_back_what_it_writes() {
        assert_eq!(tryboot::slot(&tryboot::entry("os-2")), Some("os-2"));
        assert_eq!(tryboot::slot("os_prefix=slots/../\n"), None);
        assert_eq!(
            file::parse(&file::text("a-1", "b-2", 2)),
            ("a-1".into(), "b-2".into(), 2)
        );
        assert_eq!(
            file::parse(&file::text("a-1", "", 0)),
            ("a-1".into(), String::new(), 0)
        );
        let record = uboot::Record {
            default: "a-1".into(),
            trial: "b-2".into(),
            attempts: 2,
        };
        assert_eq!(uboot::Record::parse(&record.text()), record);
        assert_eq!(
            uboot::Record::parse("default=a-1\ntrial=b-2\ntrial_attempts=9\n").attempts,
            0,
            "a count past the ladder is none"
        );
        assert_eq!(attempts("3"), Some(3));
        assert_eq!(attempts("4"), None);
        assert_eq!(attempts(""), None);
    }
}
