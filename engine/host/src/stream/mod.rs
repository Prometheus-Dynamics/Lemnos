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
//!
//! A stream's buffers can also be dma-bufs, one per buffer, for a driver whose hardware takes an
//! image only as a kernel buffer handle. The board file says so (`memory = "dma-buf"`), and the
//! host then asks its [`StreamAllocator`] for them with
//! [`allocate_handles`](StreamAllocator::allocate_handles). The allocator puts them in
//! [`Backing::handles`], and the driver borrows the one behind a buffer it holds with
//! `StreamEndpoint::buffer_handle`. An allocator that can't make them fails the attachment: a
//! stream declared as dma-bufs is never quietly made of anything else. The checks are the same
//! either way, and so is the memory's life: it is made for one attachment and never reaches the
//! next consumer.
//!
//! A buffer of dma-bufs can be several planes (`planes` in the board file), each its own
//! dma-buf, for hardware that writes a frame to several places of its own. The allocator makes
//! them with [`allocate_planes`](StreamAllocator::allocate_planes), and the driver borrows each
//! with `StreamEndpoint::plane_handle`. `write` and `read` address a buffer's planes end to end.
//!
//! A stream kept on standby (`standby = true`) has memory in place while no consumer is
//! attached: it is made when the producer next asks for a buffer, the producer fills it as it
//! would a consumer's, and the newest frame published is kept. The next consumer to attach is
//! given that memory, and the kept frame is on its ring the moment it attaches. When it leaves,
//! its memory goes with it and fresh memory is made, so no consumer ever sees another's.

mod endpoint;
mod pool;

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use lemnos_core::{CallerId, Status};
use lemnos_wire::Planes;
use lemnos_wire::ring::{Layout, STATE_CLOSED, WordMemory, heap_words};

use endpoint::StreamCore;
pub use endpoint::{BufferWriter, ProducerEndpoint};

use crate::board::DeviceSpec;
pub use crate::board::StreamMemory;

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
    /// The buffers as hardware takes them, when they are dma-bufs: one descriptor per plane of
    /// each buffer, buffer 0's planes first, each holding that plane from its first byte and
    /// at least as long as the plane (a buffer of one plane: the layout's `buffer_bytes`).
    /// `buffers` must reach the same memory, a buffer's planes end to end, so that what a
    /// producer writes and what its hardware writes land in one place. `None` when the buffers
    /// are plain memory. An attachment whose count differs from the layout's is refused.
    pub handles: Option<Arc<[OwnedFd]>>,
}

/// Makes the memory behind a stream. Called once per attachment, so a consumer that goes never
/// keeps a view of the next one's frames.
pub trait StreamAllocator: Send + Sync {
    /// Memory for a stream the board file leaves as shared memory.
    fn allocate(&self, name: &str, layout: &Layout) -> Result<Backing, Status>;

    /// Memory for a stream the board file declares as dma-bufs from `heap`: the backing's
    /// [`handles`](Backing::handles) must be `Some`, one per buffer. The status of a failure
    /// goes to whoever asked for the stream. An allocator that has no dma-bufs to give keeps
    /// this, which refuses.
    fn allocate_handles(&self, name: &str, layout: &Layout, heap: &str) -> Result<Backing, Status> {
        let _ = (name, layout, heap);
        Err(Status::NOT_SUPPORTED)
    }

    /// Memory for a stream of dma-bufs whose buffers are `planes`: the backing's
    /// [`handles`](Backing::handles) must be one per plane of each buffer, buffer 0's planes
    /// first. An allocator that makes only whole buffers keeps this, which answers a buffer of
    /// one plane with [`allocate_handles`](Self::allocate_handles) and refuses more.
    fn allocate_planes(
        &self,
        name: &str,
        layout: &Layout,
        heap: &str,
        planes: &Planes,
    ) -> Result<Backing, Status> {
        if planes.count() == 1 {
            self.allocate_handles(name, layout, heap)
        } else {
            Err(Status::NOT_SUPPORTED)
        }
    }
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
            handles: None,
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
    /// Holding the newest frame for the next consumer, on a stream kept on standby: 0 or 1.
    pub kept: u32,
    /// Whether memory is waiting for the next consumer, on a stream kept on standby.
    pub standby: bool,
}

/// What a consumer gets when it attaches.
pub struct Lease {
    pub layout: Layout,
    pub descriptors: Vec<OwnedFd>,
    pub ring: Arc<dyn WordMemory>,
    pub buffers: Arc<dyn BufferMemory>,
    pub doorbell: Arc<dyn Doorbell>,
    /// The buffers as dma-bufs, when they are: see [`Backing::handles`].
    pub handles: Option<Arc<[OwnedFd]>>,
    /// Each buffer's planes. One plane of the whole buffer unless the board file gives more.
    pub planes: Planes,
}

/// Every declared stream: those of the board file's devices, and of each device an `[[each]]`
/// entry makes, added as it is made. None is ever taken away.
pub(crate) struct Streams {
    allocator: Arc<dyn StreamAllocator>,
    table: RwLock<Table>,
}

/// Each stream, by its device's index and its name.
type Table = BTreeMap<(u32, String), Arc<Mutex<StreamCore>>>;

impl Streams {
    pub(crate) fn new(allocator: Arc<dyn StreamAllocator>, devices: &[DeviceSpec]) -> Self {
        let streams = Self {
            allocator,
            table: RwLock::new(BTreeMap::new()),
        };
        for (index, device) in devices.iter().enumerate() {
            streams.add(index as u32, device);
        }
        streams
    }

    /// Adds the streams of the device at `index`.
    pub(crate) fn add(&self, index: u32, device: &DeviceSpec) {
        let mut table = self.table.write().unwrap_or_else(PoisonError::into_inner);
        for (name, spec) in &device.streams {
            let core = StreamCore::new(spec, name, Arc::clone(&self.allocator));
            table.insert((index, name.clone()), Arc::new(Mutex::new(core)));
        }
    }

    fn core(&self, device: u32, name: &str) -> Option<Arc<Mutex<StreamCore>>> {
        self.table
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(device, name.to_string()))
            .cloned()
    }

    /// Runs `f` on every stream, under the table's lock. Allocates nothing.
    fn each(&self, mut f: impl FnMut(&mut StreamCore)) {
        let table = self.table.read().unwrap_or_else(PoisonError::into_inner);
        for core in table.values() {
            f(&mut lock(core));
        }
    }

    /// Takes the producer's end. A stream has one producer at a time.
    pub(crate) fn producer(&self, device: u32, name: &str) -> Result<ProducerEndpoint, Status> {
        let core = self.core(device, name).ok_or(Status::NOT_FOUND)?;
        ProducerEndpoint::claim(core)
    }

    /// Takes the consumer's end, for `caller`, on memory no earlier consumer has seen: the
    /// memory kept on standby for it, or memory made now.
    pub(crate) fn attach(
        &self,
        device: u32,
        name: &str,
        caller: CallerId,
    ) -> Result<Lease, Status> {
        let core = self.core(device, name).ok_or(Status::NOT_FOUND)?;
        lock(&core).attach(caller)
    }

    /// Gives up a stream `caller` holds.
    pub(crate) fn detach(&self, device: u32, name: &str, caller: CallerId) -> Result<(), Status> {
        let core = self.core(device, name).ok_or(Status::NOT_FOUND)?;
        lock(&core).detach_if(caller)
    }

    /// Gives up everything `caller` held. Runs however the caller went.
    pub(crate) fn caller_gone(&self, caller: CallerId) {
        self.each(|core| {
            let _ = core.detach_if(caller);
        });
    }

    /// What the board file says the stream's buffers are.
    pub(crate) fn memory(&self, device: u32, name: &str) -> Option<StreamMemory> {
        self.core(device, name).map(|core| lock(&core).memory())
    }

    pub(crate) fn stats(&self, device: u32, name: &str) -> Option<StreamStats> {
        self.core(device, name).map(|core| lock(&core).stats())
    }

    /// Tells every consumer the host is going, and makes no more memory.
    pub(crate) fn close_all(&self) {
        self.each(|core| core.close(STATE_CLOSED));
    }
}
