//! Which buffer of the `output0` stream is where, and the rules that make the image the block
//! wrote for a job be the image the stream's consumer reads.
//!
//! The stream's memory is the host's: one dma-buf per buffer, made anew for every consumer. The
//! module writes output 0 of a job through the dma-buf it pinned for the job's buffer. So for
//! each buffer there are two questions, and a job is sent and its frame published only when both
//! answers are yes:
//!
//! - does the driver hold the buffer on the stream (it has never been published, or it came
//!   back), and
//! - is the dma-buf the module has for that buffer the one behind that hold.
//!
//! The second is settled by handing the module the hold's handle: the module pins it and makes
//! it the buffer's memory, or says it already is, or says a job has the buffer now (then it is
//! asked again when that job's frame is finished). A hold whose consumer has gone has no handle:
//! it is given up, and comes back as a hold on the next consumer's memory.
//!
//! The block is never stopped for any of this. Memory the block may still be writing stays the
//! module's, pinned, until the block is done with that buffer; only then is it swapped. An image
//! finished into a consumer's memory after that consumer left reaches no one.
//!
//! The receiver's driver (`drivers/rp1-csi2/src/relay.rs`) keeps the same rules for a stream
//! with no jobs.

use lemnos_core::Result;
use lemnos_hal::blocks::StreamFrame;
use lemnos_hal::{ErrorKind, StreamEndpoint};

use crate::fe_hw::{Finished, Hardware, Set};

/// The most buffers a stream may have (`LEMNOS_STREAM_MAX_OWN_BUFFERS`).
pub(super) const MAX_BUFFERS: usize = 64;

/// The producer's end of the `output0` stream.
pub(super) type Frames = dyn StreamEndpoint<Error = ErrorKind> + Send;

#[derive(Debug, Clone, Copy)]
struct Slot {
    /// The driver holds the buffer on the stream.
    held: bool,
    /// The module's memory for the buffer is this hold's.
    matched: bool,
    /// The module published the buffer and hasn't had it back.
    out: bool,
    /// A job on the buffer was sent and hasn't finished.
    posted: bool,
    /// The module refused the memory behind this hold.
    refused: bool,
}

/// The state of every buffer of the `output0` stream, on both sides.
#[derive(Debug)]
pub(super) struct Relay {
    slots: [Slot; MAX_BUFFERS],
    count: u32,
    /// The stream's buffers have no handles: there is nothing to give the block.
    no_handles: bool,
}

impl Relay {
    pub(super) const fn new() -> Self {
        Self {
            slots: [Slot {
                held: false,
                matched: false,
                out: false,
                posted: false,
                refused: false,
            }; MAX_BUFFERS],
            count: 0,
            no_handles: false,
        }
    }

    /// The module's stream is new: it has no buffer's memory, no job and has published nothing.
    /// What the driver holds on the `output0` stream, it still holds.
    pub(super) fn restart(&mut self, count: u32) {
        self.count = count.min(MAX_BUFFERS as u32);
        self.no_handles = false;
        for slot in &mut self.slots {
            slot.matched = false;
            slot.out = false;
            slot.posted = false;
            slot.refused = false;
        }
    }

    /// Whether the stream's buffers turned out to have no handles.
    pub(super) const fn no_handles(&self) -> bool {
        self.no_handles
    }

    /// Whether the module refused the memory behind a buffer the driver holds now.
    pub(super) fn refused(&self) -> bool {
        self.slots.iter().any(|s| s.held && s.refused)
    }

    fn slot(&mut self, buffer: u32) -> Option<&mut Slot> {
        if buffer < self.count {
            self.slots.get_mut(buffer as usize)
        } else {
            None
        }
    }

    /// Gives up a hold with nothing behind it. Publishing it reaches no one and frees it, and
    /// it comes back from `next_buffer` as a hold on whatever is behind the stream then.
    fn give_up(&mut self, buffer: u32, frames: &mut Frames) {
        if let Some(slot) = self.slot(buffer) {
            slot.held = false;
            slot.matched = false;
        }
        let _ = frames.publish(StreamFrame {
            buffer,
            bytes: 0,
            sequence: 0,
            identity: 0,
            timestamp_ns: 0,
        });
    }

    /// Makes the module's memory for `buffer` be what is behind the driver's hold on it, and
    /// then gives the module the buffer back if it was waiting for it. Nothing happens for a
    /// buffer the driver doesn't hold or that is already settled. An error means the module's
    /// stream is over.
    fn settle(&mut self, buffer: u32, frames: &mut Frames, hw: &mut dyn Hardware) -> Result<()> {
        if !self.slot(buffer).is_some_and(|s| s.held && !s.matched) {
            return Ok(());
        }
        let set = match frames.buffer_handle(buffer) {
            Ok(Some(handle)) => hw.set_buffer(buffer, handle.fd())?,
            Ok(None) => {
                self.give_up(buffer, frames);
                return Ok(());
            }
            Err(ErrorKind::Unsupported) => {
                self.no_handles = true;
                return Ok(());
            }
            // The stream says the driver doesn't hold it after all: believe the stream.
            Err(_) => {
                if let Some(slot) = self.slot(buffer) {
                    slot.held = false;
                }
                return Ok(());
            }
        };
        let Some(slot) = self.slot(buffer) else {
            return Ok(());
        };
        match set {
            Set::Done => {
                slot.matched = true;
                if core::mem::take(&mut slot.out) {
                    hw.give_back(buffer)?;
                }
            }
            Set::Busy => {}
            Set::Refused => slot.refused = true,
        }
        Ok(())
    }

    /// Settles every buffer the driver holds: after the module's stream was made anew.
    pub(super) fn settle_all(&mut self, frames: &mut Frames, hw: &mut dyn Hardware) -> Result<()> {
        for buffer in 0..self.count {
            self.settle(buffer, frames, hw)?;
        }
        Ok(())
    }

    /// Takes the buffers the stream has free (never handed out, given back, or a consumer's
    /// that left) and settles each. Bounded: a hold with nothing behind it is given up and may
    /// come straight back, and it is looked at once a call.
    pub(super) fn take(&mut self, frames: &mut Frames, hw: &mut dyn Hardware) -> Result<()> {
        for _ in 0..self.count {
            let Ok(Some(buffer)) = frames.next_buffer() else {
                break;
            };
            let Some(slot) = self.slot(buffer) else {
                continue;
            };
            slot.held = true;
            slot.matched = false;
            slot.refused = false;
            self.settle(buffer, frames, hw)?;
        }
        Ok(())
    }

    /// Gives up every settled hold whose consumer has gone, so its buffer comes back as the
    /// next consumer's without waiting for a job to be run on it first.
    pub(super) fn sweep(&mut self, frames: &mut Frames) {
        for buffer in 0..self.count {
            let settled = self.slot(buffer).is_some_and(|s| s.held && s.matched);
            if settled && matches!(frames.buffer_handle(buffer), Ok(None)) {
                self.give_up(buffer, frames);
            }
        }
    }

    /// Whether a job may be sent on `buffer`, and if so marks it sent: the driver holds the
    /// buffer, the module's memory for it is that hold's, the hold's consumer is still there,
    /// and neither the block nor a job already has it.
    pub(super) fn claim(&mut self, buffer: u32, frames: &mut Frames) -> bool {
        let free = self
            .slot(buffer)
            .is_some_and(|s| s.held && s.matched && !s.out && !s.posted);
        if !free {
            return false;
        }
        match frames.buffer_handle(buffer) {
            Ok(Some(_)) => {
                if let Some(slot) = self.slot(buffer) {
                    slot.posted = true;
                }
                true
            }
            Ok(None) => {
                self.give_up(buffer, frames);
                false
            }
            Err(_) => false,
        }
    }

    /// A job claimed on `buffer` was not sent after all: the buffer is free for the next one.
    pub(super) fn unclaim(&mut self, buffer: u32) {
        if let Some(slot) = self.slot(buffer) {
            slot.posted = false;
        }
    }

    /// The block finished a job. Its frame is published when the driver holds its buffer, the
    /// module's memory for it is that hold's, and the hold's consumer is still there: the
    /// consumer then reads what the block wrote. Otherwise nobody may see it: the buffer is
    /// settled again, or waits to be taken again. Whether it was published.
    pub(super) fn finished(
        &mut self,
        done: &Finished,
        frames: &mut Frames,
        hw: &mut dyn Hardware,
    ) -> Result<bool> {
        let Some(slot) = self.slot(done.buffer) else {
            return Ok(false);
        };
        slot.posted = false;
        slot.out = true;
        let ready = slot.held && slot.matched;
        if ready && matches!(frames.buffer_handle(done.buffer), Ok(Some(_))) {
            let frame = StreamFrame {
                buffer: done.buffer,
                bytes: done.bytes,
                sequence: done.sequence,
                identity: done.identity,
                timestamp_ns: done.timestamp_ns,
            };
            let published = frames.publish(frame).is_ok();
            if let Some(slot) = self.slot(done.buffer) {
                if published {
                    slot.held = false;
                    slot.matched = false;
                    return Ok(true);
                }
                // Not taken: the driver still holds it, and the module can have it again.
                slot.out = false;
            }
            hw.give_back(done.buffer)?;
            return Ok(false);
        }
        if ready {
            // The consumer left while the block was writing its memory.
            self.give_up(done.buffer, frames);
        } else {
            self.settle(done.buffer, frames, hw)?;
        }
        Ok(false)
    }
}
