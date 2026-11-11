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

/// The clocks a block needs, by the index a board or chip table gives them.
pub trait Clocks {
    type Error: HalError;

    fn enable(&mut self, clock: u32) -> Result<(), Self::Error>;
    fn disable(&mut self, clock: u32) -> Result<(), Self::Error>;
    fn rate_hz(&mut self, clock: u32) -> Result<u64, Self::Error>;
    /// Asks for a rate and returns the one the clock settled on.
    fn set_rate_hz(&mut self, clock: u32, hz: u64) -> Result<u64, Self::Error>;
}

/// The reset lines of a block.
pub trait Reset {
    type Error: HalError;

    fn assert(&mut self, line: u32) -> Result<(), Self::Error>;
    fn deassert(&mut self, line: u32) -> Result<(), Self::Error>;
}

/// A serial port.
pub trait Uart {
    type Error: HalError;

    fn set_baud(&mut self, baud: u32) -> Result<(), Self::Error>;
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
    type Error: HalError;

    fn buffer_count(&self) -> u32;
    /// Whether `buffer` was taken under the consumer attached now. `false` when no consumer is
    /// attached, and when the one that was attached when the buffer was taken with
    /// `next_buffer` has gone, whether or not another has come: publishing the buffer then
    /// reaches no one, and that is how to give it up. One taken afterwards is current again.
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
    /// reads or fills the buffer itself. Allocates nothing.
    ///
    /// - `Ok(Some(handle))`: the buffer is behind `handle`, and a consumer is attached to it.
    /// - `Ok(None)`: nothing is behind the buffer now. No consumer is attached, or the consumer
    ///   that was attached when the buffer was taken with `next_buffer` has gone. Publishing the
    ///   buffer reaches no one, and that is how to give it up; one taken afterwards has a handle
    ///   again.
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
    /// Fills `packets` from one isochronous transfer into `data`, packed
    /// back to back at each packet's capacity.
    fn iso_in(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        packets: &mut [IsoPacket],
        timeout: Duration,
    ) -> Result<(), Self::Error>;
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
}
