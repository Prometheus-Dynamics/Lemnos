//! The Raspberry Pi firmware's boot count, as lemnosd uses it: the production side of
//! [`lemnos_slot_store::arrival`]. Only the `tryboot` record has one (see [`crate::boot`]).
//!
//! The count is read from the device tree (`chosen/bootloader/count` and `rsts`, each four
//! big-endian bytes) and cleared through the firmware mailbox ([`lemnos_driver_rpi_firmware_sys`]).
//! Both locations are the board's settings: `device_tree` (`/proc/device-tree` by default) and
//! `vcio` (`/dev/vcio`). Lemnos clears the count at a boot that is good and just before a restart
//! it makes. slot-init clears it in one place only: before its restart into a trial (`0 tryboot`).
//!
//! The records the sequence writes (the episodes) are the boot partition's raw records through
//! [`crate::boot::BootRecord`]; the history and the state are the data partition's files.

use lemnos_contract_indicator::NO_PROGRESS;
use std::fs;
use std::io;
use std::path::PathBuf;

use lemnos_slot_store::arrival::{self, Actions};
use lemnos_slot_store::boots;
use lemnos_slot_store::store::{Layout, Meta, State, valid_id};

use crate::SystemSlots;
use crate::boot::BootRecord;
use crate::boot_guard;

/// The device-tree directory and the mailbox device, as the board file names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Firmware {
    /// The device tree (`/proc/device-tree`): `chosen/bootloader/{count,rsts}` are read from it.
    pub device_tree: PathBuf,
    /// The firmware mailbox device (`/dev/vcio`).
    pub vcio: PathBuf,
}

/// Clears the boot count. The real one is the firmware mailbox; the tests use a recorder.
pub trait ClearCount {
    /// Sets the firmware's boot count to zero.
    fn clear_count(&self) -> io::Result<()>;
}

impl ClearCount for Firmware {
    fn clear_count(&self) -> io::Result<()> {
        lemnos_driver_rpi_firmware_sys::Mailbox::open(&self.vcio)?.clear_boot_count()
    }
}

impl Firmware {
    /// The count as the full 32-bit word (the restart baseline keeps it whole, above 255 too).
    pub fn count_word(&self) -> io::Result<u32> {
        self.word("count")
    }

    /// The count this boot started with (`0` to `255`).
    pub fn count(&self) -> io::Result<u8> {
        u8::try_from(self.word("count")?).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
    }

    /// The reset status register of the last reset (the `rsts` value).
    pub fn rsts(&self) -> io::Result<u32> {
        self.word("rsts")
    }

    /// A four-byte big-endian property of `chosen/bootloader`.
    fn word(&self, name: &str) -> io::Result<u32> {
        let path = self.device_tree.join("chosen/bootloader").join(name);
        let bytes = fs::read(&path)?;
        let four = <[u8; 4]>::try_from(bytes.as_slice()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, format!("{}", path.display()))
        })?;
        Ok(u32::from_be_bytes(four))
    }
}

/// The production [`Actions`]: the history, the state, the record and the mailbox.
pub struct Fallback<'a, M: ClearCount> {
    pub layout: &'a Layout,
    pub record: &'a mut dyn BootRecord,
    pub mailbox: &'a M,
    /// This boot's id: the history entry it annotates.
    pub boot: &'a str,
    /// The count is the U-Boot form's restart count, cleared through the record (not the mailbox).
    pub restarts: bool,
}

/// The mailbox of a form with no firmware count. The U-Boot form clears its restart count through
/// the record instead ([`Fallback::restarts`]).
pub struct NoMailbox;

impl ClearCount for NoMailbox {
    fn clear_count(&self) -> io::Result<()> {
        Ok(())
    }
}

impl<M: ClearCount> Actions for Fallback<'_, M> {
    type Error = String;

    fn report(
        &mut self,
        arrival: &arrival::Arrival,
        cause: lemnos_slot_store::fallback::Cause,
    ) -> Result<(), String> {
        let text = format!(
            "previous-by-count:default={}:files={}:rsts={:#x}:cause={}:episodes={}",
            arrival.default,
            if arrival.files_match {
                "match"
            } else {
                "changed"
            },
            arrival.rsts,
            match cause {
                lemnos_slot_store::fallback::Cause::Power => "power",
                lemnos_slot_store::fallback::Cause::Restart => "restart",
                lemnos_slot_store::fallback::Cause::Unknown => "unknown (no episode counted)",
            },
            arrival.episodes_before,
        );
        boots::annotate(self.layout, self.boot, &text)
            .map(drop)
            .map_err(|e| format!("the boot history couldn't be written: {e}"))
    }

    fn mark_not_good(&mut self, slot: &str) -> Result<(), String> {
        let mut meta = Meta::load(self.layout, slot);
        meta.set("FAILED", "yes");
        meta.set(
            "FAIL_REASON",
            "the boot count fell back to its previous slot, and its boot files or episodes said it is broken",
        );
        meta.save(self.layout, slot)
            .map_err(|e| format!("couldn't mark {slot}: {e}"))
    }

    fn record_episode(&mut self, slot: &str, episodes: u8) -> Result<(), String> {
        self.record
            .set_episodes(slot, episodes)
            .map_err(|e| e.to_string())
    }

    fn point_default(&mut self, to: &str) -> Result<(), String> {
        crate::boot::loader_reads_record(&*self.record)
            .map_err(|e| format!("couldn't point the default at {to}: {e}"))?;
        self.record
            .set_default(to)
            .map_err(|e| format!("couldn't point the default at {to}: {e}"))?;
        let mut state = State::load(self.layout);
        let old = std::mem::replace(&mut state.default, to.to_string());
        state.previous = old;
        state
            .save(self.layout)
            .map_err(|e| format!("couldn't save the state: {e}"))
    }

    fn clear_episodes(&mut self, slot: &str) -> Result<(), String> {
        // Written only when the record names this slot: a boot that is good with no episodes
        // writes nothing to the boot partition.
        match self.record.episodes().map_err(|e| e.to_string())? {
            Some((named, _)) if named == slot => {
                self.record.clear_episodes().map_err(|e| e.to_string())
            }
            _ => Ok(()),
        }
    }

    fn clear_count(&mut self) -> Result<(), String> {
        if self.restarts {
            // A write only when the count is not already 0: a healthy boot writes nothing here.
            return match self.record.restarts().map_err(|e| e.to_string())? {
                None | Some(0) => Ok(()),
                Some(_) => self
                    .record
                    .set_restarts(0)
                    .map_err(|e| format!("{COUNT_UNCLEARED}: {e}")),
            };
        }
        self.mailbox
            .clear_count()
            .map_err(|e| format!("{COUNT_UNCLEARED}: {e}"))
    }
}

/// Keeps the restart baseline for a good boot: the firmware's count as it stands, before the clear
/// (M36b). Returns the count kept. The clear that follows may fail; the baseline stays, so the
/// restarts since this good boot are counted from it.
pub(crate) fn keep_baseline(
    record: &mut dyn BootRecord,
    firmware: &Firmware,
) -> Result<u32, String> {
    let count = firmware.count_word().map_err(|e| e.to_string())?;
    record.set_baseline(count).map_err(|e| e.to_string())?;
    Ok(count)
}

/// A boot whose records can't be read to say whose they are, once the daemon ran its uptime: the
/// board came up, so the firmware's count is cleared through the mailbox, and nothing is written to
/// the boot partition (no baseline, no episode). Without it, a board that only can't judge its records
/// would count every ordinary restart toward the initramfs's way back (slot-init's `FLASH_AFTER`) and
/// end in the firmware's flashing mode. The next boot reads a count below the baseline, as after a
/// power loss, and counts from there. `Ok` and `Err` both carry the note for the state.
pub(crate) fn clear_unjudged(mailbox: &dyn ClearCount) -> Result<String, String> {
    match mailbox.clear_count() {
        Ok(()) => Ok(
            "the boot records can't be read to say whose they are; the daemon ran its uptime, so the firmware's boot count is cleared (nothing is written to the boot partition)"
                .to_string(),
        ),
        Err(e) => Err(format!(
            "{COUNT_UNCLEARED} although the daemon ran its uptime ({e}); the boot records can't be read to say whose they are; tried again in 8 s"
        )),
    }
}

/// The start of the reason a boot count could not be cleared (the firmware mailbox is not
/// reachable). The restart is not refused: the boot is counted, and the state and the indicator say so.
pub(crate) const COUNT_UNCLEARED: &str = "the boot count couldn't be cleared";
/// The start of the state's and `info`'s words while the count is not cleared (M36c).
pub(crate) const UNCLEARED: &str = "the boot count is not cleared";
/// The first wait before a clear the firmware refused is tried again. Each later wait doubles.
pub(crate) const RETRY_FIRST_NS: u64 = 1_000_000_000;
/// The longest wait between two tries of a refused clear: the failure rule's cap (8 s).
pub(crate) const RETRY_CAP_NS: u64 = 8_000_000_000;
/// What `retrying` and `health` say the failure is, in words. The exact error is in `info`.
pub(crate) const RETRY_REASON: &str = "the firmware mailbox could not clear the boot count";

const NS_PER_S: u64 = 1_000_000_000;

/// A boot count the firmware refused to clear, and the retries still owed (M36c). The clear is
/// retried from the driver's tick until it succeeds: the wait starts at [`RETRY_FIRST_NS`],
/// doubles after each refusal and stops at [`RETRY_CAP_NS`]. Nothing gives up and nothing turns off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Uncleared {
    /// Clear attempts that were refused in a row, the first (the good boot's) included.
    pub failed: u32,
    /// The wait before the next attempt. The text names it.
    pub wait_ns: u64,
    /// When the next attempt is due, on the host's clock. `None` until a tick times the first wait.
    pub due_ns: Option<u64>,
    /// The mailbox's error, as the last attempt gave it.
    pub why: String,
}

impl Uncleared {
    /// The first refusal, with its first wait still to be timed by a tick.
    pub(crate) fn refused(why: &str) -> Self {
        Self {
            failed: 1,
            wait_ns: RETRY_FIRST_NS,
            due_ns: None,
            why: why.to_string(),
        }
    }

    /// The state's and `info`'s words: `the boot count is not cleared: <why>; retrying every Ns`.
    pub(crate) fn text(&self) -> String {
        format!(
            "{UNCLEARED}: the firmware mailbox could not clear it: {}; retrying every {}s",
            self.why,
            self.wait_ns / NS_PER_S
        )
    }
}

/// What a tick did about a refused clear.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Advance {
    /// Nothing is owed.
    Nothing,
    /// Not due yet: look again at this time.
    Later(u64),
    /// Tried and refused again: look again at this time.
    Failed(u64),
    /// Cleared, after this many tries (the first included).
    Cleared(u32),
}

/// Tries the clear when its wait is over. One attempt at most per call; the caller's tick asks again.
pub(crate) fn advance(
    pending: &mut Option<Uncleared>,
    now_ns: u64,
    mailbox: &dyn ClearCount,
) -> Advance {
    let Some(p) = pending.as_mut() else {
        return Advance::Nothing;
    };
    let due = *p.due_ns.get_or_insert(now_ns.saturating_add(p.wait_ns));
    if now_ns < due {
        return Advance::Later(due);
    }
    match mailbox.clear_count() {
        Ok(()) => {
            let tries = p.failed + 1;
            *pending = None;
            Advance::Cleared(tries)
        }
        Err(e) => {
            p.failed += 1;
            p.wait_ns = p.wait_ns.saturating_mul(2).min(RETRY_CAP_NS);
            p.why = e.to_string();
            let at = now_ns.saturating_add(p.wait_ns);
            p.due_ns = Some(at);
            Advance::Failed(at)
        }
    }
}

/// Advances a refused clear at `now_ns` and writes the state's words: the failure with its next
/// wait, or the cleared line. Returns when to look again, `None` when nothing is owed.
pub(crate) fn tick_pending(
    layout: &Layout,
    pending: &mut Option<Uncleared>,
    now_ns: u64,
    mailbox: &dyn ClearCount,
) -> Option<u64> {
    let (at, text) = match advance(pending, now_ns, mailbox) {
        Advance::Nothing => return None,
        Advance::Later(at) => return Some(at),
        Advance::Failed(at) => (
            Some(at),
            pending.as_ref().map(Uncleared::text).unwrap_or_default(),
        ),
        Advance::Cleared(tries) => (None, cleared_text(tries)),
    };
    let mut state = State::load(layout);
    state.error = text;
    let _ = state.save(layout);
    at
}

/// The line written once when a refused clear finally succeeds.
pub(crate) fn cleared_text(tries: u32) -> String {
    let word = if tries == 1 { "try" } else { "tries" };
    format!("the boot count is cleared after {tries} {word}")
}

/// `health` for a count that is not cleared: the wait is named in words (1, 2, 4 or 8 seconds).
pub(crate) fn health_text(wait_ns: u64) -> &'static str {
    match wait_ns / NS_PER_S {
        1 => {
            "the boot count is not cleared: the firmware mailbox could not clear it; retrying every 1s"
        }
        2 => {
            "the boot count is not cleared: the firmware mailbox could not clear it; retrying every 2s"
        }
        4 => {
            "the boot count is not cleared: the firmware mailbox could not clear it; retrying every 4s"
        }
        _ => {
            "the boot count is not cleared: the firmware mailbox could not clear it; retrying every 8s"
        }
    }
}

impl SystemSlots {
    /// The count-based fallback at start (see [`arrival`]): when this boot is the previous slot
    /// because the firmware's count picked it, the sequence runs and its outcome becomes the
    /// state's error, which `list` shows. Nothing runs when the count picks the default.
    pub(crate) fn arrive_at_start(&mut self) {
        let Some(firmware) = self.firmware.clone() else {
            return self.arrive_uboot();
        };
        let state = self.state();
        let running = self.running();
        if !valid_id(&running) || !valid_id(&state.default) || !valid_id(&state.previous) {
            return;
        }
        if running != state.previous || running == state.default {
            return;
        }
        let count = match firmware.count() {
            Ok(count) => count,
            Err(e) => return self.note(format!("the boot count can't be read: {e}")),
        };
        if lemnos_slot_store::firmware::pick(count) != lemnos_slot_store::firmware::Pick::Previous {
            return;
        }
        let rsts = match firmware.rsts() {
            Ok(rsts) => rsts,
            Err(e) => return self.note(format!("the reset status can't be read: {e}")),
        };
        let files_match = boot_guard::verified(
            &self.layout,
            &self.boot_node,
            self.record.as_ref(),
            &state.default,
        )
        .is_ok();
        let episodes_before = match self.record.episodes() {
            Ok(Some((slot, episodes))) if slot == state.default => episodes,
            Ok(_) => 0,
            Err(e) => return self.note(format!("the episodes can't be read: {e}")),
        };
        let seen = arrival::Arrival {
            default: state.default.clone(),
            previous: running.clone(),
            files_match,
            rsts,
            episodes_before,
            threshold: self.demote_after,
        };
        let mut actions = Fallback {
            layout: &self.layout,
            record: self.record.as_mut(),
            mailbox: &firmware,
            boot: &self.boot,
            restarts: false,
        };
        let outcome = arrival::on_arrival(&mut actions, &seen);
        if let Err(why) = &outcome
            && why.starts_with(COUNT_UNCLEARED)
        {
            return self.count_refused(why);
        }
        let text = match outcome {
            Ok(arrival::Outcome::DemotedAtOnce) => format!(
                "{running} booted by the count; {} doesn't match its manifest, so it is demoted and {running} is the default",
                state.default
            ),
            Ok(arrival::Outcome::Demoted { episodes }) => format!(
                "{running} booted by the count; {} had {episodes} episodes and is demoted",
                state.default
            ),
            Ok(arrival::Outcome::EpisodeRecorded { episodes }) => format!(
                "{running} booted by the count; episode {episodes} for {} recorded; the default is kept",
                state.default
            ),
            Ok(arrival::Outcome::NoEpisode) => match lemnos_slot_store::fallback::cause(rsts) {
                lemnos_slot_store::fallback::Cause::Power => {
                    format!(
                        "{running} booted by the count after a power reset; no episode is recorded"
                    )
                }
                _ => format!(
                    "{running} booted by the count after an unnamed reset (rsts={rsts:#x}); no episode is recorded, and the boot history keeps the raw value"
                ),
            },
            Err(why) => why,
        };
        self.note(text);
    }

    /// Boot good on `slot`: the manifest must match, then the restart baseline is kept (before the
    /// clear, so it stays when the clear fails), then the slot's episodes and the boot count go
    /// (see [`arrival::on_boot_good`]). A boot whose files don't match keeps its count, and says so.
    pub(crate) fn boot_good(&mut self, slot: &str) {
        let Some(firmware) = self.firmware.clone() else {
            return self.boot_good_uboot(slot);
        };
        if let Err(why) =
            boot_guard::verified(&self.layout, &self.boot_node, self.record.as_ref(), slot)
        {
            return self.note(format!(
                "{slot} is not good yet: its boot files don't match ({why}); the count is kept"
            ));
        }
        if let Err(why) = keep_baseline(self.record.as_mut(), &firmware) {
            self.note(format!(
                "the restart baseline couldn't be kept ({why}); the good boot's restarts are not reset in the record"
            ));
        }
        let mut actions = Fallback {
            layout: &self.layout,
            record: self.record.as_mut(),
            mailbox: &firmware,
            boot: &self.boot,
            restarts: false,
        };
        match arrival::on_boot_good(&mut actions, slot) {
            Ok(()) => self.count_cleared(),
            Err(why) if why.starts_with(COUNT_UNCLEARED) => self.count_refused(&why),
            Err(why) => self.note(why),
        }
    }

    /// The clear went through. A refused clear that was owed ends here, with its tries named once;
    /// a count an earlier boot left unnamed is named as cleared too. The indicator returns to idle.
    fn count_cleared(&mut self) {
        let tries = match self.uncleared.take() {
            Some(pending) => pending.failed + 1,
            None if self.state().error.starts_with(UNCLEARED) => 1,
            None => return,
        };
        self.note(cleared_text(tries));
        self.show("idle", NO_PROGRESS);
    }

    /// The clear was refused: the count is not cleared and a retry is owed. The restart is not
    /// refused. The state's words (and `info`, `health`) say so, and the indicator shows failed.
    fn count_refused(&mut self, why: &str) {
        let err = why
            .strip_prefix(COUNT_UNCLEARED)
            .and_then(|rest| rest.strip_prefix(": "))
            .unwrap_or(why);
        let pending = self
            .uncleared
            .get_or_insert_with(|| Uncleared::refused(err));
        pending.why = err.to_string();
        let text = pending.text();
        self.note(text);
        self.show("failed", NO_PROGRESS);
    }

    /// Tries a refused clear when it is due (see [`advance`]). Returns when to look again.
    pub(crate) fn count_tick(&mut self, now_ns: u64) -> Option<u64> {
        self.uncleared.as_ref()?;
        let firmware = self.firmware.clone()?;
        let at = tick_pending(&self.layout, &mut self.uncleared, now_ns, &firmware);
        if self.uncleared.is_none() {
            self.show("idle", NO_PROGRESS);
        }
        at
    }
}

impl SystemSlots {
    /// The count-based fallback on the U-Boot form (BB2): the Pi's sequence, with the restart count read
    /// from the U-Boot environment (`lemnos_restarts`). The script raises the count on every boot that is
    /// not power-on (with the reset word named), so an arrival on the previous slot by the count is a
    /// warm restart by construction: it is classified as one, with [`fallback::WARM_RESET`].
    fn arrive_uboot(&mut self) {
        let state = self.state();
        let running = self.running();
        if !valid_id(&running) || !valid_id(&state.default) || !valid_id(&state.previous) {
            return;
        }
        if running != state.previous || running == state.default {
            return;
        }
        // Not the U-Boot form: the record has no count, and nothing runs.
        let Ok(Some(restarts)) = self.record.restarts() else {
            return;
        };
        if lemnos_slot_store::firmware::pick(restarts)
            != lemnos_slot_store::firmware::Pick::Previous
        {
            return;
        }
        let files_match = boot_guard::verified(
            &self.layout,
            &self.boot_node,
            self.record.as_ref(),
            &state.default,
        )
        .is_ok();
        let episodes_before = match self.record.episodes() {
            Ok(Some((slot, episodes))) if slot == state.default => episodes,
            Ok(_) => 0,
            Err(e) => return self.note(format!("the episodes can't be read: {e}")),
        };
        let seen = arrival::Arrival {
            default: state.default.clone(),
            previous: running.clone(),
            files_match,
            rsts: lemnos_slot_store::fallback::WARM_RESET,
            episodes_before,
            threshold: self.demote_after,
        };
        let mut actions = Fallback {
            layout: &self.layout,
            record: self.record.as_mut(),
            mailbox: &NoMailbox,
            boot: &self.boot,
            restarts: true,
        };
        let text = match arrival::on_arrival(&mut actions, &seen) {
            Ok(arrival::Outcome::DemotedAtOnce) => format!(
                "{running} booted by the count; {} doesn't match its manifest, so it is demoted and {running} is the default",
                state.default
            ),
            Ok(arrival::Outcome::Demoted { episodes }) => format!(
                "{running} booted by the count; {} had {episodes} episodes and is demoted",
                state.default
            ),
            Ok(arrival::Outcome::EpisodeRecorded { episodes }) => format!(
                "{running} booted by the count; episode {episodes} for {} recorded; the default is kept",
                state.default
            ),
            Ok(arrival::Outcome::NoEpisode) => {
                format!("{running} booted by the count; no episode is recorded")
            }
            Err(why) => why,
        };
        self.note(text);
    }

    /// Boot good on the U-Boot form: the manifest must match, then the episodes and the restart count go
    /// (the count only when it is not already 0).
    fn boot_good_uboot(&mut self, slot: &str) {
        // Not the U-Boot form: nothing to clear.
        if !matches!(self.record.restarts(), Ok(Some(_))) {
            return;
        }
        if let Err(why) =
            boot_guard::verified(&self.layout, &self.boot_node, self.record.as_ref(), slot)
        {
            return self.note(format!(
                "{slot} is not good yet: its boot files don't match ({why}); the count is kept"
            ));
        }
        let mut actions = Fallback {
            layout: &self.layout,
            record: self.record.as_mut(),
            mailbox: &NoMailbox,
            boot: &self.boot,
            restarts: true,
        };
        if let Err(why) = arrival::on_boot_good(&mut actions, slot) {
            self.note(why);
        }
    }
}
