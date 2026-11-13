//! A [`Bus`] over `lemnosd`'s socket, for a real board.
//!
//! Devices are the service's list; calls go over the socket; a frame stream is taken with
//! `lemnos-linux-stream`'s platform, so a stream whose buffers are dma-bufs maps each one. Time is
//! the board's: [`Frames::pop`] waits for a frame up to [`FRAME_WAIT`], so [`Bus::wait`] has
//! nothing to move.

use std::sync::Arc;
use std::time::Duration;

use lemnos_client::{Client, FrameStream, HeldFrame, RemoteCaller};
use lemnos_core::{Bound, Contract, Status};
use lemnos_linux_stream::LinuxStreams;
use lemnos_wire::ring::FrameSlot;

use crate::{Bus, Device, Error, Frames};

/// How long [`Frames::pop`] waits for a frame before answering that none came.
pub const FRAME_WAIT: Duration = Duration::from_millis(200);

/// The service, through one connection.
pub struct SocketBus {
    client: Client,
}

impl SocketBus {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// The connection, for a caller that needs what the bus does not offer.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Takes a device's frame stream: read-only, or writable for a stream the consumer fills.
    pub fn take_stream(
        &self,
        device: &Device,
        stream: &str,
        writable: bool,
    ) -> Result<FrameStream, Error> {
        let platform = Arc::new(LinuxStreams);
        let taken = if writable {
            self.client
                .open_stream_writable(device.index, stream, platform)
        } else {
            self.client.open_stream(device.index, stream, platform)
        };
        taken.map_err(|e| Error::Stream {
            stream: stream.to_string(),
            status: status(&e),
        })
    }
}

fn status(error: &lemnos_client::Error) -> Status {
    match error {
        lemnos_client::Error::Refused { status, .. } => *status,
        _ => Status::IO,
    }
}

impl Bus for SocketBus {
    type Caller<'a> = RemoteCaller;

    fn devices(&self, tag: &str) -> Vec<Device> {
        self.client
            .list()
            .unwrap_or_default()
            .into_iter()
            .filter(|d| d.tags.iter().any(|t| t == tag))
            .map(|d| Device {
                index: d.index,
                id: d.name,
            })
            .collect()
    }

    fn link(&self, device: &Device) -> Result<Option<Device>, Error> {
        let list = self.client.list().map_err(|e| Error::Call {
            step: "list",
            status: status(&e),
        })?;
        let link = list
            .iter()
            .find(|d| d.index == device.index)
            .ok_or_else(|| Error::NoSuchDevice(device.id.clone()))?
            .link
            .clone();
        let Some(link) = link else {
            return Ok(None);
        };
        list.into_iter()
            .find(|d| d.name == link)
            .map(|d| {
                Some(Device {
                    index: d.index,
                    id: d.name,
                })
            })
            .ok_or(Error::NoSuchDevice(link))
    }

    fn open<'a, C: Contract + ?Sized>(
        &'a self,
        device: &Device,
    ) -> Result<Bound<C, Self::Caller<'a>>, Error> {
        self.client
            .open::<C>(&device.id)
            .map_err(|_| Error::WrongContract {
                device: device.id.clone(),
                contract: C::TAG.as_str(),
            })
    }

    fn open_frames<'a>(
        &'a self,
        device: &Device,
        stream: &str,
    ) -> Result<Box<dyn Frames + 'a>, Error> {
        let stream = self.take_stream(device, stream, false)?;
        let held = (0..stream.layout().buffers).map(|_| None).collect();
        Ok(Box::new(SocketFrames { stream, held }))
    }

    fn wait(&self) -> Result<(), Error> {
        Ok(())
    }
}

/// A frame stream taken over the socket. Each frame handed out is kept by its buffer number until
/// it is released, since only the stream can give it back.
pub struct SocketFrames {
    stream: FrameStream,
    held: Vec<Option<HeldFrame>>,
}

impl Frames for SocketFrames {
    fn buffer_bytes(&self) -> usize {
        self.stream.layout().buffer_bytes as usize
    }

    fn pop(&mut self) -> Result<Option<FrameSlot>, Error> {
        let Some(frame) = self
            .stream
            .next_frame(FRAME_WAIT)
            .map_err(|_| Error::ReadFailed)?
        else {
            return Ok(None);
        };
        let slot = FrameSlot {
            buffer: frame.buffer(),
            bytes: u32::try_from(frame.len()).map_err(|_| Error::BadBuffer)?,
            sequence: frame.sequence(),
            identity: frame.identity(),
            timestamp_ns: frame.timestamp_ns(),
        };
        let place = self
            .held
            .get_mut(slot.buffer as usize)
            .ok_or(Error::BadBuffer)?;
        *place = Some(frame);
        Ok(Some(slot))
    }

    fn read(&mut self, frame: &FrameSlot, out: &mut [u8]) -> Result<(), Error> {
        let held = self
            .held
            .get(frame.buffer as usize)
            .and_then(Option::as_ref)
            .ok_or(Error::BadBuffer)?;
        let bytes = self.stream.bytes(held);
        if bytes.len() != out.len() {
            return Err(Error::ReadFailed);
        }
        out.copy_from_slice(bytes);
        Ok(())
    }

    fn release(&mut self, frame: FrameSlot) -> Result<(), Error> {
        let held = self
            .held
            .get_mut(frame.buffer as usize)
            .and_then(Option::take)
            .ok_or(Error::BadBuffer)?;
        self.stream
            .release(held)
            .map_err(|_| Error::ReturnQueueFull)
    }
}
