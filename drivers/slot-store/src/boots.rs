//! The boot history: one line per boot, kept with the slot records (`lemnos/boots`).
//!
//! On the data partition it is `board/slots/boots`; when the state directory can't be written it
//! is also kept on the boot partition as `boot-history`, beside `trial-reason`, which the loader
//! never reads. The reader takes whichever copy holds the later boot. A boot writes one entry
//! (the kernel's boot id is in it, so a driver restarted within a boot writes none more), and a
//! trial's entry is rewritten once when the trial resolves.
//!
//! An entry is one line of seven space-separated fields, a value's whitespace replaced by `_`:
//! `<n> <utc> <slot> <outcome> <reset> <requested> <boot>`. `outcome` is `on-trial`, `confirmed`,
//! `fell-back` or `unknown`; `reset` is the raw reset reason (`none` when unreadable); `requested`
//! is `by:<caller>` when the previous run asked for the restart through `system.reboot`, else `-`.
//! Nothing secret is stored.

use std::fs;
use std::io;
use std::path::PathBuf;

use crate::store::{Layout, now_utc, replace};

/// The data-partition copy, under the state directory.
pub const FILE: &str = "boots";
/// The boot-partition copy, when the state directory can't be written.
pub const BOOT_FILE: &str = "boot-history";
/// The entries kept when the board file gives no `boots_keep`.
pub const DEFAULT_KEEP: usize = 16;
/// The outcome an entry has while its trial is still running.
pub const ON_TRIAL: &str = "on-trial";

/// One boot's entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Monotonic: the last entry's plus one.
    pub n: u64,
    /// When the boot was recorded (UTC).
    pub utc: String,
    pub slot: String,
    pub outcome: String,
    pub reset: String,
    pub requested: String,
    /// The kernel's boot id.
    pub boot: String,
}

/// A value as one field: whitespace becomes `_`, an empty value `-`.
pub fn field(value: &str) -> String {
    let text: String = value
        .trim()
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect();
    if text.is_empty() { "-".into() } else { text }
}

impl Entry {
    fn line(&self) -> String {
        format!(
            "{} {} {} {} {} {} {}\n",
            self.n,
            field(&self.utc),
            field(&self.slot),
            field(&self.outcome),
            field(&self.reset),
            field(&self.requested),
            field(&self.boot)
        )
    }

    fn parse(line: &str) -> Option<Self> {
        let mut parts = line.split_whitespace();
        let entry = Self {
            n: parts.next()?.parse().ok()?,
            utc: parts.next()?.to_string(),
            slot: parts.next()?.to_string(),
            outcome: parts.next()?.to_string(),
            reset: parts.next()?.to_string(),
            requested: parts.next()?.to_string(),
            boot: parts.next()?.to_string(),
        };
        parts.next().is_none().then_some(entry)
    }
}

/// The entries in `text`, oldest first; a malformed line is skipped.
pub fn parse(text: &str) -> Vec<Entry> {
    text.lines().filter_map(Entry::parse).collect()
}

/// The entries as the file holds them.
pub fn render(entries: &[Entry]) -> String {
    entries.iter().map(Entry::line).collect()
}

fn data_path(layout: &Layout) -> PathBuf {
    layout.marker(FILE)
}

fn boot_path(layout: &Layout) -> PathBuf {
    layout.boot.join(BOOT_FILE)
}

fn last_n(entries: &[Entry]) -> u64 {
    entries.last().map_or(0, |entry| entry.n)
}

/// The history: the copy holding the later boot, the data partition's on a tie. Empty when
/// neither copy can be read.
pub fn load(layout: &Layout) -> Vec<Entry> {
    let data = fs::read_to_string(data_path(layout))
        .map(|text| parse(&text))
        .unwrap_or_default();
    let boot = fs::read_to_string(boot_path(layout))
        .map(|text| parse(&text))
        .unwrap_or_default();
    if last_n(&boot) > last_n(&data) {
        boot
    } else {
        data
    }
}

/// Writes `entries` to both copies, each as far as it can be: the data partition's, and the boot
/// partition's when the data one fails. Fails only when neither was written.
pub fn save(layout: &Layout, entries: &[Entry]) -> io::Result<()> {
    let text = render(entries);
    let data = fs::create_dir_all(layout.state_dir())
        .and_then(|()| replace(&data_path(layout), text.as_bytes()));
    match data {
        Ok(()) => Ok(()),
        Err(data_error) => replace(&boot_path(layout), text.as_bytes()).map_err(|_| data_error),
    }
}

/// Records boot `boot` (the kernel's id) with its slot, outcome, reset reason and request, keeping
/// the newest `keep` entries. The caller says the outcome: `ON_TRIAL` for a running trial, else
/// `unknown`. Returns `false` and writes nothing when this boot already has an entry.
pub fn append(
    layout: &Layout,
    boot: &str,
    slot: &str,
    outcome: &str,
    reset: &str,
    requested: &str,
    keep: usize,
) -> io::Result<bool> {
    let mut entries = load(layout);
    if entries
        .last()
        .is_some_and(|entry| entry.boot == field(boot))
    {
        return Ok(false);
    }
    entries.push(Entry {
        n: last_n(&entries) + 1,
        utc: now_utc(),
        slot: slot.to_string(),
        outcome: field(outcome),
        reset: reset.to_string(),
        requested: requested.to_string(),
        boot: field(boot),
    });
    let keep = keep.max(1);
    if entries.len() > keep {
        let drop = entries.len() - keep;
        entries.drain(..drop);
    }
    save(layout, &entries)?;
    Ok(true)
}

/// Sets the outcome of the newest entry for `slot` that is still on trial. A slot with no such
/// entry is left alone.
pub fn set_outcome(layout: &Layout, slot: &str, outcome: &str) -> io::Result<()> {
    let mut entries = load(layout);
    let Some(entry) = entries
        .iter_mut()
        .rev()
        .find(|entry| entry.slot == slot && entry.outcome == ON_TRIAL)
    else {
        return Ok(());
    };
    entry.outcome = field(outcome);
    save(layout, &entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::TempDir;

    fn layout(dir: &TempDir) -> Layout {
        Layout::new(dir.0.join("data"), dir.0.join("boot"))
    }

    #[test]
    fn an_entry_round_trips_and_a_bad_line_is_skipped() {
        let entry = Entry {
            n: 3,
            utc: "2026-11-13T10:00:00Z".into(),
            slot: "a".into(),
            outcome: ON_TRIAL.into(),
            reset: "wdt 32".into(),
            requested: "by:7".into(),
            boot: "b1".into(),
        };
        let text = format!("garbage\n{}", render(std::slice::from_ref(&entry)));
        assert_eq!(
            parse(&text),
            vec![Entry {
                reset: "wdt_32".into(),
                ..entry.clone()
            }]
        );
        assert!(
            parse("1 x a b c d").is_empty(),
            "six fields is not an entry"
        );
    }

    #[test]
    fn one_boot_writes_one_entry_and_the_history_is_bounded() {
        let dir = TempDir::new("boots-bounded");
        let layout = layout(&dir);
        assert!(append(&layout, "boot-1", "a", "unknown", "none", "-", 2).unwrap());
        assert!(
            !append(&layout, "boot-1", "a", "unknown", "none", "-", 2).unwrap(),
            "same boot: no entry"
        );
        assert!(append(&layout, "boot-2", "b", "unknown", "none", "-", 2).unwrap());
        assert!(append(&layout, "boot-3", "c", "unknown", "none", "-", 2).unwrap());
        let kept = load(&layout);
        assert_eq!(kept.len(), 2, "only the newest two are kept");
        assert_eq!(kept.iter().map(|e| e.n).collect::<Vec<_>>(), vec![2, 3]);
    }

    #[test]
    fn an_unwritable_state_directory_keeps_the_history_on_the_boot_partition() {
        let dir = TempDir::new("boots-fallback");
        let layout = layout(&dir);
        fs::create_dir_all(&layout.boot).unwrap();
        // A file where the state directory must be: nothing can be written there.
        fs::create_dir_all(&layout.data).unwrap();
        fs::create_dir_all(layout.data.join("board")).unwrap();
        fs::write(layout.data.join("board/slots"), b"blocked").unwrap();
        assert!(append(&layout, "boot-1", "a", "unknown", "none", "-", 4).unwrap());
        assert_eq!(
            fs::read_to_string(layout.boot.join(BOOT_FILE))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert_eq!(
            load(&layout)[0].boot,
            "boot-1",
            "the reader finds the boot copy"
        );
        assert!(append(&layout, "boot-2", "b", "unknown", "none", "-", 4).unwrap());
        assert_eq!(
            load(&layout).len(),
            2,
            "the next boot builds on the boot copy"
        );
    }

    #[test]
    fn a_trial_entry_is_rewritten_once_when_it_resolves() {
        let dir = TempDir::new("boots-outcome");
        let layout = layout(&dir);
        append(&layout, "boot-1", "a", "unknown", "none", "-", 16).unwrap();
        append(&layout, "boot-2", "t", ON_TRIAL, "none", "-", 16).unwrap();
        set_outcome(&layout, "t", "confirmed").unwrap();
        set_outcome(&layout, "t", "fell-back").unwrap();
        let entries = load(&layout);
        assert_eq!(
            entries[1].outcome, "confirmed",
            "a resolved entry is not rewritten again"
        );
        assert_eq!(entries[0].outcome, "unknown");
    }
}
