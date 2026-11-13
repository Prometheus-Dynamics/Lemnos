//! What a package's driver does with what it took: one small table of functions for each kind
//! of resource, and the values those functions carry.
//!
//! Every table is declared once, by `table!`, which makes the `#[repr(C)]` struct of function
//! pointers, the trait a host implements for it, the guarded `extern "C"` function behind each
//! pointer, and the table's part of the layout hash. A new method is one line here, its body in
//! the host and its use in the package.
//!
//! Every function takes the resource's [`Handle`] first and returns a [`Status`]. What it gives
//! back is written through an [`Out`]. A function the host does not serve answers
//! [`Status::NOT_SUPPORTED`].

#[cfg(test)]
use core::mem::offset_of;

use super::{Out, PackageHealth, Slice, SliceMut};
use crate::{Handle, Status};

// ---- What `take` is asked for ----

/// [`HostTable::take`](super::HostTable::take): another device, by the contract in `tag`.
pub const TAKE_DEVICE: u32 = 0;
pub const TAKE_I2C: u32 = 1;
/// An SPI device, used through [`SpiTable::transaction`].
pub const TAKE_SPI: u32 = 2;
pub const TAKE_LINE: u32 = 3;
pub const TAKE_PWM: u32 = 4;
pub const TAKE_UART: u32 = 5;
pub const TAKE_USB_DEVICE: u32 = 6;
pub const TAKE_USB_FUNCTION: u32 = 7;
pub const TAKE_REGISTERS: u32 = 8;
pub const TAKE_INTERRUPT: u32 = 9;
pub const TAKE_DMA: u32 = 10;
pub const TAKE_CLOCKS: u32 = 11;
pub const TAKE_RESET: u32 = 12;
pub const TAKE_STREAM: u32 = 13;
/// An SPI device with per-transfer settings, used through [`SpiTable::transfer`]. In a manifest
/// its need is the same kind of bus as [`TAKE_SPI`].
pub const TAKE_RAW_SPI: u32 = 14;

// ---- Limits, the same on both sides ----

/// The most steps one I2C or SPI transaction carries, and the most segments one raw SPI
/// transfer does.
pub const MAX_TRANSFER_STEPS: usize = 16;
/// The most packets one isochronous transfer carries.
pub const MAX_ISO_PACKETS: usize = 64;

/// A timeout that never comes: wait without limit.
pub const NO_TIMEOUT: u64 = u64::MAX;
/// [`StreamTable::next_buffer`]'s answer when the consumer has returned none.
pub const NO_BUFFER: u32 = u32::MAX;
/// [`StreamTable::next_job`]'s answer when the consumer has sent none.
pub const NO_JOB: usize = usize::MAX;
/// [`StreamTable::buffer_kind`]: plain memory shared with the consumer.
pub const BUFFERS_PLAIN: u32 = 0;
/// [`StreamTable::buffer_kind`]: every buffer is behind a handle hardware can take.
pub const BUFFERS_HANDLES: u32 = 1;

// ---- Values ----

/// A file descriptor of the peer's, lent: its number, good in this process for as long as the
/// call that gave it says. Made only from an open descriptor.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fd {
    number: i32,
}

impl Fd {
    /// No descriptor.
    pub const NONE: Self = Self { number: -1 };

    pub const fn is_none(self) -> bool {
        self.number < 0
    }

    /// The descriptor's number.
    pub const fn number(self) -> i32 {
        self.number
    }
}

#[cfg(all(feature = "std", unix))]
impl Fd {
    /// Lends `fd` to the peer.
    pub fn lend(fd: std::os::fd::BorrowedFd<'_>) -> Self {
        use std::os::fd::AsRawFd;
        Self {
            number: fd.as_raw_fd(),
        }
    }

    /// The descriptor, for as long as `lender` (what it was asked through) is borrowed. `None`
    /// for [`Fd::NONE`].
    pub fn borrowed<T: ?Sized>(self, _lender: &T) -> Option<std::os::fd::BorrowedFd<'_>> {
        if self.number < 0 {
            return None;
        }
        // SAFETY: an `Fd` is only made by `lend`, from an open descriptor, here or in the peer,
        // and the peer keeps it open until what lent it is used again, which the borrow of
        // `_lender` rules out for as long as the result lives.
        Some(unsafe { std::os::fd::BorrowedFd::borrow_raw(self.number) })
    }
}

/// One step of an I2C or SPI transaction.
#[repr(C)]
#[derive(Debug)]
pub struct PackageTransfer<'a> {
    /// One of the `TRANSFER_*` values. I2C takes only the first two.
    pub kind: u32,
    /// [`TRANSFER_DELAY`]: how long, nanoseconds.
    pub delay_ns: u32,
    pub write: Slice<'a, u8>,
    pub read: SliceMut<'a, u8>,
}

/// Sends `write`.
pub const TRANSFER_WRITE: u32 = 0;
/// Fills `read`.
pub const TRANSFER_READ: u32 = 1;
/// SPI: sends `write` while `read` fills.
pub const TRANSFER_BOTH: u32 = 2;
/// SPI: sends what `read` holds and fills it with what came back.
pub const TRANSFER_IN_PLACE: u32 = 3;
/// SPI: waits `delay_ns`.
pub const TRANSFER_DELAY: u32 = 4;

impl<'a> PackageTransfer<'a> {
    const fn of(kind: u32, delay_ns: u32, write: &'a [u8], read: &'a mut [u8]) -> Self {
        Self {
            kind,
            delay_ns,
            write: Slice::new(write),
            read: SliceMut::new(read),
        }
    }

    pub const fn write(bytes: &'a [u8]) -> Self {
        Self::of(TRANSFER_WRITE, 0, bytes, &mut [])
    }

    pub const fn read(room: &'a mut [u8]) -> Self {
        Self::of(TRANSFER_READ, 0, &[], room)
    }

    pub const fn both(room: &'a mut [u8], bytes: &'a [u8]) -> Self {
        Self::of(TRANSFER_BOTH, 0, bytes, room)
    }

    pub const fn in_place(bytes: &'a mut [u8]) -> Self {
        Self::of(TRANSFER_IN_PLACE, 0, &[], bytes)
    }

    pub const fn delay(delay_ns: u32) -> Self {
        Self::of(TRANSFER_DELAY, delay_ns, &[], &mut [])
    }
}

/// One transfer of a raw SPI transaction: `tx` is clocked out while `rx` fills.
#[repr(C)]
#[derive(Debug)]
pub struct PackageSpiSegment<'a> {
    /// Clock polarity and phase, 0 to 3.
    pub mode: u8,
    /// Bits per word (0: 8).
    pub bits_per_word: u8,
    /// Not 0: deselect the chip after this transfer.
    pub cs_change: u8,
    pub reserved: u8,
    /// Clock, Hz (0: the device's default).
    pub speed_hz: u32,
    /// Wait after this transfer, microseconds.
    pub delay_us: u16,
    pub reserved2: [u16; 3],
    pub tx: Slice<'a, u8>,
    pub rx: SliceMut<'a, u8>,
}

/// How a line is configured. Each field is numbered by the driver model, in the order its
/// values are declared.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackageLineConfig {
    /// 0 input, 1 output.
    pub direction: u8,
    /// The logical value an output starts at.
    pub initial: u8,
    pub active_low: u8,
    /// 0 as it is, 1 pull up, 2 pull down, 3 disabled.
    pub bias: u8,
    /// 0 push-pull, 1 open drain, 2 open source.
    pub drive: u8,
    /// 0 none, 1 rising, 2 falling, 3 both.
    pub edge: u8,
    pub reserved: u16,
    pub debounce_us: u32,
}

/// An edge an input saw, or none.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackageEdge {
    /// 0: there was no edge, and the rest means nothing.
    pub present: u32,
    pub rising: u32,
    pub seq: u32,
    pub reserved: u32,
    pub timestamp_ns: u64,
}

/// A PWM channel's settings.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackagePwmConfig {
    pub period_ns: u64,
    pub duty_ns: u64,
    /// 0 normal, 1 inversed.
    pub polarity: u32,
    pub enabled: u32,
}

/// Interrupts delivered since the last event, or none.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackageInterruptEvent {
    /// 0: the timeout passed first, and the rest means nothing.
    pub present: u32,
    pub count: u32,
    pub timestamp_ns: u64,
}

/// Memory a block can reach.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackageDmaBuffer {
    pub id: u32,
    pub reserved: u32,
    pub len: u64,
    pub device_address: u64,
}

/// One filled stream buffer.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackageFrame {
    pub buffer: u32,
    pub bytes: u32,
    pub sequence: u64,
    pub identity: u64,
    pub timestamp_ns: u64,
}

/// The fields of a USB control transfer other than its data.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackageControlSetup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub reserved: u16,
}

/// One packet of an isochronous transfer.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackageIsoPacket {
    /// Bytes this packet may carry, set by the caller.
    pub capacity: u32,
    /// Bytes it did carry, set by the transfer.
    pub received: u32,
    /// Not 0: the packet arrived intact.
    pub ok: u32,
}

/// What happened on the wire to a USB function, or nothing.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackageUsbEvent {
    /// One of the `USB_EVENT_*` values.
    pub kind: u32,
    /// [`USB_EVENT_SETUP`]: the request's `wLength`.
    pub length: u32,
    /// [`USB_EVENT_SETUP`]: the request.
    pub setup: PackageControlSetup,
}

pub const USB_EVENT_NONE: u32 = 0;
pub const USB_EVENT_CONFIGURED: u32 = 1;
pub const USB_EVENT_DECONFIGURED: u32 = 2;
pub const USB_EVENT_SUSPENDED: u32 = 3;
pub const USB_EVENT_RESUMED: u32 = 4;
pub const USB_EVENT_SETUP: u32 = 5;

macro_rules! shape {
    ($t:ty $(, $field:ident)*) => {
        &[
            core::mem::size_of::<$t>() as u64,
            core::mem::align_of::<$t>() as u64
            $(, core::mem::offset_of!($t, $field) as u64)*
        ]
    };
}
pub(super) use shape;

/// The size, alignment and field offsets of every value above.
pub(super) const VALUE_SHAPES: [&[u64]; 12] = [
    shape!(Fd, number),
    shape!(PackageTransfer<'static>, kind, delay_ns, write, read),
    shape!(
        PackageSpiSegment<'static>,
        mode,
        bits_per_word,
        cs_change,
        reserved,
        speed_hz,
        delay_us,
        reserved2,
        tx,
        rx
    ),
    shape!(
        PackageLineConfig,
        direction,
        initial,
        active_low,
        bias,
        drive,
        edge,
        reserved,
        debounce_us
    ),
    shape!(PackageEdge, present, rising, seq, reserved, timestamp_ns),
    shape!(PackagePwmConfig, period_ns, duty_ns, polarity, enabled),
    shape!(PackageInterruptEvent, present, count, timestamp_ns),
    shape!(PackageDmaBuffer, id, reserved, len, device_address),
    shape!(
        PackageFrame,
        buffer,
        bytes,
        sequence,
        identity,
        timestamp_ns
    ),
    shape!(
        PackageControlSetup,
        request_type,
        request,
        value,
        index,
        reserved
    ),
    shape!(PackageIsoPacket, capacity, received, ok),
    shape!(PackageUsbEvent, kind, length, setup),
];

// ---- The tables ----

macro_rules! table {
    (
        $(#[$doc:meta])*
        $table:ident served by $host:ident {
            $(
                $(#[$method_doc:meta])*
                fn $name:ident($($arg:ident: $ty:ty),* $(,)?);
            )*
        }
    ) => {
        $(#[$doc])*
        #[repr(C)]
        #[derive(Debug, Clone, Copy)]
        pub struct $table {
            $(
                $(#[$method_doc])*
                pub $name: extern "C" fn($($arg: $ty),*) -> Status,
            )*
        }

        impl $table {
            /// Size, alignment and the offset of every function, for the layout hash.
            pub(super) const SHAPE: &'static [u64] = shape!($table $(, $name)*);

            /// Every function's name and offset, to check a C header against.
            #[cfg(test)]
            pub(crate) const FIELDS: &'static [(&'static str, usize)] =
                &[$((stringify!($name), offset_of!($table, $name))),*];
        }

        #[doc = concat!("The host's side of [`", stringify!($table), "`], in Rust. A function left out answers")]
        /// [`Status::NOT_SUPPORTED`].
        #[cfg(feature = "std")]
        pub trait $host {
            $(
                $(#[$method_doc])*
                fn $name($($arg: $ty),*) -> Status {
                    $(let _ = $arg;)*
                    Status::NOT_SUPPORTED
                }
            )*
        }

        #[cfg(feature = "std")]
        impl $table {
            /// The table that serves `H`: each function runs `H`'s inside
            /// [`guard`](crate::guard).
            pub const fn over<H: $host>() -> Self {
                $(
                    extern "C" fn $name<H: $host>($($arg: $ty),*) -> Status {
                        crate::guard(|| H::$name($($arg),*))
                    }
                )*
                Self { $($name: $name::<H>),* }
            }
        }
    };
}

table! {
    /// Another device, taken with [`TAKE_DEVICE`].
    DeviceTable served by DeviceHost {
        /// Calls a method on the contract the device was taken for. The reply's length goes to
        /// `len`.
        fn call(
            device: Handle,
            method: u16,
            args: Slice<'_, u8>,
            reply: SliceMut<'_, u8>,
            len: Out<'_, usize>,
        );
        fn health(device: Handle, health: Out<'_, PackageHealth>);
    }
}

table! {
    /// An I2C bus.
    I2cTable served by I2cHost {
        /// Runs one transaction of at most [`MAX_TRANSFER_STEPS`] steps, each a write or a read.
        fn transaction(bus: Handle, address: u16, steps: SliceMut<'_, PackageTransfer<'_>>);
    }
}

table! {
    /// An SPI device: one bus and one chip select.
    SpiTable served by SpiHost {
        /// Runs one transaction of at most [`MAX_TRANSFER_STEPS`] steps on a device taken with
        /// [`TAKE_SPI`], the chip selected throughout.
        fn transaction(device: Handle, steps: SliceMut<'_, PackageTransfer<'_>>);
        /// Runs one transaction of at most [`MAX_TRANSFER_STEPS`] segments, each with its own
        /// settings, on a device taken with [`TAKE_RAW_SPI`].
        fn transfer(device: Handle, segments: SliceMut<'_, PackageSpiSegment<'_>>);
    }
}

table! {
    /// A claimed GPIO line.
    LineTable served by LineHost {
        fn configure(line: Handle, config: PackageLineConfig);
        /// The logical value: 0 or 1.
        fn get(line: Handle, value: Out<'_, u32>);
        fn set(line: Handle, value: u32);
        /// The next pending edge, without waiting.
        fn read_edge(line: Handle, edge: Out<'_, PackageEdge>);
    }
}

table! {
    /// A claimed PWM channel.
    PwmTable served by PwmHost {
        fn configure(pwm: Handle, config: PackagePwmConfig);
        fn config(pwm: Handle, config: Out<'_, PackagePwmConfig>);
    }
}

table! {
    /// A serial port.
    UartTable served by UartHost {
        fn set_baud(uart: Handle, baud: u32);
        fn write(uart: Handle, data: Slice<'_, u8>, len: Out<'_, usize>);
        /// What arrived within `timeout_ns`: 0 bytes when nothing did.
        fn read(uart: Handle, out: SliceMut<'_, u8>, timeout_ns: u64, len: Out<'_, usize>);
    }
}

table! {
    /// A memory-mapped register window.
    RegistersTable served by RegistersHost {
        fn size(registers: Handle, size: Out<'_, usize>);
        fn read32(registers: Handle, offset: usize, value: Out<'_, u32>);
        fn write32(registers: Handle, offset: usize, value: u32);
        fn modify32(registers: Handle, offset: usize, clear: u32, set: u32);
        fn write_block(registers: Handle, offset: usize, values: Slice<'_, u32>);
    }
}

table! {
    /// An interrupt line.
    InterruptTable served by InterruptHost {
        fn enable(interrupt: Handle);
        fn disable(interrupt: Handle);
        /// Waits up to `timeout_ns` ([`NO_TIMEOUT`]: without limit; 0: only checks).
        fn wait(interrupt: Handle, timeout_ns: u64, event: Out<'_, PackageInterruptEvent>);
    }
}

table! {
    /// DMA memory.
    DmaTable served by DmaHost {
        fn allocate(dma: Handle, len: u64, buffer: Out<'_, PackageDmaBuffer>);
        fn release(dma: Handle, id: u32);
        fn write(dma: Handle, id: u32, offset: u64, data: Slice<'_, u8>);
        fn read(dma: Handle, id: u32, offset: u64, out: SliceMut<'_, u8>);
        fn sync_for_device(dma: Handle, id: u32);
        fn sync_for_cpu(dma: Handle, id: u32);
    }
}

table! {
    /// The clocks of a block.
    ClocksTable served by ClocksHost {
        fn enable(clocks: Handle, clock: u32);
        fn disable(clocks: Handle, clock: u32);
        fn rate_hz(clocks: Handle, clock: u32, hz: Out<'_, u64>);
        /// Asks for a rate and writes the one the clock settled on.
        fn set_rate_hz(clocks: Handle, clock: u32, hz: u64, settled: Out<'_, u64>);
    }
}

table! {
    /// The reset lines of a block.
    ResetTable served by ResetHost {
        fn assert_reset(reset: Handle, line: u32);
        fn deassert_reset(reset: Handle, line: u32);
    }
}

table! {
    /// The producer's end of a stream of buffers.
    StreamTable served by StreamHost {
        fn buffer_count(stream: Handle, count: Out<'_, u32>);
        fn publish(stream: Handle, frame: PackageFrame);
        /// The next buffer the consumer has returned, or [`NO_BUFFER`].
        fn next_buffer(stream: Handle, buffer: Out<'_, u32>);
        /// Copies the consumer's next job into `out` and writes its length, or [`NO_JOB`].
        fn next_job(stream: Handle, out: SliceMut<'_, u8>, len: Out<'_, usize>);
        /// Copies `data` into `buffer` at `offset`. `attached` is 0 when no consumer is.
        fn write(
            stream: Handle,
            buffer: u32,
            offset: usize,
            data: Slice<'_, u8>,
            attached: Out<'_, u32>,
        );
        /// Copies from `buffer` at `offset` into `out`. `attached` is 0 when no consumer is.
        fn read(
            stream: Handle,
            buffer: u32,
            offset: usize,
            out: SliceMut<'_, u8>,
            attached: Out<'_, u32>,
        );
        /// The buffer's memory as a descriptor, lent until the stream's next call, or
        /// [`Fd::NONE`] when nothing is behind the buffer now.
        fn buffer_handle(stream: Handle, buffer: u32, fd: Out<'_, Fd>);
        /// Whether `buffer` was taken under the consumer attached now: 0 for no.
        fn buffer_current(stream: Handle, buffer: u32, current: Out<'_, u32>);
        /// What the stream's buffers are: [`BUFFERS_PLAIN`] or [`BUFFERS_HANDLES`].
        fn buffer_kind(stream: Handle, kind: Out<'_, u32>);
        /// Ends the attached consumer's stream as faulted.
        fn fault_consumer(stream: Handle);
    }
}

table! {
    /// An opened USB device.
    UsbDeviceTable served by UsbDeviceHost {
        fn config_descriptor(device: Handle, index: u8, out: SliceMut<'_, u8>, len: Out<'_, usize>);
        fn reset(device: Handle);
        fn detach_kernel_driver(device: Handle, interface: u8);
        fn claim_interface(device: Handle, interface: u8);
        fn release_interface(device: Handle, interface: u8);
        fn set_alternate(device: Handle, interface: u8, alternate: u8);
        fn control_in(
            device: Handle,
            setup: PackageControlSetup,
            data: SliceMut<'_, u8>,
            timeout_ns: u64,
            len: Out<'_, usize>,
        );
        fn control_out(
            device: Handle,
            setup: PackageControlSetup,
            data: Slice<'_, u8>,
            timeout_ns: u64,
            len: Out<'_, usize>,
        );
        fn bulk_in(
            device: Handle,
            endpoint: u8,
            data: SliceMut<'_, u8>,
            timeout_ns: u64,
            len: Out<'_, usize>,
        );
        fn bulk_out(
            device: Handle,
            endpoint: u8,
            data: Slice<'_, u8>,
            timeout_ns: u64,
            len: Out<'_, usize>,
        );
        fn interrupt_in(
            device: Handle,
            endpoint: u8,
            data: SliceMut<'_, u8>,
            timeout_ns: u64,
            len: Out<'_, usize>,
        );
        /// Starts an isochronous stream from the IN `endpoint`: `transfers` transfers of
        /// `packets` packets, at most [`MAX_ISO_PACKETS`], of `packet_bytes` each, kept queued.
        fn iso_in_start(
            device: Handle,
            endpoint: u8,
            transfers: u8,
            packets: u16,
            packet_bytes: u32,
        );
        /// Takes the stream's oldest done transfer into `data`, packet `i` at `i *
        /// packet_bytes`, and queues it again.
        fn iso_in_next(
            device: Handle,
            data: SliceMut<'_, u8>,
            packets: SliceMut<'_, PackageIsoPacket>,
            timeout_ns: u64,
        );
        /// Ends the stream.
        fn iso_in_stop(device: Handle);
    }
}

table! {
    /// This board appearing as a USB device.
    UsbFunctionTable served by UsbFunctionHost {
        fn enable(function: Handle, descriptors: Slice<'_, u8>);
        fn disable(function: Handle);
        /// What happened since the last call, or [`USB_EVENT_NONE`].
        fn next_event(function: Handle, event: Out<'_, PackageUsbEvent>);
        fn control_in(function: Handle, data: Slice<'_, u8>, timeout_ns: u64, len: Out<'_, usize>);
        fn control_out(
            function: Handle,
            data: SliceMut<'_, u8>,
            timeout_ns: u64,
            len: Out<'_, usize>,
        );
        fn control_stall(function: Handle);
        fn set_halt(function: Handle, endpoint: u8, halted: u32);
        fn read(
            function: Handle,
            endpoint: u8,
            data: SliceMut<'_, u8>,
            timeout_ns: u64,
            len: Out<'_, usize>,
        );
        fn write(
            function: Handle,
            endpoint: u8,
            data: Slice<'_, u8>,
            timeout_ns: u64,
            len: Out<'_, usize>,
        );
    }
}

/// The size, alignment and function offsets of every table above.
pub(super) const TABLE_SHAPES: [&[u64]; 14] = [
    DeviceTable::SHAPE,
    I2cTable::SHAPE,
    SpiTable::SHAPE,
    LineTable::SHAPE,
    PwmTable::SHAPE,
    UartTable::SHAPE,
    RegistersTable::SHAPE,
    InterruptTable::SHAPE,
    DmaTable::SHAPE,
    ClocksTable::SHAPE,
    ResetTable::SHAPE,
    StreamTable::SHAPE,
    UsbDeviceTable::SHAPE,
    UsbFunctionTable::SHAPE,
];
