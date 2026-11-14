//! The good-boot deadline (M36): what counts as a good boot, and the board file's setting for how
//! long it may take.
//!
//! A good boot is two facts and nothing else: lemnosd has reached ready (its unit is active, which
//! for `Type=notify` means it sent `READY=1`), and the running slot's booted marker
//! (`board/slots/booted`, written by slot-init before `switch_root`) names a slot. Device health
//! is not one of them. The waiter that enforces the deadline is `lemnos-good-boot`
//! (`service/good-boot`): a systemd unit outside lemnosd, so it works when lemnosd is wedged or
//! never starts.

use crate::store::valid_id;

/// The deadline when the board file does not give one, in seconds.
pub const DEFAULT_DEADLINE_S: u32 = 300;
/// The shortest deadline a board may give: a slow first boot (a card, a late device) must fit.
pub const MIN_DEADLINE_S: u32 = 30;
/// The longest deadline a board may give.
pub const MAX_DEADLINE_S: u32 = 3600;
/// The board file key that gives the deadline (in the `system-slots` device's `config`).
pub const KEY: &str = "good_boot_s";
/// Why a value of [`KEY`] is refused, in words that follow the key.
pub const REASON: &str = "must be a whole number of seconds from 30 to 3600 (the default is 300): a boot that takes longer than the deadline is restarted";

/// The deadline the board file gives, from its `good_boot_s` value (`None` when it is not given).
///
/// # Errors
///
/// A refusal in words, naming the key: a value that is not an integer, or outside
/// [`MIN_DEADLINE_S`] to [`MAX_DEADLINE_S`].
pub fn deadline_from_board(value: Option<i64>) -> Result<u32, String> {
    let Some(value) = value else {
        return Ok(DEFAULT_DEADLINE_S);
    };
    let seconds = u32::try_from(value)
        .ok()
        .filter(|s| (MIN_DEADLINE_S..=MAX_DEADLINE_S).contains(s));
    seconds.ok_or_else(|| format!("{KEY} is {value}: {REASON}"))
}

/// The slot that a good boot runs, or `None` when the boot is not good.
///
/// `ready` is whether lemnosd has reached ready; `marker` is the text of the booted marker (the
/// slot id slot-init wrote, a trailing newline allowed). Nothing else is an input: a device that
/// is failing does not make a boot bad here.
pub fn good(ready: bool, marker: &str) -> Option<String> {
    let slot = marker.trim();
    (ready && valid_id(slot)).then(|| slot.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_key_is_the_stated_default() {
        assert_eq!(deadline_from_board(None), Ok(DEFAULT_DEADLINE_S));
        assert_eq!(DEFAULT_DEADLINE_S, 300);
    }

    #[test]
    fn a_value_in_range_is_taken_as_seconds() {
        assert_eq!(deadline_from_board(Some(30)), Ok(30));
        assert_eq!(deadline_from_board(Some(3600)), Ok(3600));
    }

    #[test]
    fn a_bad_value_is_refused_by_name_with_the_range() {
        for bad in [0, 29, 3601, -1, i64::from(u32::MAX) + 1] {
            let why = deadline_from_board(Some(bad)).unwrap_err();
            assert!(why.starts_with("good_boot_s is "), "{why}");
            assert!(why.contains("from 30 to 3600"), "{why}");
        }
    }

    #[test]
    fn good_needs_ready_and_a_valid_marker_and_nothing_else() {
        assert_eq!(good(true, "os-1\n"), Some("os-1".to_string()));
        assert_eq!(good(false, "os-1\n"), None, "not ready is not good");
        assert_eq!(good(true, ""), None, "a missing marker is not good");
        assert_eq!(
            good(true, "../etc\n"),
            None,
            "a marker that names no slot is not good"
        );
        assert_eq!(good(true, ".hidden\n"), None);
    }
}
