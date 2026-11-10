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

/// The producer's end of a stream of buffers. Whoever opened the stream reads
/// the other end.
pub trait StreamEndpoint {
    type Error: HalError;

    fn buffer_count(&self) -> u32;
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
    Configured,
    Deconfigured,
    Suspended,
    Resumed,
    Setup(ControlSetup),
}

/// This board appearing as a USB device.
pub trait UsbFunction {
    type Error: HalError;

    /// Presents `descriptors` to the host and connects.
    fn enable(&mut self, descriptors: &[u8]) -> Result<(), Self::Error>;
    fn disable(&mut self) -> Result<(), Self::Error>;
    fn next_event(&mut self) -> Result<Option<UsbFunctionEvent>, Self::Error>;
    fn read(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, Self::Error>;
    fn write(&mut self, endpoint: u8, data: &[u8], timeout: Duration)
    -> Result<usize, Self::Error>;
}
