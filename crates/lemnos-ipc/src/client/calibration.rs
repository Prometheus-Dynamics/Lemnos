//! `DeviceClient`'s calibration requests: start, stop, apply, discard and
//! reset a device's calibration, and read its status.

use super::{ClientError, DeviceClient, reply};
use crate::wire::{Message, Request};
use lemnos_device::{CalibrationCommand, CalibrationStatus};

impl DeviceClient {
    /// Starts, stops, applies, discards or resets `device`'s calibration (see
    /// [`CalibrationCommand`]). A device with no calibration refuses with
    /// [`Refusal::Unsupported`](crate::Refusal::Unsupported); one that is being
    /// read refuses with a [`Refusal::Device`](crate::Refusal::Device) of
    /// `Busy`, which is retryable.
    pub fn calibration(
        &mut self,
        device: &str,
        command: CalibrationCommand,
    ) -> Result<(), ClientError> {
        let id = self.conn.next_id;
        self.conn.next_id = self.conn.next_id.wrapping_add(1).max(1);
        let request = Request::Calibration {
            id,
            device: device.into(),
            command,
        };
        self.conn
            .request(&request, reply(id))?
            .map(|_| ())
            .map_err(ClientError::Refused)
    }

    /// `device`'s calibration status: its revision, the running routine and
    /// progress, whether a candidate or a failure is waiting, and the three
    /// parts (accelerometer, gyroscope, magnetometer).
    pub fn calibration_status(&mut self, device: &str) -> Result<CalibrationStatus, ClientError> {
        let id = self.conn.next_id;
        self.conn.next_id = self.conn.next_id.wrapping_add(1).max(1);
        let request = Request::CalibrationStatus {
            id,
            device: device.into(),
        };
        self.conn
            .request(&request, |m| match m {
                Message::CalibrationStatus {
                    id: got, status, ..
                } if *got == id => Some(Ok(*status)),
                Message::Reply {
                    id: got,
                    result: Err(refusal),
                } if *got == id => Some(Err(*refusal)),
                _ => None,
            })?
            .map_err(ClientError::Refused)
    }
}
