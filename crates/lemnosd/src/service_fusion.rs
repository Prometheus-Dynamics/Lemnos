//! The service's fusion glue: building the fusion devices, keeping the IMU's
//! and magnetometer's internal subscriptions in step with them, and feeding
//! their samples to the filters (`fusion.rs` has the device).

use super::Service;
use crate::devices::Placement;
use crate::fusion::{self, Fusion};
use lemnos_device::{BoxedDevice, DeviceStatus, MAX_CHANNELS};

impl Service {
    /// Builds the fusion device in slot `index` once its IMU (and, in 9-axis
    /// mode, its magnetometer) can be used; until then its reason says what
    /// is missing, and it is tried again.
    pub(super) fn build_fusion(&mut self, index: usize, now_ms: u64) {
        let slot = &self.slots[index];
        if slot.present || now_ms < slot.next_build_ms {
            return;
        }
        match fusion::resolve(index, &self.slots) {
            Ok(fusion) => {
                let slot = &mut self.slots[index];
                slot.info = Some(&fusion::FUSION_INFO);
                slot.placement = Placement::Composite;
                slot.present = true;
                slot.reason.clear();
                // Its values come from the IMU's reads, never a read of its own.
                slot.next_read_us = u64::MAX;
                if let Some(status) = slot.set_status(DeviceStatus::Degraded, None) {
                    self.status_event(index, status);
                }
                self.fusions.push(fusion);
            }
            Err(why) => {
                let slot = &mut self.slots[index];
                slot.reason = why;
                slot.next_build_ms = now_ms + super::FUSION_RETRY_MS;
                if let Some(status) = slot.set_status(DeviceStatus::Missing, None) {
                    self.status_event(index, status);
                }
            }
        }
    }

    /// Keeps the IMU's and magnetometer's internal subscriptions in step with
    /// the fusion devices' subscribers.
    pub(super) fn sync_fusion(&mut self) {
        let now_us = self.now_us();
        fusion::sync(&self.fusions, &mut self.slots, now_us);
    }

    /// Writes the fans' hand-back plans for the stop helper.
    // The service logs to stderr (the journal).
    #[allow(clippy::print_stderr)]
    pub(super) fn save_fan_state(&self) {
        let path = crate::fans::fan_state_path(&self.socket);
        if let Err(error) = crate::fans::write_fan_state(&path, &self.fan_restore_targets()) {
            eprintln!("lemnosd: {}: {error}", path.display());
        }
    }

    /// Feeds slot `index`'s samples (oldest first, `period_us` apart, the last
    /// at `read_us`) to the fusion devices that use it: the IMU's to the
    /// filter, the magnetometer's to its field. Each fusion whose output
    /// changed publishes it and delivers it to its subscribers.
    pub(super) fn feed_fusion(
        &mut self,
        index: usize,
        samples: &[[i32; MAX_CHANNELS]],
        read_us: u64,
        period_us: u64,
    ) {
        let mut changed = Vec::new();
        for fi in 0..self.fusions.len() {
            let (imu, mag) = (self.fusions[fi].imu, self.fusions[fi].mag);
            let is_imu = imu == index;
            if !is_imu && mag != Some(index) {
                continue;
            }
            let imu_status = self.slots[imu]
                .device
                .as_ref()
                .and_then(BoxedDevice::calibration_status);
            let mag_status = mag
                .and_then(|m| self.slots[m].device.as_ref())
                .and_then(BoxedDevice::calibration_status);
            let Some(info) = self.slots[index].info else {
                continue;
            };
            let f = &mut self.fusions[fi];
            f.note_confidence(imu_status.as_ref(), mag_status.as_ref());
            let fed = if is_imu {
                f.feed_imu(info, samples, read_us, period_us)
            } else {
                let trusted = Fusion::mag_trusted(mag_status.as_ref());
                f.feed_mag(info, samples, read_us, period_us, trusted)
            };
            if fed {
                changed.push(fi);
            }
        }
        for fi in changed {
            let slot = self.fusions[fi].slot;
            if let Some(status) = self.fusions[fi].publish(&mut self.slots) {
                self.status_event(slot, status);
            }
            let (out_us, now_us) = (self.slots[slot].read_us, self.now_us());
            self.deliver(slot, out_us, now_us);
        }
    }
}
