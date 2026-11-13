//! Frame streams: a pool of buffers and a set of shm rings between one producer and one consumer.
//!
//! A board file declares a device's streams (`[devices.streams.<name>]`). A driver takes one with
//! a `StreamHandle` need of the same name; a caller takes the other end with
//! [`Host::stream_attach`](crate::Host::stream_attach), once per stream. After that no frame goes
//! through the host's lock on the way to the consumer beyond what the producer itself holds, and
//! no frame crosses a socket.
//!
//! Every shared object of a stream is the shm family's (`docs/ring.md`): the frame ring, the buffer
//! pool, the hold marks, the doorbell and the job ring. The host makes them for each attachment and
//! hands the consumer their descriptors ([`Lease`]), in the order of
//! [`lemnos_wire::ring::fds`]. The consumer is not trusted: every buffer number and every job read
//! back from the shared memory is checked, and a consumer that gets anything wrong loses the stream
//! and nothing else.
//!
//! A stream's buffers can also be dma-bufs, one per plane of each buffer, for a driver whose hardware
//! takes an image only as a kernel buffer handle. The board file says so (`memory = "dma-buf"`), and
//! the host then asks its [`StreamAllocator`] for them with
//! [`allocate_planes`](StreamAllocator::allocate_planes). The driver borrows the one behind a buffer
//! it holds with `StreamEndpoint::buffer_handle` or `plane_handle`. An allocator that can't make them
//! fails the attachment: a stream declared as dma-bufs is never quietly made of anything else.
//!
//! A stream kept on standby (`standby = true`) has memory in place while no consumer is attached: it
//! is made when the producer next asks for a buffer, the producer fills it as it would a consumer's,
//! and the newest frame published is kept. The next consumer to attach is given that memory, and the
//! kept frame is on its ring the moment it attaches. When it leaves, its memory goes with it and fresh
//! memory is made, so no consumer ever sees another's.

mod endpoint;
mod pool;

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use lemnos_core::{CallerId, Status};
use lemnos_wire::Planes;
use lemnos_wire::ring::Layout;

use endpoint::StreamCore;
pub use endpoint::{BufferWriter, ProducerEndpoint};

use crate::board::DeviceSpec;
pub use crate::board::StreamMemory;

/// Which way the producer touches a buffer's memory, for a dma-buf that needs the kernel told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// Brackets the producer's own CPU access to a dma-buf, so a device whose memory isn't coherent
/// sees what was written and the CPU sees what the device wrote. `begin` before the access and
/// `end` after it. Called once per plane touched, with the plane's pool index. A failure changes
/// nothing about where the bytes go, so there is no result.
pub trait DmaSync: Send + Sync {
    fn begin(&self, index: usize, access: Access);
    fn end(&self, index: usize, access: Access);
}

/// The buffers a stream's pool is made of, from an allocator, for one attachment.
pub struct Buffers {
    /// One descriptor for shared memory, holding every buffer at its stride; one per plane of each
    /// buffer, buffer 0's planes first, for dma-bufs.
    pub fds: Vec<OwnedFd>,
    /// Whether they are dma-bufs: their descriptors are then lent to the producer's hardware.
    pub dma: bool,
    /// Brackets the producer's accesses, for dma-bufs. `None` for memory the CPU alone uses.
    pub sync: Option<Arc<dyn DmaSync>>,
}

/// Makes the memory behind a stream's buffers. Called once per attachment, so a consumer that goes
/// never keeps a view of the next one's frames. The rings, the holds and the doorbell are the
/// host's own and need no allocator.
pub trait StreamAllocator: Send + Sync {
    /// Memory for a stream the board file leaves as shared memory: one sealed memfd of
    /// `layout.pool_bytes()`, every buffer at its stride.
    fn allocate(&self, name: &str, layout: &Layout) -> Result<Buffers, Status>;

    /// Memory for a stream the board file declares as dma-bufs from `heap`: one dma-buf per plane of
    /// each buffer, each at least as long as the plane. The status of a failure goes to whoever asked
    /// for the stream. An allocator that has no dma-bufs to give keeps this, which refuses.
    fn allocate_planes(
        &self,
        name: &str,
        layout: &Layout,
        heap: &str,
        planes: &Planes,
    ) -> Result<Buffers, Status> {
        let _ = (name, layout, heap, planes);
        Err(Status::NOT_SUPPORTED)
    }

    /// As [`allocate_planes`](Self::allocate_planes), but with `read_only` the dma-bufs are
    /// read-only files: the stream has reader slots (`readers` in the board file), and a reader's
    /// descriptor must not be writable. An allocator that can't make read-only dma-bufs keeps this
    /// default and gives writable ones; a ticket on such a stream is then refused, so nothing is
    /// given to a reader that could write.
    fn allocate_planes_for(
        &self,
        name: &str,
        layout: &Layout,
        heap: &str,
        planes: &Planes,
        read_only: bool,
        tail: u32,
    ) -> Result<Buffers, Status> {
        let _ = (read_only, tail);
        self.allocate_planes(name, layout, heap, planes)
    }
}

/// Memory from memfds, for a consumer in the same process or a host that has no dma-heap.
pub struct MemfdAllocator;

impl StreamAllocator for MemfdAllocator {
    fn allocate(&self, name: &str, layout: &Layout) -> Result<Buffers, Status> {
        let _ = name;
        let bytes = usize::try_from(layout.pool_bytes()).map_err(|_| Status::NO_MEMORY)?;
        let fd = shm_ring::sealed_memfd(bytes).map_err(|_| Status::NO_MEMORY)?;
        Ok(Buffers {
            fds: vec![fd],
            dma: false,
            sync: None,
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
    /// Published frames dropped: no consumer, no free buffer, from before this consumer attached,
    /// or replaced before the consumer took them (see the replacement rule in `docs/streams.md`).
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

/// What a consumer gets when it attaches: the descriptors of the stream, in the order of
/// [`lemnos_wire::ring::fds`] followed by the pool's buffers, and what they describe.
pub struct Lease {
    pub layout: Layout,
    pub descriptors: Vec<OwnedFd>,
    /// The buffers as dma-bufs, when they are: see [`ProducerEndpoint::buffer_handle`].
    pub handles: Option<Arc<[OwnedFd]>>,
    /// Each buffer's planes. One plane of the whole buffer unless the board file gives more.
    pub planes: Planes,
    /// Whether the pool is one shared descriptor (`false`: one per plane of each buffer).
    pub shared: bool,
}

/// A reader slot the consumer of a stream asked for: a third process's view of the stream, read-only,
/// with its own marks, bell and liveness pipe. `descriptors` are those of
/// [`lemnos_wire::ring::ticket::fds`], in that order, for the service to pass to the reader.
#[derive(Debug)]
pub struct ReaderTicket {
    /// The slot the reader marks through, which the host numbers.
    pub slot: u32,
    pub layout: Layout,
    /// Each plane's length, as the stream has them.
    pub planes: Planes,
    /// Slack past each buffer, for an importer.
    pub tail_bytes: u32,
    pub descriptors: Vec<OwnedFd>,
    /// What the reader was granted: how many buffers it may hold at once, and for how long.
    pub held: u32,
    pub hold_ms: u32,
    /// What the reader may do: the bits of `lemnos_wire::ring::ticket::rights`.
    pub rights: u8,
}

/// Why a reader slot was refused, with what the refusal names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketRefused {
    pub status: Status,
    /// Names the stream, and the limit or the amount that refused it.
    pub text: String,
}

/// What the host knows of one reader slot: whether it is live, and why and when it was cut off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TicketState {
    /// Whether the slot is in use by a reader.
    pub live: bool,
    /// The cut-off reason (a `shm_ring_buffers::page::cut` value), or 0 while the reader has not been cut off.
    pub reason: u64,
    /// When it was cut off, `CLOCK_MONOTONIC` nanoseconds; 0 when it has not been.
    pub cut_at_ns: u64,
    /// How many readers of this stream have been cut off since the consumer attached.
    pub cuts: u64,
}

impl Lease {
    /// The consumer's end, attached in this process from the descriptors. Used by tests and by a
    /// consumer that shares the host's process.
    pub fn consumer(self) -> Result<lemnos_wire::ring::Consumer, lemnos_wire::ring::RingError> {
        self.attach(false)
    }

    /// As [`consumer`](Self::consumer), with the buffers mapped writable too, for a stream the
    /// consumer fills.
    pub fn consumer_writable(
        self,
    ) -> Result<lemnos_wire::ring::Consumer, lemnos_wire::ring::RingError> {
        self.attach(true)
    }

    fn attach(
        self,
        writable: bool,
    ) -> Result<lemnos_wire::ring::Consumer, lemnos_wire::ring::RingError> {
        let lengths = if self.planes.count() == 1 {
            Vec::new()
        } else {
            self.planes.lengths().to_vec()
        };
        lemnos_wire::ring::Consumer::attach(
            self.descriptors,
            self.layout,
            &lengths,
            self.shared,
            writable,
        )
    }
}

/// Every declared stream: those of the board file's devices, and of each device an `[[each]]`
/// entry makes, added as it is made. None is ever taken away.
pub(crate) struct Streams {
    allocator: Arc<dyn StreamAllocator>,
    table: RwLock<Table>,
    /// The daemon's signed mode: on unless it runs with open packages. Readers may import dma-bufs
    /// with it off, or with it on where the stream says so. Secure until told otherwise.
    signed: std::sync::atomic::AtomicBool,
}

/// Each stream, by its device's index and its name.
type Table = BTreeMap<(u32, String), Arc<Mutex<StreamCore>>>;

impl Streams {
    pub(crate) fn new(allocator: Arc<dyn StreamAllocator>, devices: &[DeviceSpec]) -> Self {
        let streams = Self {
            allocator,
            table: RwLock::new(BTreeMap::new()),
            signed: std::sync::atomic::AtomicBool::new(true),
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
            f(&mut endpoint::lock(core));
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
        endpoint::lock(&core).attach(caller)
    }

    /// Gives up a stream `caller` holds.
    pub(crate) fn detach(&self, device: u32, name: &str, caller: CallerId) -> Result<(), Status> {
        let core = self.core(device, name).ok_or(Status::NOT_FOUND)?;
        endpoint::lock(&core).detach_if(caller)
    }

    /// Gives `caller`, the stream's consumer, a reader slot on it. See `StreamCore::ticket`.
    pub(crate) fn ticket(
        &self,
        device: u32,
        name: &str,
        caller: CallerId,
        held: u32,
        hold_ms: u32,
        import: bool,
    ) -> Result<ReaderTicket, TicketRefused> {
        let signed = self.signed.load(std::sync::atomic::Ordering::Relaxed);
        let Some(core) = self.core(device, name) else {
            return Err(TicketRefused {
                status: Status::NOT_FOUND,
                text: format!("device {device} has no stream {name:?}"),
            });
        };
        endpoint::lock(&core).ticket(caller, held, hold_ms, import, signed)
    }

    /// Why a reader slot of `caller`'s stream was cut off, and whether it is live.
    pub(crate) fn ticket_state(
        &self,
        device: u32,
        name: &str,
        caller: CallerId,
        slot: u32,
    ) -> Result<TicketState, Status> {
        let core = self.core(device, name).ok_or(Status::NOT_FOUND)?;
        endpoint::lock(&core).ticket_state(caller, slot)
    }

    /// Gives back a reader slot `caller` was granted on the stream.
    pub(crate) fn unticket(
        &self,
        device: u32,
        name: &str,
        caller: CallerId,
        slot: u32,
    ) -> Result<(), Status> {
        let core = self.core(device, name).ok_or(Status::NOT_FOUND)?;
        endpoint::lock(&core).unticket(caller, slot)
    }

    /// The pass over every stream's readers, on the daemon's tick. See `StreamCore::tick`.
    pub(crate) fn tick(&self) {
        self.each(StreamCore::tick);
    }

    /// Sets the daemon's signed mode. See the `signed` field.
    pub(crate) fn set_signed(&self, signed: bool) {
        self.signed
            .store(signed, std::sync::atomic::Ordering::Relaxed);
    }

    /// Gives up everything `caller` held. Runs however the caller went.
    pub(crate) fn caller_gone(&self, caller: CallerId) {
        self.each(|core| {
            let _ = core.detach_if(caller);
        });
    }

    /// What the board file says the stream's buffers are.
    pub(crate) fn memory(&self, device: u32, name: &str) -> Option<StreamMemory> {
        self.core(device, name)
            .map(|core| endpoint::lock(&core).memory())
    }

    pub(crate) fn stats(&self, device: u32, name: &str) -> Option<StreamStats> {
        self.core(device, name)
            .map(|core| endpoint::lock(&core).stats())
    }

    /// Tells every consumer the host is going, and makes no more memory.
    pub(crate) fn close_all(&self) {
        self.each(|core| core.close(lemnos_wire::ring::STATE_CLOSED));
    }
}
