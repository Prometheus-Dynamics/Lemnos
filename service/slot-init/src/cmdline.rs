//! The kernel command line's slot arguments. The bundle writes them into each slot's
//! `cmdline.txt` under one prefix, its `boot_args` setting, and the command line that boots is
//! that file byte for byte (its hash is in the signed manifest):
//!
//! ```text
//! <prefix>.slot=<id> <prefix>.roothash=<64 hex> <prefix>.src.<source>=<device-tree path> ... panic=5
//! ```
//!
//! The prefix is whatever comes before `.slot=`, so the initramfs needs no setting of its own. The
//! `.src.` words are the board's boot-source table (one per medium the board boots from; see
//! [`lemnos_slot_store::disk`]). The data partition is not named here: it is found on the boot disk
//! by a fixed rule ([`crate::boot`]).

/// The value of `key=` on `cmdline`, the last one when it is given twice (as the kernel takes
/// it).
pub fn arg<'a>(cmdline: &'a str, key: &str) -> Option<&'a str> {
    cmdline
        .split_ascii_whitespace()
        .filter_map(|word| word.strip_prefix(key)?.strip_prefix('='))
        .next_back()
}

/// The boot-source table on `cmdline`: `<prefix>.src.<source>=<path>` as `<source>=<path>`.
pub fn sources(cmdline: &str, prefix: &str) -> Vec<String> {
    let key = format!("{prefix}.src.");
    cmdline
        .split_ascii_whitespace()
        .filter_map(|word| {
            let (name, path) = word.split_once('=')?;
            Some(format!("{}={path}", name.strip_prefix(&key)?))
        })
        .collect()
}

/// The slot arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub prefix: String,
    pub slot: String,
    /// `""` when missing: the slot then fails, and falls back.
    pub roothash: String,
    /// The boot-source table: each `<prefix>.src.<source>=<path>` word as `<source>=<path>`, in
    /// order. Empty when the command line has none.
    pub sources: Vec<String>,
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
            sources: sources(cmdline, prefix),
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
    fn the_boot_source_table_is_read_in_order() {
        let args =
            Args::parse("os.slot=a os.src.mmc0=/soc/mmc@1 os.src.nvme0=/axi/pcie@2 x.src.usb0=/y")
                .unwrap();
        assert_eq!(args.sources, ["mmc0=/soc/mmc@1", "nvme0=/axi/pcie@2"]);
        assert!(Args::parse("os.slot=a").unwrap().sources.is_empty());
    }

    #[test]
    fn the_slot_is_needed_and_no_data_argument_is_read() {
        assert!(Args::parse("console=tty1").is_err());
        assert!(Args::parse("os.roothash=b").is_err());
        let args = Args::parse("os.slot=a os.roothash=b os.data=PARTUUID=old-02").unwrap();
        assert_eq!(args.slot, "a");
    }
}

/// The line `lemnos-ctl tree --cmdline-slot` writes, with the installer's root hash in place of
/// `@roothash@`: the parser reads it, in any word order, with other words beside it.
#[cfg(test)]
mod generated_line {
    use super::Args;

    const HASH: &str = "ab00000000000000000000000000000000000000000000000000000000000cd1";

    #[test]
    fn the_generated_line_with_the_hole_filled_is_read() {
        let line = format!("os.slot=os-1 os.roothash={HASH} panic=5\n");
        let args = Args::parse(&line).unwrap();
        assert_eq!(args.prefix, "os");
        assert_eq!(args.slot, "os-1");
        assert_eq!(args.roothash, HASH);
    }

    #[test]
    fn extra_words_in_any_order_and_unknown_words_are_ignored() {
        let line =
            format!("console=tty1 quiet panic=5 os.roothash={HASH} vendor.flag=1 os.slot=os-1\n");
        let args = Args::parse(&line).unwrap();
        assert_eq!(args.prefix, "os");
        assert_eq!(args.slot, "os-1");
        assert_eq!(args.roothash, HASH);
    }

    #[test]
    fn a_key_given_twice_takes_the_last() {
        let args = Args::parse(&format!("os.slot=os-1 os.slot=os-2 os.roothash={HASH}")).unwrap();
        assert_eq!(args.slot, "os-2");
    }
}
