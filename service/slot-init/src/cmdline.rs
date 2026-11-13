//! The kernel command line's slot arguments. The bundle writes them into each slot's
//! `cmdline.txt` under one prefix, its `boot_args` setting, and the command line that boots is
//! that file byte for byte (its hash is in the signed manifest):
//!
//! ```text
//! <prefix>.slot=<id> <prefix>.roothash=<64 hex> ... panic=5
//! ```
//!
//! The prefix is whatever comes before `.slot=`, so the initramfs needs no setting of its own. The
//! data partition is not named here: it is found by a fixed rule ([`crate::boot`]).

/// The value of `key=` on `cmdline`, the last one when it is given twice (as the kernel takes
/// it).
pub fn arg<'a>(cmdline: &'a str, key: &str) -> Option<&'a str> {
    cmdline
        .split_ascii_whitespace()
        .filter_map(|word| word.strip_prefix(key)?.strip_prefix('='))
        .next_back()
}

/// The slot arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub prefix: String,
    pub slot: String,
    /// `""` when missing: the slot then fails, and falls back.
    pub roothash: String,
}

impl Args {
    /// # Errors
    ///
    /// No `<prefix>.slot=`: without it nothing can be found.
    pub fn parse(cmdline: &str) -> Result<Self, String> {
        let prefix = cmdline
            .split_ascii_whitespace()
            .find_map(|word| {
                let (key, _) = word.split_once('=')?;
                key.strip_suffix(".slot").filter(|p| !p.is_empty())
            })
            .ok_or("no <prefix>.slot= on the kernel command line")?;
        let slot = arg(cmdline, &format!("{prefix}.slot")).unwrap_or_default();
        Ok(Self {
            prefix: prefix.to_string(),
            slot: slot.to_string(),
            roothash: arg(cmdline, &format!("{prefix}.roothash"))
                .unwrap_or_default()
                .to_string(),
        })
    }

    /// The root hash on another slot's command line (its `cmdline.txt`), under this prefix.
    pub fn roothash_in(&self, cmdline: &str) -> String {
        arg(cmdline, &format!("{}.roothash", self.prefix))
            .unwrap_or_default()
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prefix_is_what_comes_before_slot() {
        let args = Args::parse("console=tty1 os.slot=os-2 os.roothash=ab panic=5").unwrap();
        assert_eq!(args.prefix, "os");
        assert_eq!(args.slot, "os-2");
        assert_eq!(args.roothash, "ab");
        assert_eq!(args.roothash_in("x os.roothash=cd y"), "cd");
        assert_eq!(arg("a=1 a=2", "a"), Some("2"));
    }

    #[test]
    fn the_slot_is_needed_and_no_data_argument_is_read() {
        assert!(Args::parse("console=tty1").is_err());
        assert!(Args::parse("os.roothash=b").is_err());
        let args = Args::parse("os.slot=a os.roothash=b os.data=PARTUUID=old-02").unwrap();
        assert_eq!(args.slot, "a");
    }
}
