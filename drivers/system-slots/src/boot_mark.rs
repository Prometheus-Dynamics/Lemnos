//! Boot good once a boot: the mark a finished boot good leaves under `/run`.
//!
//! A default boot that has run its uptime is judged good once: its boot files are verified, the
//! device baseline is written and the boot count is cleared. That is storage work, so a daemon that
//! restarts must not do it again for the same boot. The driver writes this mark when the boot good
//! COMPLETED (verified and cleared), never when it began, so a daemon that died part way leaves no
//! mark and the next start does the whole of it. The mark names the kernel's boot id and the slot:
//! a mark from another boot, or for another slot, is not this boot's and is ignored. `/run` is
//! emptied by every boot; the boot id makes the mark safe even where it is not.
//!
//! The file is three lines, `boot <id>`, `slot <id>` and `at <unix seconds>`, replaced whole by a
//! rename.

use std::fs;
use std::io;
use std::path::Path;

/// Where the mark is kept unless the board file says (`[slots] boot_good_mark`). The daemon's
/// runtime directory, kept across the daemon's restarts within a boot.
pub(crate) const DEFAULT: &str = "/run/lemnos/boot-good";

/// A boot good that finished this boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Done {
    /// The slot it judged good.
    pub(crate) slot: String,
    /// When it finished, in seconds since the epoch.
    pub(crate) at_s: u64,
    /// Whether this daemon did it (`false`: an earlier daemon of the same boot).
    pub(crate) here: bool,
}

/// The mark at `path`, when it is this boot's (`boot`). A missing, unreadable or malformed mark, or
/// another boot's, is no mark.
pub(crate) fn read(path: &Path, boot: &str) -> Option<Done> {
    let text = fs::read_to_string(path).ok()?;
    let (mut seen_boot, mut slot, mut at_s) = (None, None, None);
    for line in text.lines() {
        match line.split_once(' ') {
            Some(("boot", v)) => seen_boot = Some(v),
            Some(("slot", v)) => slot = Some(v),
            Some(("at", v)) => at_s = v.parse::<u64>().ok(),
            _ => {}
        }
    }
    if boot.is_empty() || seen_boot? != boot {
        return None;
    }
    let slot = slot?;
    if slot.is_empty() {
        return None;
    }
    Some(Done {
        slot: slot.to_string(),
        at_s: at_s?,
        here: false,
    })
}

/// Writes the mark for this boot (`boot`): `slot` was judged good at `at_s`. Written beside and
/// renamed over, so a reader sees the old mark or the new one, never part of one.
pub(crate) fn write(path: &Path, boot: &str, slot: &str, at_s: u64) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?
        .to_string_lossy();
    let dir = path.parent().unwrap_or(Path::new("."));
    let beside = dir.join(format!(".{name}.new"));
    fs::write(&beside, format!("boot {boot}\nslot {slot}\nat {at_s}\n"))?;
    fs::rename(&beside, path)
}

/// `info`'s words for the boot good of this boot.
pub(crate) fn words(done: Option<&Done>, error: Option<&str>) -> String {
    let Some(done) = done else {
        return "not yet this boot".to_string();
    };
    let by = if done.here {
        "this daemon"
    } else {
        "an earlier daemon of this boot"
    };
    let mut text = format!(
        "ran this boot for {} at {} s, by {by}",
        done.slot, done.at_s
    );
    if let Some(error) = error {
        text.push_str(&format!(
            "; its mark couldn't be written ({error}), so a restarted daemon does it again"
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mark_is_read_back_for_its_boot_only() {
        let dir = lemnos_scratch::Scratch::adopt(
            std::env::temp_dir().join(format!("boot-mark-{}", std::process::id())),
        );
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("boot-good");
        assert_eq!(read(&path, "b1"), None, "no file is no mark");
        write(&path, "b1", "os-2", 1234).unwrap();
        assert_eq!(
            read(&path, "b1"),
            Some(Done {
                slot: "os-2".into(),
                at_s: 1234,
                here: false
            })
        );
        assert_eq!(read(&path, "b2"), None, "another boot's mark is ignored");
        assert_eq!(read(&path, ""), None, "an unknown boot matches nothing");
        fs::write(&path, "boot b1\nslot os-2\n").unwrap();
        assert_eq!(
            read(&path, "b1"),
            None,
            "a mark without its time is malformed"
        );
    }
}
