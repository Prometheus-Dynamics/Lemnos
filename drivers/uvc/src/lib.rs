//! A USB camera, served as `camera.frames/1`: one driver, no sensor, receiver or ISP.
//!
//! Settings come from the device's `config`:
//!
//! - `frame_bytes` (integer, required): the size of the buffers of the stream named `frames`, which
//!   is the device's `[devices.streams.frames] buffer_bytes`. At most
//!   [`MAX_FRAME_BYTES`](descriptors::MAX_FRAME_BYTES). A frame larger than this is dropped and
//!   counted.
//!
//! The device needs `usb`, the camera, and `frames`, the stream. Binding touches no USB device.
//! All the work is in [`Lifecycle::tick`], at most one USB transfer per tick.
//!
//! The host's own video driver is taken off the control and streaming interfaces before they are
//! claimed, on every attempt, since a reset or replug can bind it again. Each whole frame is
//! written into a buffer of the stream and published. With no consumer attached, or no buffer
//! free, the frame is dropped and counted, and capture carries on.
//!
//! A device that has gone (its handle answers `Unavailable`) can't be reached through that handle
//! again, so the driver's health becomes [`Health::Gone`]. The host then drops this driver and
//! binds it again under the failure rule, which opens whatever is plugged in under the board name
//! now. Nobody has to call a method for that: the tick that finds the handle dead is enough.
//! What was selected and whether it was streaming are not kept across it. A host that does not
//! bind again leaves the driver retrying for ever, and every method but `frame_stream` answers
//! `NO_DEVICE` until the handle answers again.
//!
//! An idle camera is sent nothing, so each idle tick reads the start of its descriptor to see
//! that the handle is alive. Idle ticks come after a method and when the backend hints that the
//! device may have changed, so an idle camera that is unplugged is noticed at the hint, or at the
//! next method without one.
//!
//! The driver has to be given a device by the board file, by port or by vendor and product. A
//! camera that is absent when the service starts is bound when it arrives.
//!
//! Everything from the device is untrusted: see the module docs of [`descriptors`] and
//! [`payload`]. A frame's identity holds its PTS and source clock reference, see
//! [`session::identity`]; the timestamp is the host's clock at the frame's end, not the device's.

pub mod descriptors;
pub mod payload;
pub mod probe;
pub mod session;

#[cfg(test)]
mod tests;

use lemnos_contract_camera_frames::{CameraFrames, Counters, FrameFormat, MAX_STREAM_NAME};
use lemnos_core::{
    Config, ConfigValue, Description, Health, Item, Lifecycle, MethodId, Result, Status,
    StreamHandle, UsbDeviceHandle, driver,
};

use descriptors::MAX_FRAME_BYTES;
use session::Session;

/// The stream a consumer opens, which is also the name of the need.
pub const STREAM_NAME: &str = "frames";

const COUNTERS: MethodId = MethodId {
    contract: 0,
    method: 8,
};
const START: MethodId = MethodId {
    contract: 0,
    method: 6,
};
const STOP: MethodId = MethodId {
    contract: 0,
    method: 7,
};

#[driver("uvc", provides = [CameraFrames])]
pub struct Uvc {
    #[needs]
    usb: UsbDeviceHandle,
    #[needs]
    frames: StreamHandle,
    session: Session,
}

impl Lifecycle for Uvc {
    type Needs = UvcNeeds;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;

    const TICKS: bool = true;

    fn start(needs: UvcNeeds, cfg: &Config<'_>) -> Result<Self> {
        let capacity = frame_bytes(cfg)?;
        Ok(Self {
            usb: needs.usb,
            frames: needs.frames,
            session: Session::new(capacity),
        })
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::action("start", START));
        out.add(Item::action("stop", STOP));
        out.add(Item::value("frames", COUNTERS).field("frames"));
        out.add(Item::value("dropped", COUNTERS).field("dropped"));
        out.add(Item::value("errors", COUNTERS).field("errors"));
        let state = self.session.state();
        out.add(Item::value(state, COUNTERS));
        if let Some(warning) = self.session.warning().filter(|warning| *warning != state) {
            out.add(Item::value(warning, COUNTERS));
        }
    }

    fn health(&self) -> Health {
        if self.session.is_gone() {
            // The handle is dead: only being bound again opens the device afresh.
            return Health::Gone(session::GONE);
        }
        match (self.session.fault(), self.session.warning()) {
            (Some(fault), _) => Health::Failed(fault),
            (None, Some(warning)) => Health::Degraded(warning),
            (None, None) => Health::Ok,
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.session.tick(&mut *self.usb, &mut *self.frames, now_ns)
    }
}

impl Uvc {
    /// `NO_DEVICE` while the device is gone, for a host that leaves this driver in place.
    fn present(&self) -> Result<()> {
        if self.session.is_gone() {
            Err(Status::NO_DEVICE)
        } else {
            Ok(())
        }
    }
}

impl CameraFrames for Uvc {
    fn format_count(&mut self) -> Result<u16> {
        self.present()?;
        let modes = self.session.modes().ok_or(Status::AGAIN)?;
        u16::try_from(modes.len()).map_err(|_| Status::IO)
    }

    fn format(&mut self, index: u16) -> Result<FrameFormat> {
        self.present()?;
        let mode = *self
            .session
            .modes()
            .ok_or(Status::AGAIN)?
            .get(usize::from(index))
            .ok_or(Status::NOT_FOUND)?;
        Ok(FrameFormat {
            fourcc: mode.fourcc,
            width: mode.width,
            height: mode.height,
            fps_milli: mode.fps_milli,
            max_frame_bytes: mode.max_frame_bytes,
        })
    }

    fn select_format(&mut self, index: u16) -> Result<()> {
        self.present()?;
        if self.session.is_started() {
            return Err(Status::BUSY);
        }
        let modes = self.session.modes().ok_or(Status::AGAIN)?;
        if usize::from(index) >= modes.len() {
            return Err(Status::NOT_FOUND);
        }
        self.session.select(index);
        Ok(())
    }

    fn selected(&mut self) -> Result<Option<u16>> {
        self.present()?;
        Ok(self.session.selected())
    }

    fn frame_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        let name = STREAM_NAME.as_bytes();
        into.get_mut(..name.len())
            .ok_or(Status::INVALID_ARGUMENT)?
            .copy_from_slice(name);
        const _: () = assert!(STREAM_NAME.len() <= MAX_STREAM_NAME);
        Ok(name.len())
    }

    fn start(&mut self) -> Result<()> {
        self.present()?;
        if self.session.selected().is_none() {
            return Err(Status::INVALID_ARGUMENT);
        }
        if !self.session.is_started() {
            self.session.start();
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.session.stop();
        self.present()?;
        Ok(())
    }

    fn counters(&mut self) -> Result<Counters> {
        self.present()?;
        let (frames, dropped, errors) = self.session.counters();
        Ok(Counters {
            frames,
            dropped,
            errors,
        })
    }
}

/// The `frame_bytes` config: the stream's buffer size, within what a frame can be.
fn frame_bytes(cfg: &Config<'_>) -> Result<usize> {
    match cfg.get("frame_bytes") {
        Some(ConfigValue::Int(bytes)) if bytes > 0 && bytes <= i64::from(MAX_FRAME_BYTES) => {
            usize::try_from(bytes).map_err(|_| Status::INVALID_ARGUMENT)
        }
        _ => Err(Status::INVALID_ARGUMENT),
    }
}
