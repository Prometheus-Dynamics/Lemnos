//! What a driver asks of a hardware block, a USB bus or a frame stream.
//!
//! Each trait has a Linux implementation, a microcontroller implementation
//! where the hardware exists, and a simulated one. Nothing here allocates, and
//! every result that has a length is written into a buffer the caller owns.

use core::time::Duration;

use crate::HalError;

/// A memory-mapped register window.
pub trait Registers {
    type Error: HalError;

    /// The window's size in bytes.
    fn size(&self) -> usize;
    fn read32(&mut self, offset: usize) -> Result<u32, Self::Error>;
    fn write32(&mut self, offset: usize, value: u32) -> Result<(), Self::Error>;

    /// Clears `clear`, then sets `set`, in one read-modify-write.
    fn modify32(&mut self, offset: usize, clear: u32, set: u32) -> Result<(), Self::Error> {
        let value = self.read32(offset)?;
        self.write32(offset, (value & !clear) | set)
    }

    /// Writes consecutive 32-bit registers starting at `offset`.
    fn write_block(&mut self, offset: usize, values: &[u32]) -> Result<(), Self::Error> {
        for (i, value) in values.iter().enumerate() {
            self.write32(offset + i * 4, *value)?;
        }
        Ok(())
    }
}

/// One or more interrupts delivered since the last event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterruptEvent {
    /// Interrupts delivered, so a slow reader can see what it missed.
    pub count: u32,
    pub timestamp_ns: u64,
}

/// An interrupt line.
pub trait Interrupt {
    type Error: HalError;

    fn enable(&mut self) -> Result<(), Self::Error>;
    fn disable(&mut self) -> Result<(), Self::Error>;
    /// Waits for an interrupt; `None` means the timeout passed first. A
    /// timeout of `None` waits without limit, and zero only checks.
    fn wait(&mut self, timeout: Option<Duration>) -> Result<Option<InterruptEvent>, Self::Error>;
}

/// Memory a block can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaBuffer {
    pub id: u32,
    pub len: u64,
    /// The address as the block sees it.
    pub device_address: u64,
}

/// DMA memory. Every address a block is given comes from here.
pub trait DmaBuffers {
    type Error: HalError;

    fn allocate(&mut self, len: u64) -> Result<DmaBuffer, Self::Error>;
    fn release(&mut self, id: u32) -> Result<(), Self::Error>;
    fn write(&mut self, id: u32, offset: u64, data: &[u8]) -> Result<(), Self::Error>;
    fn read(&mut self, id: u32, offset: u64, out: &mut [u8]) -> Result<(), Self::Error>;
    /// Hands the buffer to the block.
    fn sync_for_device(&mut self, id: u32) -> Result<(), Self::Error>;
    /// Takes the buffer back from the block.
    fn sync_for_cpu(&mut self, id: u32) -> Result<(), Self::Error>;
}

/// The clocks a block needs, by the index a board or chip table gives them. An index past the
/// last clock is `NotFound`, so a driver can take every clock the board names without knowing how
/// many it names.
pub trait Clocks {
    type Error: HalError;

    fn enable(&mut self, clock: u32) -> Result<(), Self::Error>;
    fn disable(&mut self, clock: u32) -> Result<(), Self::Error>;
    fn rate_hz(&mut self, clock: u32) -> Result<u64, Self::Error>;
    /// Asks for a rate and returns the one the clock settled on.
    fn set_rate_hz(&mut self, clock: u32, hz: u64) -> Result<u64, Self::Error>;
}

/// The reset lines of a block, by index the same way: an index past the last line is `NotFound`.
pub trait Reset {
    type Error: HalError;

    fn assert(&mut self, line: u32) -> Result<(), Self::Error>;
    fn deassert(&mut self, line: u32) -> Result<(), Self::Error>;
}

/// Most clocks or reset lines [`enable_clocks`] and [`pulse_resets`] go through.
pub const MAX_LINES: u32 = 32;

/// Turns on every clock `clocks` has, index 0 first, stopping at the first index that is
/// `NotFound` (or at [`MAX_LINES`]). Returns how many it turned on. Any other failure ends it.
pub fn enable_clocks<C: Clocks + ?Sized>(clocks: &mut C) -> Result<u32, C::Error> {
    for clock in 0..MAX_LINES {
        match clocks.enable(clock) {
            Ok(()) => {}
            Err(e) if e.kind() == crate::ErrorKind::NotFound => return Ok(clock),
            Err(e) => return Err(e),
        }
    }
    Ok(MAX_LINES)
}

/// Asserts every reset line `reset` has, index 0 first, then releases them in the same order:
/// the block starts from its reset state. Lines are counted as [`enable_clocks`] counts clocks.
/// Returns how many lines there are.
pub fn pulse_resets<R: Reset + ?Sized>(reset: &mut R) -> Result<u32, R::Error> {
    let mut lines = MAX_LINES;
    for line in 0..MAX_LINES {
        match reset.assert(line) {
            Ok(()) => {}
            Err(e) if e.kind() == crate::ErrorKind::NotFound => {
                lines = line;
                break;
            }
            Err(e) => return Err(e),
        }
    }
    for line in 0..lines {
        reset.deassert(line)?;
    }
    Ok(lines)
}

/// A serial port's parity bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Parity {
    /// No parity bit.
    #[default]
    None,
    /// The parity bit makes the count of ones even.
    Even,
    /// The parity bit makes the count of ones odd.
    Odd,
}

/// A serial port's stop bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StopBits {
    /// One stop bit.
    #[default]
    One,
    /// Two stop bits.
    Two,
}

/// How a serial port frames its bytes: always 8 data bits, at `baud`, with a parity bit or none,
/// and one or two stop bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartFormat {
    pub baud: u32,
    pub parity: Parity,
    pub stop_bits: StopBits,
}

impl UartFormat {
    /// 8N1 at `baud`, the frame a port opens with.
    pub const fn new(baud: u32) -> Self {
        Self {
            baud,
            parity: Parity::None,
            stop_bits: StopBits::One,
        }
    }
}

/// A serial port. It opens as 8N1 at a rate the backend chooses, until the driver configures it.
pub trait Uart {
    type Error: HalError;

    /// Sets the rate and the frame. A rate or frame the port can't do is `InvalidInput`.
    fn configure(&mut self, format: UartFormat) -> Result<(), Self::Error>;
    fn write(&mut self, data: &[u8]) -> Result<usize, Self::Error>;
    /// Returns what arrived, which is 0 bytes when the timeout passed first.
    fn read(&mut self, out: &mut [u8], timeout: Duration) -> Result<usize, Self::Error>;
}

/// One filled buffer, ready for the consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamFrame {
    pub buffer: u32,
    pub bytes: u32,
    pub sequence: u64,
    /// Means something only to the producer and the consumer.
    pub identity: u64,
    /// When the frame was made, in nanoseconds on the host's monotonic clock
    /// (`CLOCK_MONOTONIC` on Linux); 0 when the producer has no time for it.
    pub timestamp_ns: u64,
}

#[cfg(all(feature = "std", unix))]
extern crate std;

/// What names a buffer on a host that has file descriptors.
#[cfg(all(feature = "std", unix))]
type HandleInner<'a> = std::os::fd::BorrowedFd<'a>;

/// Nothing: there is no value of this type, so no handle can be made here.
#[cfg(not(all(feature = "std", unix)))]
type HandleInner<'a> = (core::convert::Infallible, core::marker::PhantomData<&'a ()>);

/// One stream buffer's memory, borrowed, in the form hardware takes it. On a host with file
/// descriptors it is the buffer's dma-buf descriptor: one dma-buf per buffer, holding the whole
/// buffer from its first byte. Nowhere else can one be made, so a stream there answers
/// [`StreamEndpoint::buffer_handle`] without one.
///
/// The type is the same with every feature. `fd` and the conversion from a descriptor come
/// with the `std` feature on Unix.
#[derive(Debug, Clone, Copy)]
pub struct BufferHandle<'a> {
    #[cfg_attr(
        not(all(feature = "std", unix)),
        expect(dead_code, reason = "no handle exists here, so nothing reads one")
    )]
    inner: HandleInner<'a>,
}

#[cfg(all(feature = "std", unix))]
impl<'a> BufferHandle<'a> {
    /// The buffer's dma-buf. It is as large as the stream's buffers or larger (a heap rounds
    /// up), so check what hardware is told to write against its real size, not the stream's.
    #[must_use]
    pub fn fd(&self) -> std::os::fd::BorrowedFd<'a> {
        self.inner
    }
}

#[cfg(all(feature = "std", unix))]
impl<'a> From<std::os::fd::BorrowedFd<'a>> for BufferHandle<'a> {
    fn from(fd: std::os::fd::BorrowedFd<'a>) -> Self {
        Self { inner: fd }
    }
}

#[cfg(all(feature = "std", unix))]
impl std::os::fd::AsFd for BufferHandle<'_> {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.inner
    }
}

/// What a stream's buffers are made of. It is fixed when the stream is declared, so a driver
/// can ask before any consumer has attached and refuse a stream it can't work on when it binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferKind {
    /// Plain memory shared with the consumer: `write` and `read` are the only way into it, and
    /// [`StreamEndpoint::buffer_handle`] answers `Unsupported`.
    Plain,
    /// Every buffer is behind a handle hardware can take (a dma-buf, on a host with file
    /// descriptors): [`StreamEndpoint::buffer_handle`] lends it while a consumer is attached.
    /// `write` and `read` reach the same memory.
    Handles,
}

/// The producer's end of a stream of buffers. Whoever opened the stream reads
/// the other end.
pub trait StreamEndpoint {
    /// Every stream's errors have the kinds' meaning, so a default here can refuse.
    type Error: HalError + From<crate::ErrorKind>;

    fn buffer_count(&self) -> u32;
    /// Whether `buffer` was taken on the memory in place now: the attached consumer's, or, on a
    /// stream kept on standby, the memory waiting for the next consumer. `false` when there is
    /// no memory in place, and when the memory the buffer was taken on has gone (its consumer
    /// left), whether or not other memory has come: publishing the buffer then reaches no one,
    /// and that is how to give it up. One taken afterwards is current again.
    ///
    /// On a stream kept on standby (`standby = true` in the board file) memory is in place while
    /// no consumer is attached, so a producer fills buffers then as at any other time. A frame
    /// published with nobody attached is kept, the newest only, and goes to the next consumer
    /// the moment it attaches, on that same memory: nothing is copied. The memory a consumer
    /// leaves goes with it, and fresh memory takes its place.
    ///
    /// This is how a producer that holds buffers for a consumer (a job's buffers, named by
    /// number) sees that the consumer it held them for is no longer the one there, on any kind
    /// of stream: `write` and `read` reach whatever consumer is attached, and only a stream
    /// with handles says so through [`buffer_handle`](Self::buffer_handle). Refused, as
    /// `write` is, for a buffer the producer doesn't hold. Allocates nothing.
    fn buffer_current(&self, buffer: u32) -> Result<bool, Self::Error>;
    /// What the stream's buffers are. The answer never changes for a stream, with or without a
    /// consumer, so a driver whose hardware takes buffers only by handle asks once when it
    /// binds and refuses [`BufferKind::Plain`] there.
    fn buffer_kind(&self) -> BufferKind;
    /// Passes a filled buffer to the consumer.
    fn publish(&mut self, frame: StreamFrame) -> Result<(), Self::Error>;
    /// The next buffer the consumer has returned, if any.
    fn next_buffer(&mut self) -> Result<Option<u32>, Self::Error>;
    /// Copies the consumer's next job into `out` and returns its length. What
    /// the bytes mean is between the driver and the consumer.
    fn next_job(&mut self, out: &mut [u8]) -> Result<Option<usize>, Self::Error>;
    /// Copies `data` into `buffer` at `offset`, for a producer whose hardware doesn't fill the
    /// buffer itself. `false` when no consumer is attached, so there was nowhere for it to land.
    /// Refused for a buffer the producer doesn't hold (taken with `next_buffer` and not yet
    /// published) and for a range outside the buffer. Allocates nothing.
    fn write(&mut self, buffer: u32, offset: usize, data: &[u8]) -> Result<bool, Self::Error>;
    /// Copies from `buffer` at `offset` into `out`, for a producer whose consumer hands it input
    /// in a buffer (a job's configuration, an image to work on). `false` when no consumer is
    /// attached. Refused for a buffer the producer doesn't hold and for a range outside the
    /// buffer. The consumer shares this memory and can change it at any time, so read once and
    /// check the copy, never the buffer. Allocates nothing.
    fn read(&mut self, buffer: u32, offset: usize, out: &mut [u8]) -> Result<bool, Self::Error>;
    /// Borrows `buffer`'s memory in the form hardware takes it, for a producer whose hardware
    /// reads or fills the buffer itself. On a stream whose buffers are several planes, this is
    /// plane 0's: see [`plane_handle`](Self::plane_handle). Allocates nothing.
    ///
    /// - `Ok(Some(handle))`: the buffer is behind `handle`, on the memory in place now: a
    ///   consumer's, or the memory a stream kept on standby holds for the next one.
    /// - `Ok(None)`: nothing is behind the buffer now. No memory is in place, or the memory the
    ///   buffer was taken on has gone with its consumer. Publishing the buffer reaches no one,
    ///   and that is how to give it up; one taken afterwards has a handle again.
    /// - An error of kind [`Unsupported`](crate::ErrorKind::Unsupported): this stream's buffers
    ///   have no handles (plain shared memory, a microcontroller). `write` and `read` are the
    ///   only way into them.
    /// - Refused, as `write` is, for a buffer the producer doesn't hold.
    ///
    /// `write` and `read` reach the same memory as the handle does, so a producer that fills a
    /// buffer in software needn't ask which kind of stream it has.
    ///
    /// How long things last:
    ///
    /// - The borrow ends at the endpoint's next `&mut` call. Hardware that works past that needs
    ///   its own hold on the memory: the kernel takes one when it is given the descriptor in a
    ///   request, and a duplicate of the descriptor is one. Either keeps the memory alive for as
    ///   long as it is held, whatever the stream does meanwhile, so hardware can never be left
    ///   writing memory that was freed or given to someone else. The worst a late write does is
    ///   land where nobody reads it.
    /// - Hardware must have finished with a buffer before it is published. After `publish` the
    ///   consumer reads the buffer, and may hand it back to be taken again.
    /// - A handle is good for one taking of the buffer. A stream's memory is made anew for each
    ///   consumer, so buffer 3 of this consumer and buffer 3 of the next are different memory:
    ///   ask again for every buffer `next_buffer` gives, and keep no handle by buffer number.
    ///   Memory handed to hardware under a consumer that has since gone stays that consumer's
    ///   alone. The next consumer never sees it, and it never sees the next consumer's.
    /// - The consumer shares the memory and can change it at any time, as with `read`: hardware
    ///   given a consumer's image must take nothing in it on trust.
    fn buffer_handle(&self, buffer: u32) -> Result<Option<BufferHandle<'_>>, Self::Error>;
    /// How many memory planes each buffer is: 1 unless the board file gives the stream
    /// `planes`, for hardware that writes one frame to several places of its own (a V4L2
    /// format with several memory planes). Never changes for a stream.
    fn planes(&self) -> u32 {
        1
    }
    /// Where plane `plane` starts in a buffer as `write` and `read` address it: the planes
    /// before it, end to end. `None` for a plane the buffers don't have. Allocates nothing.
    fn plane_offset(&self, plane: u32) -> Option<usize> {
        (plane == 0).then_some(0)
    }
    /// Borrows plane `plane` of `buffer` in the form hardware takes it, with everything
    /// [`buffer_handle`](Self::buffer_handle) says: each plane is its own dma-buf, holding
    /// that plane from its first byte, at least as long as the board file gives it (a heap
    /// rounds up). Plane 0 is what `buffer_handle` lends. Refused for a plane the buffers
    /// don't have. Allocates nothing.
    fn plane_handle(
        &self,
        buffer: u32,
        plane: u32,
    ) -> Result<Option<BufferHandle<'_>>, Self::Error> {
        if plane == 0 {
            self.buffer_handle(buffer)
        } else {
            Err(crate::ErrorKind::InvalidInput.into())
        }
    }
    /// Ends the attached consumer's stream as faulted, for a consumer that broke the rules of
    /// what it posts: bytes on the job queue that are no job of the driver's contract. It is the
    /// same end a consumer that corrupts the ring gets. The consumer sees its stream faulted
    /// and has to attach again to carry on; the jobs it had posted are gone with it and
    /// everything it held comes back free. Nothing else is touched: the producer keeps its end
    /// and the next consumer starts clean.
    ///
    /// Buffers the producer holds were taken under the consumer that is now gone, so nothing is
    /// behind them: publish each to give it up, as for a consumer that left by itself. Does
    /// nothing when no consumer is attached. Allocates nothing.
    fn fault_consumer(&mut self) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbSpeed {
    Unknown,
    Low,
    Full,
    High,
    Super,
    SuperPlus,
}

/// Where a device is plugged in right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbDeviceId {
    pub bus: u8,
    pub address: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbDeviceInfo {
    pub id: UsbDeviceId,
    pub vendor: u16,
    pub product: u16,
    pub class: u8,
    pub speed: UsbSpeed,
}

/// The fields of a control transfer other than its data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlSetup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
}

impl ControlSetup {
    /// Bit 7 of `request_type`: the data stage goes from the device to the host.
    pub const DIRECTION_IN: u8 = 0x80;

    /// Whether the data stage, if there is one, goes from the device to the host.
    pub const fn is_in(&self) -> bool {
        self.request_type & Self::DIRECTION_IN != 0
    }
}

/// One packet of an isochronous transfer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IsoPacket {
    /// Bytes this packet may carry, set by the caller.
    pub capacity: u32,
    /// Bytes it did carry, set by the transfer.
    pub received: u32,
    /// Whether the packet arrived intact.
    pub ok: bool,
}

/// This board's USB host: finds what's plugged in and opens it.
pub trait UsbHost {
    type Error: HalError;
    type Device: UsbDevice<Error = Self::Error>;

    /// Fills `out` and returns how many devices there are, which may be more
    /// than fit.
    fn enumerate(&mut self, out: &mut [UsbDeviceInfo]) -> Result<usize, Self::Error>;
    fn open(&mut self, id: UsbDeviceId) -> Result<Self::Device, Self::Error>;
}

/// An opened USB device.
pub trait UsbDevice {
    type Error: HalError;

    fn config_descriptor(&mut self, index: u8, out: &mut [u8]) -> Result<usize, Self::Error>;
    /// Resets the device at its port. What the caller sees is one of three things:
    ///
    /// - `Ok`: the device took the reset and is still the device this handle opened, with the
    ///   descriptors it had. The handle is good and its interface claims stand. The device is in
    ///   its default state, so set any alternate setting again; a host may or may not put them
    ///   back.
    /// - An error of kind [`Unavailable`](crate::ErrorKind::Unavailable): this handle's device is
    ///   no longer there. The reset made it leave the bus or come back as a different device
    ///   (what a firmware load ends in), or it had already gone; a host can't always tell these
    ///   apart, so neither can the caller. The handle is dead for good: every later call on it,
    ///   `reset` included, fails with `Unavailable`. Drop it. Whatever the device is now is
    ///   opened afresh.
    /// - Any other error: the reset was not carried out. The handle is as it was and the call
    ///   can be made again.
    ///
    /// Allocates nothing.
    fn reset(&mut self) -> Result<(), Self::Error>;
    /// Takes `interface` away from a driver of the host's own that is bound to it (a kernel
    /// driver), so that [`claim_interface`](Self::claim_interface) no longer finds it `Busy`.
    /// This does not claim the interface.
    ///
    /// `Ok` when no such driver is bound afterwards, which includes when none was bound before
    /// and a host that has no drivers of its own. A claim held through a handle, this one or
    /// another, is not a kernel driver and is left alone.
    ///
    /// Nothing puts the driver back: not [`release_interface`](Self::release_interface), not
    /// dropping the handle. The interface stays unbound until the host binds it again, which a
    /// reset or a replug can do, so detach again before claiming after either.
    ///
    /// Allocates nothing.
    fn detach_kernel_driver(&mut self, interface: u8) -> Result<(), Self::Error>;
    /// Claims `interface` for this handle. `Busy` when something else holds it, a driver of the
    /// host's own included: see [`detach_kernel_driver`](Self::detach_kernel_driver).
    fn claim_interface(&mut self, interface: u8) -> Result<(), Self::Error>;
    fn release_interface(&mut self, interface: u8) -> Result<(), Self::Error>;
    fn set_alternate(&mut self, interface: u8, alternate: u8) -> Result<(), Self::Error>;
    fn control_in(
        &mut self,
        setup: ControlSetup,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, Self::Error>;
    fn control_out(
        &mut self,
        setup: ControlSetup,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, Self::Error>;
    fn bulk_in(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, Self::Error>;
    fn bulk_out(
        &mut self,
        endpoint: u8,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, Self::Error>;
    fn interrupt_in(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, Self::Error>;
    /// Starts an isochronous stream from the IN `endpoint`: `transfers` transfers of `packets`
    /// packets each, every packet room for `packet_bytes`, are queued with the host controller
    /// now, and each one [`iso_in_next`](Self::iso_in_next) hands over is queued again at once.
    /// So the controller always has transfers queued, and no service interval goes by between
    /// two of them with nothing to take the device's packet, however long the caller takes
    /// between calls, up to `transfers - 1` transfers' time.
    ///
    /// `InvalidInput` for `transfers` of 0 or over [`MAX_ISO_TRANSFERS`], `packets` of 0 or
    /// over [`MAX_ISO_PACKETS`], or `packet_bytes` of 0 or over [`MAX_ISO_PACKET_BYTES`]. `Busy`
    /// while a stream is started: stop it first. Nothing is queued after an error. The memory
    /// the stream needs is taken here; nothing after this allocates.
    fn iso_in_start(
        &mut self,
        endpoint: u8,
        transfers: u8,
        packets: u16,
        packet_bytes: u32,
    ) -> Result<(), Self::Error>;
    /// Takes the oldest transfer of the stream once it is done, waiting at most `timeout`, and
    /// queues it again. Packet `i` lands at `i * packet_bytes` in `data` and is described by
    /// `packets[i]`: its `capacity` is `packet_bytes`, its `received` how many bytes came, and
    /// `ok` whether they came intact. A packet that came short leaves the rest of its room as
    /// it was.
    ///
    /// `packets` holds exactly the stream's `packets`, and `data` at least `packets *
    /// packet_bytes` bytes, or this is `InvalidInput` and the stream is left as it is; so is a
    /// call with no stream started. `Timeout` when nothing was done in time: the stream goes
    /// on, and nothing is lost but what the device sent while every transfer was done and not
    /// yet taken. `Unavailable` when the device has gone, for good as for any call. Any other
    /// error says the transfer failed as a whole; it is queued again all the same, and the
    /// next call goes on with the one after it. Allocates nothing.
    fn iso_in_next(
        &mut self,
        data: &mut [u8],
        packets: &mut [IsoPacket],
        timeout: Duration,
    ) -> Result<(), Self::Error>;
    /// Takes every transfer of the stream back from the controller, and waits until it has
    /// let go of them. `Ok`, doing nothing, with no stream started. Dropping the device does
    /// this too. Allocates nothing.
    fn iso_in_stop(&mut self) -> Result<(), Self::Error>;
}

/// The most transfers an isochronous stream keeps queued ([`UsbDevice::iso_in_start`]).
pub const MAX_ISO_TRANSFERS: u8 = 16;
/// The most packets in one isochronous transfer.
pub const MAX_ISO_PACKETS: u16 = 64;
/// The most bytes one isochronous packet carries: three transactions of 1024 bytes in one
/// high-speed microframe.
pub const MAX_ISO_PACKET_BYTES: u32 = 3 * 1024;

/// The bytes an isochronous endpoint moves in one service interval, from its `wMaxPacketSize`:
/// the packet size in bits 0 to 10 times the transactions per microframe, one more than bits
/// 11 and 12 say (at high speed; always one at full speed, where those bits are 0).
#[must_use]
pub const fn iso_interval_bytes(max_packet: u16) -> u32 {
    let size = (max_packet & 0x7ff) as u32;
    let transactions = ((max_packet >> 11) & 3) as u32 + 1;
    size * transactions
}

/// Whether an isochronous stream's shape is one [`UsbDevice::iso_in_start`] takes.
#[must_use]
pub const fn iso_shape_ok(transfers: u8, packets: u16, packet_bytes: u32) -> bool {
    transfers != 0
        && transfers <= MAX_ISO_TRANSFERS
        && packets != 0
        && packets <= MAX_ISO_PACKETS
        && packet_bytes != 0
        && packet_bytes <= MAX_ISO_PACKET_BYTES
}

/// What happened on the wire to this board acting as a USB device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbFunctionEvent {
    /// The host chose the configuration. Endpoints carry data from now on, none of them halted.
    Configured,
    /// The configuration is no longer in force: the host chose configuration 0, reset the bus,
    /// or the cable was pulled. A backend can't always tell these apart, so neither can the
    /// function. Endpoints carry nothing until the next [`Configured`](Self::Configured), and a
    /// control request that was pending is gone.
    Deconfigured,
    /// The bus went idle. Nothing moves until [`Resumed`](Self::Resumed); the configuration
    /// stands.
    Suspended,
    Resumed,
    /// A control request for the function to answer. It is pending until it is answered with
    /// [`UsbFunction::control_in`] or [`UsbFunction::control_out`], refused with
    /// [`UsbFunction::control_stall`], or abandoned by the host.
    Setup {
        setup: ControlSetup,
        /// `wLength`: how many bytes the data stage may carry, and 0 when there is none. It
        /// bounds what `control_in` sends and is what `control_out` takes.
        length: u16,
    },
}

/// This board appearing as a USB device.
///
/// # States
///
/// A function is disabled until [`enable`](Self::enable) succeeds and after
/// [`disable`](Self::disable). While it is disabled every call but `enable` and `disable` fails
/// with [`Unavailable`](crate::ErrorKind::Unavailable).
///
/// While it is enabled the host decides the rest. Endpoints carry data only between a
/// [`Configured`](UsbFunctionEvent::Configured) and the next
/// [`Deconfigured`](UsbFunctionEvent::Deconfigured); outside that, [`read`](Self::read),
/// [`write`](Self::write) and [`set_halt`](Self::set_halt) fail with `Unavailable`. While the
/// bus is suspended nothing moves, so a transfer runs into its timeout.
///
/// # A host reset or a cable pull
///
/// Neither disables the function, and neither needs `enable` again:
///
/// - [`next_event`](Self::next_event) reports `Deconfigured` if the function was configured.
/// - A `read` or `write` under way ends with `Unavailable`, and so does every one after it
///   until the next `Configured`. Bytes of the transfer that was cut are lost.
/// - A pending control request is gone: answering or stalling it fails with `Unavailable`.
/// - Every endpoint halt is cleared.
/// - When the host comes back and configures the function, `Configured` is reported and the
///   endpoints work again, with the descriptors `enable` was given.
///
/// When the controller itself lets go of the function (it was taken away, or switched to
/// another role), `next_event` fails with `Unavailable` and keeps failing: the function is
/// enabled in name only. Call `disable`, then `enable` once the controller is back; `enable`
/// says whether it is. A handle is never dead for good: `disable` then `enable` always starts
/// over.
///
/// # What the backend answers
///
/// The backend answers the requests any USB device answers from what `enable` was given
/// (`GET_DESCRIPTOR`, `SET_ADDRESS`, `SET_CONFIGURATION`, `GET_STATUS` and the like), and the
/// host's `CLEAR_FEATURE(ENDPOINT_HALT)`. The function sees the rest as
/// [`Setup`](UsbFunctionEvent::Setup): class and vendor requests, and standard requests to an
/// interface or endpoint that the backend does not answer.
///
/// A backend whose controller assigns them may show the host other endpoint addresses,
/// interface numbers and string indexes than the descriptors carry. The function always uses
/// the ones it gave: in `read`, `write` and `set_halt`, and in the `index` of a request sent to
/// one of its interfaces or endpoints.
///
/// # Isochronous endpoints and alternate settings
///
/// An isochronous endpoint moves one service interval's bytes at a time, with no handshake
/// and no retry. On one:
///
/// - Each [`write`](Self::write) is what goes out in one service interval: at most the
///   endpoint's bytes per interval ([`iso_interval_bytes`] of its descriptor at the speed in
///   force; the larger of the two speeds' when the backend can't tell which), else
///   `InvalidInput` and nothing is taken. The backend keeps several writes queued with the
///   controller, so one is there at each interval while the function prepares the next: a
///   write returns as soon as its bytes are queued, and `Timeout` only when every place in
///   the queue is still taken. Bytes the host did not ask for in time are dropped, and a
///   write that failed is not reported: the next interval carries the next write.
/// - Each [`read`](Self::read) is what came in one service interval, oldest first. `data`
///   must hold the endpoint's bytes per interval, else `InvalidInput`. The backend keeps
///   several reads queued, so what the host sends between two calls is kept while there is
///   room.
/// - [`set_halt`](Self::set_halt) is `Unsupported`.
///
/// Interfaces may have alternate settings, and the host picks one with `SET_INTERFACE`. An
/// isochronous endpoint goes in a setting other than 0, so the default setting reserves no
/// bandwidth. A backend either answers `SET_INTERFACE` and `GET_INTERFACE` itself, and may then
/// report [`Configured`](UsbFunctionEvent::Configured) again when the setting changes, or hands
/// them to the function as [`Setup`](UsbFunctionEvent::Setup) events like any other request.
/// Either way a function that needs to know when to start streaming can learn it from its own
/// class requests. A backend may limit how many settings an
/// interface has, or take an endpoint in only one setting of the configuration, and refuses a
/// set beyond that in `enable` with `Unsupported`.
///
/// Nothing here allocates once `enable` has returned.
pub trait UsbFunction {
    type Error: HalError;

    /// Presents `descriptors` to the host and connects. The bytes are a descriptor set in the
    /// layout [`UsbDescriptors`](crate::usb_descriptors::UsbDescriptors) defines and checks;
    /// a set that fails the check is `InvalidInput` and nothing is presented. A valid set the
    /// controller can't carry (an endpoint it doesn't have, a descriptor its framework
    /// refuses) is `Unsupported` or `InvalidInput`, and nothing is presented either.
    ///
    /// `Busy` when the function is already enabled: `disable` it first. `NotFound` when the
    /// controller, or something the board has to set up for it, isn't there. After any error
    /// the function is disabled and nothing of the attempt is left behind, so `enable` can be
    /// called again.
    fn enable(&mut self, descriptors: &[u8]) -> Result<(), Self::Error>;
    /// Disconnects from the host and forgets the descriptors. `Ok` on a function that is
    /// already disabled. A pending control request and every halt go with it.
    fn disable(&mut self) -> Result<(), Self::Error>;
    /// What happened since the last call, oldest first, or `None`. Never waits.
    ///
    /// A control request still pending when this is called is stalled by it: a function that
    /// reads on has not answered. An error of kind `Unavailable` means the controller let go
    /// of the function, as the trait's docs say.
    fn next_event(&mut self) -> Result<Option<UsbFunctionEvent>, Self::Error>;
    /// Answers the pending control request, which must be one whose data goes to the host
    /// ([`ControlSetup::is_in`]), with `data` as its data stage. At most the request's
    /// `length` bytes are sent, and fewer end the stage short. Returns how many were sent.
    ///
    /// `InvalidInput` when no request is pending or the pending one goes the other way; it
    /// stays pending. `Unavailable` when the host abandoned the request (a new request, a
    /// reset, a pull): nothing is pending afterwards. After `Ok` and after any other error
    /// nothing is pending either.
    ///
    /// Waits at most `timeout` for the host to take the data, then fails with `Timeout`. A
    /// backend that can't abandon a data stage once it has begun waits until the host ends
    /// it, which a host does within its own control timeout or by a reset.
    fn control_in(&mut self, data: &[u8], timeout: Duration) -> Result<usize, Self::Error>;
    /// Takes the data stage of the pending control request, which must be one whose data
    /// comes from the host, into `data`, and so accepts the request. `data` must hold the
    /// request's `length` bytes. A request with a `length` of 0 has no data stage and is
    /// accepted by this call with an empty `data`. Returns how many bytes came.
    ///
    /// Errors, what stays pending and `timeout` are as for [`control_in`](Self::control_in).
    fn control_out(&mut self, data: &mut [u8], timeout: Duration) -> Result<usize, Self::Error>;
    /// Refuses the pending control request with a protocol stall, which the host sees as
    /// "request not supported". Nothing is pending afterwards. `InvalidInput` when no request
    /// is pending, `Unavailable` when the host had already abandoned it.
    fn control_stall(&mut self) -> Result<(), Self::Error>;
    /// Halts `endpoint` (`halted` true: it answers the host with a stall until the halt is
    /// cleared) or clears its halt and resets its data toggle (`halted` false). The host can
    /// clear a halt too, and the function is not told.
    ///
    /// `InvalidInput` for an endpoint the descriptors don't name, `Unsupported` for one that
    /// can't be halted (an isochronous endpoint), `Unavailable` while not configured.
    fn set_halt(&mut self, endpoint: u8, halted: bool) -> Result<(), Self::Error>;
    /// Takes what the host sent to the OUT endpoint `endpoint` (its address in the
    /// descriptors) into `data`, waiting at most `timeout`. Returns how many bytes came, which
    /// is 0 for a zero-length packet.
    ///
    /// `InvalidInput` for an endpoint the descriptors don't name or one of the other
    /// direction, `Timeout` when nothing came, `Unavailable` while not configured.
    ///
    /// A `Timeout` loses nothing: what the host sends later is there for the next `read`,
    /// whole. A `timeout` of zero only looks, so a caller that never waits reads this way. A
    /// transfer ends at a short packet, and `read` gives one transfer at a time: bytes of a
    /// transfer that did not fit `data` come with the next `read`, when `data` is a multiple
    /// of the endpoint's packet size. With any other size the bytes of a packet that did not
    /// fit may be dropped.
    fn read(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, Self::Error>;
    /// Hands `data` to the controller to send to the host on the IN endpoint `endpoint` (its
    /// address in the descriptors, bit 7 set) as one transfer, waiting at most `timeout` for
    /// the endpoint to be free of what was handed to it before. Returns how many bytes the
    /// controller took, which can be fewer than `data` holds: hand it the rest next, and it
    /// goes out as part of the same transfer when what was taken is a multiple of the
    /// endpoint's packet size (a controller takes no other short count). An empty `data`
    /// sends a zero-length packet.
    ///
    /// `Ok` says the bytes are the controller's, not that the host has them: the host takes
    /// them when it next asks, and a reset or a pull before that loses them, as it loses any
    /// transfer under way. `Timeout` when the endpoint stayed busy: nothing of `data` was
    /// taken, nothing already handed over is disturbed, and the call can be made again. A
    /// `timeout` of zero only looks. A transfer that failed after it was handed over is
    /// reported by the next `write` to the endpoint, which takes nothing. The other errors
    /// are as for [`read`](Self::read).
    fn write(&mut self, endpoint: u8, data: &[u8], timeout: Duration)
    -> Result<usize, Self::Error>;
    /// Borrows the function's two bulk endpoints as descriptors a process can read and write
    /// itself: `[OUT, IN]`, the OUT endpoint (the host's writes, read by the device) first. The
    /// descriptors block, as the controller's files do, and stay good while the function is
    /// enabled, through the host going away and coming back. Once the function is disabled
    /// they are dead for good, and a holder must give them up.
    ///
    /// `Unavailable` while not enabled, `Unsupported` for a function that doesn't have one bulk
    /// OUT and one bulk IN endpoint, or whose backend can't lend them. The borrow is one of
    /// [`BufferHandle`], which is the descriptor form every backend lends. Allocates nothing.
    fn endpoint_handles(&self) -> Result<[BufferHandle<'_>; 2], Self::Error>;
}
