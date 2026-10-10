//! The service's calibration requests (`Request::Calibration`,
//! `Request::CalibrationStatus`) and the periodic save of changed calibrations
//! (`calibration.rs` has the files).

use super::Service;
use crate::calibration;
use crate::devices::Placement;
use lemnos_device::{CalibrationCommand, CalibrationStatus};
use lemnos_hal::ErrorKind;
use lemnos_ipc::Refusal;

impl Service {
    /// A calibration command (`Request::Calibration`), under the device's
    /// write policy. A device being read refuses with `Busy` (retry it); the
    /// fusion device takes only `reset`, which restarts its filter.
    pub(super) fn calibration(
        &mut self,
        client: usize,
        device: &str,
        command: CalibrationCommand,
    ) -> Result<(), Refusal> {
        let index = self.slot_of(device).ok_or(Refusal::UnknownDevice)?;
        if !self.slots[index].allows(&self.clients[client].name) {
            return Err(Refusal::NotAllowed);
        }
        let now_ms = self.now_ms();
        if self.slots[index].placement == Placement::Composite {
            if command != CalibrationCommand::Reset {
                return Err(Refusal::Unsupported);
            }
            let fi = self
                .fusions
                .iter()
                .position(|f| f.slot == index)
                .ok_or(Refusal::Device(ErrorKind::Unavailable))?;
            if let Some(status) = self.fusions[fi].reset(&mut self.slots) {
                self.status_event(index, status);
            }
            return Ok(());
        }
        let slot = &mut self.slots[index];
        if slot.busy {
            return Err(Refusal::Device(ErrorKind::Busy));
        }
        let Some(device_ref) = slot.device.as_mut() else {
            return Err(Refusal::Device(
                slot.error.unwrap_or(ErrorKind::Unavailable),
            ));
        };
        device_ref
            .calibration_command(command)
            .map_err(|kind| match kind {
                ErrorKind::Unsupported => Refusal::Unsupported,
                kind => Refusal::Device(kind),
            })?;
        slot.calibration_cache = device_ref.calibration_status();
        if command == CalibrationCommand::Apply {
            // An applied calibration is saved now, not at the next pass.
            calibration::persist(slot, &self.calibration_dir, now_ms, false);
        }
        Ok(())
    }

    /// A device's calibration status (`Request::CalibrationStatus`). While a
    /// read holds the device, the status is the last one seen.
    pub(super) fn calibration_status(
        &mut self,
        device: &str,
    ) -> Result<CalibrationStatus, Refusal> {
        let index = self.slot_of(device).ok_or(Refusal::UnknownDevice)?;
        let slot = &mut self.slots[index];
        if slot.placement == Placement::Composite {
            return Err(Refusal::Unsupported);
        }
        if let Some(device_ref) = slot.device.as_ref() {
            slot.calibration_cache = device_ref.calibration_status();
        } else if !slot.present {
            return Err(Refusal::Device(
                slot.error.unwrap_or(ErrorKind::Unavailable),
            ));
        } else if slot.busy && slot.calibration_cache.is_none() {
            // Being read for the first time: retryable.
            return Err(Refusal::Device(ErrorKind::Busy));
        }
        slot.calibration_cache.ok_or(Refusal::Unsupported)
    }

    /// Saves every device's calibration whose revision changed (once a minute
    /// per device, see `calibration.rs`).
    pub(super) fn poll_calibration(&mut self, now_ms: u64) {
        if now_ms < self.next_calibration_ms {
            return;
        }
        self.next_calibration_ms = now_ms + super::CALIBRATION_POLL_MS;
        for slot in self.slots.iter_mut() {
            calibration::persist(slot, &self.calibration_dir, now_ms, true);
        }
    }
}
