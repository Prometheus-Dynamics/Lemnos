//! The count-based fallback's decision, as pure functions: what the last reset was, and what
//! lemnosd does when the firmware's boot count lands it on the previous slot. The lemnosd sequencing
//! (the record writes, the count clear, the watchdog service) uses these; it is not built here.
//!
//! Standing rule: health ignores power cycles. A board that browns out during boot must not lose its
//! default by that. Only a warm reset (`Cause::Restart`) that repeats against the same slot counts
//! toward a demotion, and only once it reaches the threshold; an unnamed reset counts nothing.

/// The power-on bit of the PM reset status (`chosen/bootloader/rsts`). Observed on the board: `0x1000`
/// after a power-on reset (the coordinator's board read, 2026-11-13). Its name and meaning are not in
/// the kernel sources read here; the observation is the only evidence for it.
pub const POWER_ON_RESET: u32 = 0x0000_1000;
/// The warm-reset value: `0x20` after a software restart. `0x1020` (see [`TRYBOOT_RESET`]) is the
/// reading on a tryboot boot. The kernel sources read here do not name these bits.
pub const WARM_RESET: u32 = 0x0000_0020;
/// The last reset's cause, as lemnosd sees it. Only the last reset is visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// A power-on reset: the power-on bit alone (partition bits aside). No other bit is set.
    Power,
    /// A warm reset: `0x20` or `0x1020` (a software restart, a tryboot boot). It counts an episode.
    Restart,
    /// A value no rule names (for example `0x0`, `0x40`, or any value not yet observed). It counts
    /// no episode, and the boot history keeps the raw value.
    Unknown,
}

/// The tryboot boot's full `rsts` value (observed `0x1020`: the power-on bit with the warm-reset bit).
pub const TRYBOOT_RESET: u32 = 0x0000_1020;

/// Classifies the last reset from the FULL `rsts` value, every bit: no bit is masked, and a value
/// is one of the observed readings or it is unknown. The rules:
/// - `0x1000` is `Power` (a power-on reset);
/// - `0x20` and `0x1020` are `Restart` (a warm reset: a software restart or a tryboot boot);
/// - any other value, including `0x0`, is `Unknown`: no episode is counted, and the boot history
///   keeps the raw value with `cause=unknown`, reported by name.
pub const fn cause(rsts: u32) -> Cause {
    match rsts {
        POWER_ON_RESET => Cause::Power,
        WARM_RESET | TRYBOOT_RESET => Cause::Restart,
        _ => Cause::Unknown,
    }
}

/// The default demotion threshold, from `[slots] demote_after_episodes`.
pub const DEFAULT_THRESHOLD: u8 = 2;

/// The threshold from the board file's integer, or the reason it is refused: at least 1, and at
/// most 255 (an episode count is one byte).
pub fn threshold_from_board(value: Option<i64>) -> Result<u8, &'static str> {
    match value {
        None => Ok(DEFAULT_THRESHOLD),
        Some(n) if n < 1 => Err("demote_after_episodes must be at least 1"),
        Some(n) => u8::try_from(n).map_err(|_| "demote_after_episodes must be at most 255"),
    }
}

/// The configured threshold, or the reason it is refused: at least 1.
pub fn threshold(value: Option<u8>) -> Result<u8, &'static str> {
    match value {
        None => Ok(DEFAULT_THRESHOLD),
        Some(0) => Err("demote_after_episodes must be at least 1"),
        Some(n) => Ok(n),
    }
}

/// What lemnosd does when it arrives on the previous slot by the count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The failed default's files do not match its manifest: demote now.
    DemoteNow,
    /// Files match, warm reset (`Restart`): record an episode with `episodes` total, then clear the count.
    /// Demotes only if `episodes >= threshold` (the caller applies that after the record is written).
    Episode {
        /// The episode count after this one.
        episodes: u8,
        /// Whether the threshold is reached with this episode.
        reached: bool,
    },
    /// Files match, `Power` or `Unknown`: no episode. Clear the count and stay on the previous.
    NoEpisode,
}

/// The decision for one arrival on the previous slot.
///
/// `files_match`: the failed default's boot files match its manifest. `episodes_before`: the episodes
/// already recorded against that slot (the record's value; 0 for none).
pub const fn decide(
    files_match: bool,
    cause: Cause,
    episodes_before: u8,
    threshold: u8,
) -> Decision {
    if !files_match {
        return Decision::DemoteNow;
    }
    match cause {
        // Health ignores power cycles, and an unnamed pattern is no evidence against the slot: no
        // episode. The boot history carries the raw value (`arrival::report`).
        Cause::Power | Cause::Unknown => Decision::NoEpisode,
        Cause::Restart => {
            let episodes = episodes_before.saturating_add(1);
            Decision::Episode {
                episodes,
                reached: episodes >= threshold,
            }
        }
    }
}

/// The episode count after a boot-good on the slot: always zero.
pub const fn after_boot_good() -> u8 {
    0
}

#[cfg(test)]
#[path = "fallback_tests.rs"]
mod tests;
