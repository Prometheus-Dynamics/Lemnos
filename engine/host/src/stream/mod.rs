//! Frame streams: a pool of buffers and a shared ring between one producer and one consumer.
//!
//! A board file declares a device's streams (`[devices.streams.<name>]`). A driver takes one with
//! a `StreamHandle` need of the same name; a caller takes the other end with
//! [`Host::stream_attach`](crate::Host::stream_attach), once per stream. After that no frame goes
//! through the host's lock on the way to the consumer beyond what the producer itself holds, and
//! no frame crosses a socket.
//!
//! The ring's memory is shared with the consumer, which is not trusted. The pool in this module
//! never leaves the host. Every buffer number and fence read back from the ring is checked against
//! it, and a consumer that gets anything wrong loses the stream and nothing else.

mod endpoint;
mod pool;

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};

use lemnos_core::{CallerId, Status};
use lemnos_wire::ring::{Layout, STATE_CLOSED, WordMemory, heap_words};

use endpoint::StreamCore;
pub use endpoint::{BufferWriter, ProducerEndpoint};

use crate::board::DeviceSpec;

/// Writes the buffers a stream hands the consumer. One per attachment.
pub trait BufferMemory: Send + Sync {
    /// Copies `data` to `offset`. `false`, and nothing written, when it doesn't fit.
    fn write(&self, offset: usize, data: &[u8]) -> bool;
    /// Copies from `offset` into `out`. `false`, and nothing read, when it doesn't fit.
    fn read(&self, offset: usize, out: &mut [u8]) -> bool;
}

/// Wakes a waiting consumer. Called once per published frame, and must not block.
pub trait Doorbell: Send + Sync {
    fn ring(&self);
}

/// What a stream is backed by, for one attachment.
pub struct Backing {
    pub ring: Arc<dyn WordMemory>,
    pub buffers: Arc<dyn BufferMemory>,
    pub doorbell: Arc<dyn Doorbell>,
    /// Handed to the consumer as they are: the ring, the doorbell and the buffers. Empty for a
    /// consumer in this process.
    pub descriptors: Vec<OwnedFd>,
}

/// Makes the memory behind a stream. Called once per attachment, so a consumer that goes never
/// keeps a view of the next one's frames.
pub trait StreamAllocator: Send + Sync {
    fn allocate(&self, name: &str, layout: &Layout) -> Result<Backing, Status>;
}

/// Heap memory, for a consumer in the same process: a host that's a library rather than a service.
pub struct HeapAllocator;

struct HeapBuffers {
    bytes: Mutex<Vec<u8>>,
}

fn lock<T: ?Sized>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl BufferMemory for HeapBuffers {
    fn write(&self, offset: usize, data: &[u8]) -> bool {
        let Some(end) = offset.checked_add(data.len()) else {
            return false;
        };
        lock(&self.bytes).get_mut(offset..end).is_some_and(|to| {
            to.copy_from_slice(data);
            true
        })
    }

    fn read(&self, offset: usize, out: &mut [u8]) -> bool {
        let Some(end) = offset.checked_add(out.len()) else {
            return false;
        };
        lock(&self.bytes).get(offset..end).is_some_and(|from| {
            out.copy_from_slice(from);
            true
        })
    }
}

struct NoDoorbell;

impl Doorbell for NoDoorbell {
    fn ring(&self) {}
}

impl StreamAllocator for HeapAllocator {
    fn allocate(&self, _name: &str, layout: &Layout) -> Result<Backing, Status> {
        let pool = usize::try_from(layout.pool_bytes()).map_err(|_| Status::INVALID_ARGUMENT)?;
        Ok(Backing {
            ring: heap_words(layout.ring_words()),
            buffers: Arc::new(HeapBuffers {
                bytes: Mutex::new(vec![0; pool]),
            }),
            doorbell: Arc::new(NoDoorbell),
            descriptors: Vec::new(),
        })
    }
}

/// How a stream stands. Counts since the host started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StreamStats {
    /// Whether a consumer is attached.
    pub attached: bool,
    /// Whether a producer holds the stream's endpoint.
    pub claimed: bool,
    /// Consumers attached so far.
    pub attachments: u64,
    /// Frames the producer published.
    pub published: u64,
    /// Published frames that reached the ring.
    pub delivered: u64,
    /// Published frames dropped: no consumer, ring full, or from before this consumer attached.
    pub dropped: u64,
    /// Buffers the consumer gave back.
    pub returned: u64,
    /// Consumers cut off for breaking the rules.
    pub faults: u64,
    /// Buffers in each state, always summing to the pool.
    pub free: u32,
    pub producer_held: u32,
    pub consumer_held: u32,
}

/// What a consumer gets when it attaches.
pub struct Lease {
    pub layout: Layout,
    pub descriptors: Vec<OwnedFd>,
    pub ring: Arc<dyn WordMemory>,
    pub buffers: Arc<dyn BufferMemory>,
    pub doorbell: Arc<dyn Doorbell>,
}

/// Every declared stream. Built once and never resized.
pub(crate) struct Streams {
    allocator: Arc<dyn StreamAllocator>,
    table: BTreeMap<(u32, String), Arc<Mutex<StreamCore>>>,
}

impl Streams {
    pub(crate) fn new(allocator: Arc<dyn StreamAllocator>, devices: &[DeviceSpec]) -> Self {
        let mut table = BTreeMap::new();
        for (index, device) in devices.iter().enumerate() {
            for (name, spec) in &device.streams {
                let core = StreamCore::new(spec);
                table.insert((index as u32, name.clone()), Arc::new(Mutex::new(core)));
            }
        }
        Self { allocator, table }
    }

    fn core(&self, device: u32, name: &str) -> Option<&Arc<Mutex<StreamCore>>> {
        self.table.get(&(device, name.to_string()))
    }

    /// Takes the producer's end. A stream has one producer at a time.
    pub(crate) fn producer(&self, device: u32, name: &str) -> Result<ProducerEndpoint, Status> {
        let core = self.core(device, name).ok_or(Status::NOT_FOUND)?;
        ProducerEndpoint::claim(Arc::clone(core))
    }

    /// Takes the consumer's end, for `caller`, on memory no earlier consumer has seen.
    pub(crate) fn attach(
        &self,
        device: u32,
        name: &str,
        caller: CallerId,
    ) -> Result<Lease, Status> {
        let core = self.core(device, name).ok_or(Status::NOT_FOUND)?;
        let mut core = lock(core);
        let layout = core.layout();
        if core.is_attached() {
            return Err(Status::BUSY);
        }
        let backing = self.allocator.allocate(name, &layout)?;
        let Backing {
            ring,
            buffers,
            doorbell,
            descriptors,
        } = backing;
        core.attach(
            caller,
            Arc::clone(&ring),
            Arc::clone(&buffers),
            Arc::clone(&doorbell),
        )?;
        Ok(Lease {
            layout,
            descriptors,
            ring,
            buffers,
            doorbell,
        })
    }

    /// Gives up a stream `caller` holds.
    pub(crate) fn detach(&self, device: u32, name: &str, caller: CallerId) -> Result<(), Status> {
        let core = self.core(device, name).ok_or(Status::NOT_FOUND)?;
        lock(core).detach_if(caller)
    }

    /// Gives up everything `caller` held. Runs however the caller went.
    pub(crate) fn caller_gone(&self, caller: CallerId) {
        for core in self.table.values() {
            let _ = lock(core).detach_if(caller);
        }
    }

    pub(crate) fn stats(&self, device: u32, name: &str) -> Option<StreamStats> {
        self.core(device, name).map(|core| lock(core).stats())
    }

    /// Tells every consumer the host is going.
    pub(crate) fn close_all(&self) {
        for core in self.table.values() {
            lock(core).detach(STATE_CLOSED);
        }
    }
}
