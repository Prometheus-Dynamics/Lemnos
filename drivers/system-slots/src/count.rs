//! The Raspberry Pi firmware's boot count, as lemnosd uses it: the production side of
//! [`lemnos_slot_store::arrival`]. Only the `tryboot` record has one (see [`crate::boot`]).
//!
//! The count is read from the device tree (`chosen/bootloader/count` and `rsts`, each four
//! big-endian bytes) and cleared through the firmware mailbox ([`lemnos_driver_rpi_firmware_sys`]).
//! Both locations are the board's settings: `device_tree` (`/proc/device-tree` by default) and
//! `vcio` (`/dev/vcio`). Lemnos clears the count at a boot that is good and just before a restart
//! it makes; it never clears it in slot-init.
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
        self.mailbox
            .clear_count()
            .map_err(|e| format!("{COUNT_UNCLEARED}: {e}"))
    }
}

/// The start of the reason a boot count could not be cleared (the firmware mailbox is not
/// reachable). The restart is not refused: the boot is counted, and the state and the indicator say so.
pub(crate) const COUNT_UNCLEARED: &str = "the boot count couldn't be cleared";
/// The start of the state's reason while the boot is counted and not cleared.
pub(crate) const COUNTED_NOT_CLEARED: &str = "the boot is counted, not cleared";

impl SystemSlots {
    /// The count-based fallback at start (see [`arrival`]): when this boot is the previous slot
    /// because the firmware's count picked it, the sequence runs and its outcome becomes the
    /// state's error, which `list` shows. Nothing runs when the count picks the default.
    pub(crate) fn arrive_at_start(&mut self) {
        let Some(firmware) = self.firmware.clone() else {
            return;
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
        };
        let outcome = arrival::on_arrival(&mut actions, &seen);
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

    /// Boot good on `slot`: the manifest must match, then the slot's episodes and the boot count go
    /// (see [`arrival::on_boot_good`]). A boot whose files don't match keeps its count, and says so.
    pub(crate) fn boot_good(&mut self, slot: &str) {
        let Some(firmware) = self.firmware.clone() else {
            return;
        };
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
            mailbox: &firmware,
            boot: &self.boot,
        };
        match arrival::on_boot_good(&mut actions, slot) {
            Ok(()) => {
                // A count cleared again ends the loud state; the indicator returns to idle.
                if self.state().error.starts_with(COUNTED_NOT_CLEARED) {
                    self.note(String::new());
                    self.show("idle", NO_PROGRESS);
                }
            }
            Err(why) if why.starts_with(COUNT_UNCLEARED) => {
                // Loud, and not refused: the reason names the mailbox, and the indicator shows failed.
                self.note(format!(
                    "{COUNTED_NOT_CLEARED}: {why}; the restart is not refused"
                ));
                self.show("failed", NO_PROGRESS);
            }
            Err(why) => self.note(why),
        }
    }
}
