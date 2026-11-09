//! A [`Bus`] over a [`lemnos_host::Host`] in this process, with simulated time.
//!
//! Calls go through the host's own dispatch, so a driver's panic, error and backoff reach the tool
//! the way they would reach any caller. Frames come off the host's real ring. Time moves only when
//! [`Bus::wait`] moves it, to the next tick a device asked for.

use std::sync::Arc;
use std::time::Duration;

use lemnos_core::{Bound, Caller, CallerId, Contract, LOCAL_ARGS, MethodId, Result, Status};
use lemnos_host::Host;
use lemnos_host::stream::Lease;
use lemnos_sim::SimClock;
use lemnos_wire::ring::{Consumer, FrameSlot, WordMemory};

use crate::{Bus, Device, Error, Frames};

/// The host, and the simulated clock it was started with.
pub struct HostBus {
    host: Host,
    clock: SimClock,
}

impl HostBus {
    /// `clock` must be the clock the host was started with, or [`Bus::wait`] moves nothing.
    pub fn new(host: Host, clock: SimClock) -> Self {
        Self { host, clock }
    }

    /// The host, for a caller that needs what the bus does not offer.
    pub fn host(&self) -> &Host {
        &self.host
    }
}

/// Carries one contract's calls to a device on the host.
pub struct HostCaller<'a> {
    host: &'a Host,
    device: u32,
    contract: u8,
    args: [u8; LOCAL_ARGS],
}

impl Caller for HostCaller<'_> {
    fn args(&mut self) -> &mut [u8] {
        &mut self.args
    }

    fn call(&mut self, method: u16, args_len: usize, reply: &mut [u8]) -> Result<usize> {
        let args = self.args.get(..args_len).ok_or(Status::BUFFER_TOO_SMALL)?;
        let method = MethodId {
            contract: self.contract,
            method,
        };
        self.host.call(self.device, method, args, reply)
    }
}

/// A consumer's end of a stream on the host. Gives the stream back when dropped.
struct HostFrames<'a> {
    host: &'a Host,
    device: u32,
    stream: String,
    lease: Lease,
    consumer: Consumer,
}

impl Drop for HostFrames<'_> {
    fn drop(&mut self) {
        let _ = self
            .host
            .stream_detach(self.device, &self.stream, CallerId::LOCAL);
    }
}

impl Frames for HostFrames<'_> {
    fn buffer_bytes(&self) -> usize {
        self.lease.layout.buffer_bytes as usize
    }

    fn pop(&mut self) -> std::result::Result<Option<FrameSlot>, Error> {
        self.consumer.pop_frame().map_err(Error::Ring)
    }

    fn read(&mut self, frame: &FrameSlot, out: &mut [u8]) -> std::result::Result<(), Error> {
        let offset = self
            .lease
            .layout
            .buffer_offset(frame.buffer)
            .ok_or(Error::BadBuffer)?;
        if self.lease.buffers.read(offset, out) {
            Ok(())
        } else {
            Err(Error::ReadFailed)
        }
    }

    fn release(&mut self, frame: FrameSlot) -> std::result::Result<(), Error> {
        match self.consumer.release(frame.buffer, frame.fence) {
            Ok(true) => Ok(()),
            Ok(false) => Err(Error::ReturnQueueFull),
            Err(e) => Err(Error::Ring(e)),
        }
    }
}

impl Bus for HostBus {
    type Caller<'a>
        = HostCaller<'a>
    where
        Self: 'a;

    fn devices(&self, tag: &str) -> Vec<Device> {
        self.host
            .devices()
            .into_iter()
            .filter(|info| info.tags.iter().any(|t| t.as_str() == tag))
            .map(|info| Device {
                index: info.index,
                id: info.id,
            })
            .collect()
    }

    fn link(&self, device: &Device) -> std::result::Result<Option<Device>, Error> {
        let Some(id) = self.host.link(&device.id) else {
            return Ok(None);
        };
        let info = self
            .host
            .device(&id)
            .ok_or_else(|| Error::NoSuchDevice(id.clone()))?;
        Ok(Some(Device {
            index: info.index,
            id: info.id,
        }))
    }

    fn open<'a, C: Contract + ?Sized>(
        &'a self,
        device: &Device,
    ) -> std::result::Result<Bound<C, HostCaller<'a>>, Error> {
        let manifest = self
            .host
            .manifest(device.index)
            .ok_or_else(|| Error::NoSuchDevice(device.id.clone()))?;
        let wanted = <C as Contract>::TAG.as_str();
        let position = manifest
            .provides
            .iter()
            .position(|c| c.tag.as_str() == wanted)
            .ok_or_else(|| Error::WrongContract {
                device: device.id.clone(),
                contract: wanted,
            })?;
        let contract = u8::try_from(position).map_err(|_| Error::BadBuffer)?;
        Ok(Bound::new(HostCaller {
            host: &self.host,
            device: device.index,
            contract,
            args: [0; LOCAL_ARGS],
        }))
    }

    fn open_frames<'a>(
        &'a self,
        device: &Device,
        stream: &str,
    ) -> std::result::Result<Box<dyn Frames + 'a>, Error> {
        let lease = self
            .host
            .stream_attach(device.index, stream, CallerId::LOCAL)
            .map_err(|status| Error::Stream {
                stream: stream.to_string(),
                status,
            })?;
        let ring: Arc<dyn WordMemory> = Arc::clone(&lease.ring);
        let consumer = Consumer::attach(ring, lease.layout).map_err(Error::Ring)?;
        Ok(Box::new(HostFrames {
            host: &self.host,
            device: device.index,
            stream: stream.to_string(),
            lease,
            consumer,
        }))
    }

    fn wait(&self) -> std::result::Result<(), Error> {
        let next = self.host.next_tick_ns().ok_or(Error::NoTicks)?;
        let now = self.clock.now_ns();
        self.clock
            .advance(Duration::from_nanos(next.saturating_sub(now)));
        self.host.run_ticks();
        Ok(())
    }
}
