//! The boot records' file formats: what tells the boot loader which slot to start. The
//! `system-slots` driver writes them; `lemnos-slot-init` reads them and, to fall back, writes a
//! trial into them.

/// The board firmware's one-shot boot: `default.txt` and `trial.txt` on the boot partition, each
/// one `os_prefix` line, and a restart with [`tryboot::RESTART_ARG`] reading `trial.txt` once.
pub mod tryboot {
    /// The default slot's file.
    pub const DEFAULT_FILE: &str = "default.txt";
    /// The trial slot's file.
    pub const TRIAL_FILE: &str = "trial.txt";
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

/// One `boot-record` file, `default=` and `trial=` lines, for a loader with one fixed kernel:
/// the initramfs picks the slot and clears the trial when it starts it.
pub mod file {
    /// The record's file on the boot partition.
    pub const FILE: &str = "boot-record";

    /// The record's default and trial (`""` for none).
    pub fn parse(text: &str) -> (String, String) {
        let value = |key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
                .filter(|id| crate::store::valid_id(id))
                .unwrap_or("")
                .to_string()
        };
        (value("default"), value("trial"))
    }

    /// The text form.
    pub fn text(default: &str, trial: &str) -> String {
        let mut text = format!("default={default}\n");
        if !trial.is_empty() {
            text.push_str(&format!("trial={trial}\n"));
        }
        text
    }
}

/// U-Boot's standard boot flow: `boot-record.env`, read by `env import -t`.
pub mod uboot {
    /// The record's file on the boot partition.
    pub const RECORD_FILE: &str = "boot-record.env";
    /// The record's keys, as the script imports them.
    pub const KEYS: [&str; 3] = ["default", "trial", "trial_armed"];

    /// The record's three values.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct Record {
        pub default: String,
        pub trial: String,
        pub armed: bool,
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
                    "trial_armed" => record.armed = value == "1",
                    _ => {}
                }
            }
            record
        }

        /// The text form: one `name=value` line per key, in [`KEYS`] order, nothing else.
        pub fn text(&self) -> String {
            format!(
                "default={}\ntrial={}\ntrial_armed={}\n",
                self.default,
                self.trial,
                u8::from(self.armed)
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
            file::parse(&file::text("a-1", "b-2")),
            ("a-1".into(), "b-2".into())
        );
        assert_eq!(
            file::parse(&file::text("a-1", "")),
            ("a-1".into(), String::new())
        );
        let record = uboot::Record {
            default: "a-1".into(),
            trial: "b-2".into(),
            armed: true,
        };
        assert_eq!(uboot::Record::parse(&record.text()), record);
    }
}
