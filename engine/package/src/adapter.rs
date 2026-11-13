//! The host's end of a loaded package: a [`Binder`] that has the package build its driver, and
//! the [`Instance`] the host then runs like any other. Also what the driver is given: the table
//! of handles behind the package's calls back into the host.

use std::panic::resume_unwind;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lemnos_abi::package::{
    BUFFERS_HANDLES, BUFFERS_PLAIN, ClocksHost, DRIVER_TICKS, DRIVER_TRACKS_CALLERS, DeviceHost,
    DmaHost, DriverTable, Fd, HostTable, I2cHost, InterruptHost, LineHost, Loaded, MAX_ISO_PACKETS,
    MAX_TRANSFER_STEPS, NO_BUFFER, NO_JOB, NO_TIMEOUT, Out, PackageBinding, PackageControlSetup,
    PackageDmaBuffer, PackageDriver, PackageEdge, PackageFrame, PackageHealth, PackageHost,
    PackageInterruptEvent, PackageIsoPacket, PackageItem, PackageLineConfig, PackagePwmConfig,
    PackageSpiSegment, PackageTransfer, PackageUsbEvent, PwmHost, RegistersHost, ResetHost, Slice,
    SliceMut, SpiHost, StreamHost, TAKE_CLOCKS, TAKE_DEVICE, TAKE_DMA, TAKE_I2C, TAKE_INTERRUPT,
    TAKE_LINE, TAKE_PWM, TAKE_RAW_SPI, TAKE_REGISTERS, TAKE_RESET, TAKE_SPI, TAKE_STREAM,
    TAKE_UART, TAKE_USB_DEVICE, TAKE_USB_FUNCTION, TRANSFER_BOTH, TRANSFER_DELAY,
    TRANSFER_IN_PLACE, TRANSFER_READ, TRANSFER_WRITE, UartHost, UsbDeviceHost, UsbFunctionHost,
    host_table,
};
use lemnos_abi::{Handle, Slab};
use lemnos_core::package::{
    dma_buffer_out, edge_out, frame_in, health_in, health_out, interrupt_event_out, item_in,
    line_config_in, pwm_config_in, pwm_config_out, segment_in, settings_out, setup_in,
    status_of_bus, timeout_in, usb_event_out,
};
use lemnos_core::{
    Caller, CallerId, ClocksHandle, Config, Description, Dev, DmaHandle, ErrorKind, Health, I2c,
    InterruptHandle, Line, MAX_ITEMS, Manifest, MethodId, NeedKind, Pwm, RawSpiBus,
    RegistersHandle, Reply, ResetHandle, Resources, Result, Spi, Status, StreamHandle, Uart,
    UsbDeviceHandle, UsbFunctionHandle,
};
use lemnos_hal::blocks::{BufferKind, IsoPacket};
use lemnos_hal::i2c::{I2c as _, Operation};
use lemnos_hal::raw::SpiSegment;
use lemnos_hal::spi::{self, SpiDevice as _};
use lemnos_host::{Binder, DriverEntry, Instance};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Something a package's driver holds, by handle. Each is behind its own lock, so a call stuck
/// in one holds up nothing else.
#[derive(Clone)]
enum Resource {
    Device(Arc<Mutex<Dev<()>>>),
    I2c(Arc<Mutex<I2c>>),
    Spi(Arc<Mutex<Spi>>),
    RawSpi(Arc<Mutex<RawSpiBus>>),
    Line(Arc<Mutex<Line>>),
    Pwm(Arc<Mutex<Pwm>>),
    Uart(Arc<Mutex<Uart>>),
    UsbDevice(Arc<Mutex<UsbDeviceHandle>>),
    UsbFunction(Arc<Mutex<UsbFunctionHandle>>),
    Registers(Arc<Mutex<RegistersHandle>>),
    Interrupt(Arc<Mutex<InterruptHandle>>),
    Dma(Arc<Mutex<DmaHandle>>),
    Clocks(Arc<Mutex<ClocksHandle>>),
    Reset(Arc<Mutex<ResetHandle>>),
    Stream(Arc<Mutex<StreamHandle>>),
}

/// Everything every package's driver holds. Locked only to look a handle up, never across a
/// call.
static RESOURCES: Mutex<Slab<Resource>> = Mutex::new(Slab::new());

pub(crate) fn resources_held() -> usize {
    lock(&RESOURCES).len()
}

fn resource(handle: Handle) -> Result<Resource> {
    lock(&RESOURCES).get(handle).cloned()
}

fn release(handle: Handle) -> Status {
    let removed = lock(&RESOURCES).remove(handle);
    // Dropped here, with the table unlocked.
    match removed {
        Ok(_) => Status::OK,
        Err(status) => status,
    }
}

/// The handles one bind gave out. Whatever the driver has not released by the time it is gone
/// is released here, so a driver that panics or forgets leaks nothing.
#[derive(Default)]
struct Taken(Vec<Handle>);

impl Drop for Taken {
    fn drop(&mut self) {
        for handle in self.0.drain(..) {
            let _ = release(handle);
        }
    }
}

/// What a binding driver may ask for: what its board entry says it may have.
struct Asked<'a, 'b> {
    resources: &'a mut Resources<'b>,
    manifest: &'static Manifest,
    taken: Taken,
}

fn kept<T>(resource: T) -> Arc<Mutex<T>> {
    Arc::new(Mutex::new(resource))
}

impl PackageBinding for Asked<'_, '_> {
    fn provided(&mut self, need: &str) -> bool {
        self.resources.provided(need)
    }

    fn take(&mut self, kind: u32, need: &str, tag: &str) -> Result<Handle> {
        let res = &mut *self.resources;
        let resource = match kind {
            TAKE_DEVICE => {
                // Only a device the manifest declared, for the contract it declared.
                let declared = self.manifest.needs.iter().find_map(|n| match n.kind {
                    NeedKind::Device(declared) if n.name == need && declared.as_str() == tag => {
                        Some(declared)
                    }
                    _ => None,
                });
                let declared = declared.ok_or(Status::INVALID_ARGUMENT)?;
                let caller = res.provider().device(need, declared)?;
                Resource::Device(kept(Dev::new(caller)))
            }
            TAKE_I2C => Resource::I2c(kept(res.i2c(need)?)),
            TAKE_SPI => Resource::Spi(kept(res.spi(need)?)),
            TAKE_RAW_SPI => Resource::RawSpi(kept(res.raw_spi(need)?)),
            TAKE_LINE => Resource::Line(kept(res.line(need)?)),
            TAKE_PWM => Resource::Pwm(kept(res.pwm(need)?)),
            TAKE_UART => Resource::Uart(kept(res.uart(need)?)),
            TAKE_USB_DEVICE => Resource::UsbDevice(kept(res.usb_device(need)?)),
            TAKE_USB_FUNCTION => Resource::UsbFunction(kept(res.usb_function(need)?)),
            TAKE_REGISTERS => Resource::Registers(kept(res.registers(need)?)),
            TAKE_INTERRUPT => Resource::Interrupt(kept(res.interrupt(need)?)),
            TAKE_DMA => Resource::Dma(kept(res.dma(need)?)),
            TAKE_CLOCKS => Resource::Clocks(kept(res.clocks(need)?)),
            TAKE_RESET => Resource::Reset(kept(res.reset(need)?)),
            TAKE_STREAM => Resource::Stream(kept(res.stream(need)?)),
            _ => return Err(Status::NOT_SUPPORTED),
        };
        let handle = lock(&RESOURCES).insert(resource)?;
        self.taken.0.push(handle);
        Ok(handle)
    }
}

/// What a bound driver may do with what it holds.
struct Held;

/// The resource of one kind behind a handle, or the function's answer when the handle is stale
/// or names another kind of resource.
macro_rules! held {
    ($handle:expr, $kind:ident) => {
        match resource($handle) {
            Ok(Resource::$kind(held)) => held,
            Ok(_) => return Status::INVALID_ARGUMENT,
            Err(status) => return status,
        }
    };
}

/// A resource's answer as a status, its value written where the driver asked.
fn answer<T, U>(
    result: core::result::Result<T, ErrorKind>,
    out: Out<'_, U>,
    carry: impl FnOnce(T) -> U,
) -> Status {
    match result {
        Ok(value) => {
            out.set(carry(value));
            Status::OK
        }
        Err(kind) => status_of_bus(kind),
    }
}

fn done(result: core::result::Result<(), ErrorKind>) -> Status {
    match result {
        Ok(()) => Status::OK,
        Err(kind) => status_of_bus(kind),
    }
}

impl PackageHost for Held {
    fn release(resource: Handle) -> Status {
        release(resource)
    }
}

impl DeviceHost for Held {
    fn call(
        device: Handle,
        method: u16,
        args: Slice<'_, u8>,
        reply: SliceMut<'_, u8>,
        len: Out<'_, usize>,
    ) -> Status {
        let device = held!(device, Device);
        let mut device = lock(&device);
        let (args, reply) = (args.as_slice(), reply.into_slice());
        let Some(room) = device.caller.args().get_mut(..args.len()) else {
            return Status::BUFFER_TOO_SMALL;
        };
        room.copy_from_slice(args);
        match device.caller.call(method, args.len(), reply) {
            Ok(used) => {
                len.set(used.min(reply.len()));
                Status::OK
            }
            Err(status) if status.is_ok() => Status::IO,
            Err(status) => status,
        }
    }

    fn health(device: Handle, health: Out<'_, PackageHealth>) -> Status {
        health.set(match resource(device) {
            Ok(Resource::Device(device)) => health_out(lock(&device).health()),
            _ => health_out(Health::Failed("missing")),
        });
        Status::OK
    }
}

impl I2cHost for Held {
    fn transaction(
        bus: Handle,
        address: u16,
        mut steps: SliceMut<'_, PackageTransfer<'_>>,
    ) -> Status {
        let bus = held!(bus, I2c);
        let Ok(address) = u8::try_from(address) else {
            return Status::INVALID_ARGUMENT;
        };
        let steps = steps.as_mut_slice();
        if steps.len() > MAX_TRANSFER_STEPS {
            return Status::INVALID_ARGUMENT;
        }
        let count = steps.len();
        let mut operations: [Operation<'_>; MAX_TRANSFER_STEPS] =
            core::array::from_fn(|_| Operation::Write(&[]));
        for (operation, step) in operations.iter_mut().zip(steps.iter_mut()) {
            *operation = match step.kind {
                TRANSFER_READ => Operation::Read(step.read.as_mut_slice()),
                TRANSFER_WRITE => Operation::Write(step.write.as_slice()),
                _ => return Status::INVALID_ARGUMENT,
            };
        }
        done(lock(&bus).transaction(address, &mut operations[..count]))
    }
}

impl SpiHost for Held {
    fn transaction(device: Handle, mut steps: SliceMut<'_, PackageTransfer<'_>>) -> Status {
        let device = held!(device, Spi);
        let steps = steps.as_mut_slice();
        if steps.len() > MAX_TRANSFER_STEPS {
            return Status::INVALID_ARGUMENT;
        }
        let count = steps.len();
        let mut operations: [spi::Operation<'_, u8>; MAX_TRANSFER_STEPS] =
            core::array::from_fn(|_| spi::Operation::Write(&[]));
        for (operation, step) in operations.iter_mut().zip(steps.iter_mut()) {
            *operation = match step.kind {
                TRANSFER_READ => spi::Operation::Read(step.read.as_mut_slice()),
                TRANSFER_WRITE => spi::Operation::Write(step.write.as_slice()),
                TRANSFER_BOTH => {
                    spi::Operation::Transfer(step.read.as_mut_slice(), step.write.as_slice())
                }
                TRANSFER_IN_PLACE => spi::Operation::TransferInPlace(step.read.as_mut_slice()),
                TRANSFER_DELAY => spi::Operation::DelayNs(step.delay_ns),
                _ => return Status::INVALID_ARGUMENT,
            };
        }
        done(lock(&device).transaction(&mut operations[..count]))
    }

    fn transfer(device: Handle, mut segments: SliceMut<'_, PackageSpiSegment<'_>>) -> Status {
        let device = held!(device, RawSpi);
        let carried = segments.as_mut_slice();
        if carried.len() > MAX_TRANSFER_STEPS {
            return Status::INVALID_ARGUMENT;
        }
        let count = carried.len();
        let mut carried = carried.iter_mut();
        let mut row: [SpiSegment<'_>; MAX_TRANSFER_STEPS] =
            core::array::from_fn(|_| match carried.next() {
                Some(segment) => segment_in(segment),
                None => SpiSegment {
                    config: Default::default(),
                    tx: &[],
                    rx: &mut [],
                    cs_change: false,
                    delay_us: 0,
                },
            });
        done(lock(&device).transfer(&mut row[..count]))
    }
}

impl LineHost for Held {
    fn configure(line: Handle, config: PackageLineConfig) -> Status {
        let line = held!(line, Line);
        let Some(config) = line_config_in(config) else {
            return Status::INVALID_ARGUMENT;
        };
        done(lock(&line).configure(&config))
    }

    fn get(line: Handle, value: Out<'_, u32>) -> Status {
        let line = held!(line, Line);
        answer(lock(&line).get(), value, u32::from)
    }

    fn set(line: Handle, value: u32) -> Status {
        let line = held!(line, Line);
        done(lock(&line).set(value != 0))
    }

    fn read_edge(line: Handle, edge: Out<'_, PackageEdge>) -> Status {
        let line = held!(line, Line);
        answer(lock(&line).read_edge(), edge, edge_out)
    }
}

impl PwmHost for Held {
    fn configure(pwm: Handle, config: PackagePwmConfig) -> Status {
        let pwm = held!(pwm, Pwm);
        let Some(config) = pwm_config_in(config) else {
            return Status::INVALID_ARGUMENT;
        };
        done(lock(&pwm).configure(&config))
    }

    fn config(pwm: Handle, config: Out<'_, PackagePwmConfig>) -> Status {
        let pwm = held!(pwm, Pwm);
        answer(lock(&pwm).config(), config, |config| {
            pwm_config_out(&config)
        })
    }
}

impl UartHost for Held {
    fn set_baud(uart: Handle, baud: u32) -> Status {
        let uart = held!(uart, Uart);
        done(lock(&uart).set_baud(baud))
    }

    fn write(uart: Handle, data: Slice<'_, u8>, len: Out<'_, usize>) -> Status {
        let uart = held!(uart, Uart);
        let data = data.as_slice();
        answer(lock(&uart).write(data), len, |sent| sent.min(data.len()))
    }

    fn read(uart: Handle, out: SliceMut<'_, u8>, timeout_ns: u64, len: Out<'_, usize>) -> Status {
        let uart = held!(uart, Uart);
        let out = out.into_slice();
        let room = out.len();
        answer(lock(&uart).read(out, timeout_in(timeout_ns)), len, |got| {
            got.min(room)
        })
    }
}

impl RegistersHost for Held {
    fn size(registers: Handle, size: Out<'_, usize>) -> Status {
        let registers = held!(registers, Registers);
        size.set(lock(&registers).size());
        Status::OK
    }

    fn read32(registers: Handle, offset: usize, value: Out<'_, u32>) -> Status {
        let registers = held!(registers, Registers);
        answer(lock(&registers).read32(offset), value, |value| value)
    }

    fn write32(registers: Handle, offset: usize, value: u32) -> Status {
        let registers = held!(registers, Registers);
        done(lock(&registers).write32(offset, value))
    }

    fn modify32(registers: Handle, offset: usize, clear: u32, set: u32) -> Status {
        let registers = held!(registers, Registers);
        done(lock(&registers).modify32(offset, clear, set))
    }

    fn write_block(registers: Handle, offset: usize, values: Slice<'_, u32>) -> Status {
        let registers = held!(registers, Registers);
        let values = values.as_slice();
        // The whole block lies inside the window, or none of it is written.
        let mut registers = lock(&registers);
        let end = values
            .len()
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(offset));
        if end.is_none_or(|end| end > registers.size()) {
            return Status::INVALID_ARGUMENT;
        }
        done(registers.write_block(offset, values))
    }
}

impl InterruptHost for Held {
    fn enable(interrupt: Handle) -> Status {
        let interrupt = held!(interrupt, Interrupt);
        done(lock(&interrupt).enable())
    }

    fn disable(interrupt: Handle) -> Status {
        let interrupt = held!(interrupt, Interrupt);
        done(lock(&interrupt).disable())
    }

    fn wait(interrupt: Handle, timeout_ns: u64, event: Out<'_, PackageInterruptEvent>) -> Status {
        let interrupt = held!(interrupt, Interrupt);
        let timeout = (timeout_ns != NO_TIMEOUT).then(|| timeout_in(timeout_ns));
        answer(lock(&interrupt).wait(timeout), event, interrupt_event_out)
    }
}

impl DmaHost for Held {
    fn allocate(dma: Handle, len: u64, buffer: Out<'_, PackageDmaBuffer>) -> Status {
        let dma = held!(dma, Dma);
        answer(lock(&dma).allocate(len), buffer, dma_buffer_out)
    }

    fn release(dma: Handle, id: u32) -> Status {
        let dma = held!(dma, Dma);
        done(lock(&dma).release(id))
    }

    fn write(dma: Handle, id: u32, offset: u64, data: Slice<'_, u8>) -> Status {
        let dma = held!(dma, Dma);
        done(lock(&dma).write(id, offset, data.as_slice()))
    }

    fn read(dma: Handle, id: u32, offset: u64, out: SliceMut<'_, u8>) -> Status {
        let dma = held!(dma, Dma);
        done(lock(&dma).read(id, offset, out.into_slice()))
    }

    fn sync_for_device(dma: Handle, id: u32) -> Status {
        let dma = held!(dma, Dma);
        done(lock(&dma).sync_for_device(id))
    }

    fn sync_for_cpu(dma: Handle, id: u32) -> Status {
        let dma = held!(dma, Dma);
        done(lock(&dma).sync_for_cpu(id))
    }
}

impl ClocksHost for Held {
    fn enable(clocks: Handle, clock: u32) -> Status {
        let clocks = held!(clocks, Clocks);
        done(lock(&clocks).enable(clock))
    }

    fn disable(clocks: Handle, clock: u32) -> Status {
        let clocks = held!(clocks, Clocks);
        done(lock(&clocks).disable(clock))
    }

    fn rate_hz(clocks: Handle, clock: u32, hz: Out<'_, u64>) -> Status {
        let clocks = held!(clocks, Clocks);
        answer(lock(&clocks).rate_hz(clock), hz, |hz| hz)
    }

    fn set_rate_hz(clocks: Handle, clock: u32, hz: u64, settled: Out<'_, u64>) -> Status {
        let clocks = held!(clocks, Clocks);
        answer(lock(&clocks).set_rate_hz(clock, hz), settled, |hz| hz)
    }
}

impl ResetHost for Held {
    fn assert_reset(reset: Handle, line: u32) -> Status {
        let reset = held!(reset, Reset);
        done(lock(&reset).assert(line))
    }

    fn deassert_reset(reset: Handle, line: u32) -> Status {
        let reset = held!(reset, Reset);
        done(lock(&reset).deassert(line))
    }
}

impl StreamHost for Held {
    fn buffer_count(stream: Handle, count: Out<'_, u32>) -> Status {
        let stream = held!(stream, Stream);
        count.set(lock(&stream).buffer_count());
        Status::OK
    }

    fn publish(stream: Handle, frame: PackageFrame) -> Status {
        let stream = held!(stream, Stream);
        done(lock(&stream).publish(frame_in(frame)))
    }

    fn next_buffer(stream: Handle, buffer: Out<'_, u32>) -> Status {
        let stream = held!(stream, Stream);
        answer(lock(&stream).next_buffer(), buffer, |buffer| {
            buffer.unwrap_or(NO_BUFFER)
        })
    }

    fn next_job(stream: Handle, out: SliceMut<'_, u8>, len: Out<'_, usize>) -> Status {
        let stream = held!(stream, Stream);
        let out = out.into_slice();
        let room = out.len();
        answer(lock(&stream).next_job(out), len, |job| {
            job.map_or(NO_JOB, |len| len.min(room))
        })
    }

    fn write(
        stream: Handle,
        buffer: u32,
        offset: usize,
        data: Slice<'_, u8>,
        attached: Out<'_, u32>,
    ) -> Status {
        let stream = held!(stream, Stream);
        answer(
            lock(&stream).write(buffer, offset, data.as_slice()),
            attached,
            u32::from,
        )
    }

    fn read(
        stream: Handle,
        buffer: u32,
        offset: usize,
        out: SliceMut<'_, u8>,
        attached: Out<'_, u32>,
    ) -> Status {
        let stream = held!(stream, Stream);
        answer(
            lock(&stream).read(buffer, offset, out.into_slice()),
            attached,
            u32::from,
        )
    }

    fn buffer_handle(stream: Handle, buffer: u32, fd: Out<'_, Fd>) -> Status {
        let stream = held!(stream, Stream);
        // The stream keeps the descriptor open until its next call, which only the driver that
        // holds this handle can make.
        answer(lock(&stream).buffer_handle(buffer), fd, |handle| {
            handle.map_or(Fd::NONE, |handle| Fd::lend(handle.fd()))
        })
    }

    fn buffer_current(stream: Handle, buffer: u32, current: Out<'_, u32>) -> Status {
        let stream = held!(stream, Stream);
        answer(lock(&stream).buffer_current(buffer), current, u32::from)
    }

    fn buffer_kind(stream: Handle, kind: Out<'_, u32>) -> Status {
        let stream = held!(stream, Stream);
        kind.set(match lock(&stream).buffer_kind() {
            BufferKind::Plain => BUFFERS_PLAIN,
            BufferKind::Handles => BUFFERS_HANDLES,
        });
        Status::OK
    }

    fn fault_consumer(stream: Handle) -> Status {
        let stream = held!(stream, Stream);
        done(lock(&stream).fault_consumer())
    }
}

impl UsbDeviceHost for Held {
    fn config_descriptor(
        device: Handle,
        index: u8,
        out: SliceMut<'_, u8>,
        len: Out<'_, usize>,
    ) -> Status {
        let device = held!(device, UsbDevice);
        let out = out.into_slice();
        let room = out.len();
        answer(lock(&device).config_descriptor(index, out), len, |got| {
            got.min(room)
        })
    }

    fn reset(device: Handle) -> Status {
        let device = held!(device, UsbDevice);
        done(lock(&device).reset())
    }

    fn detach_kernel_driver(device: Handle, interface: u8) -> Status {
        let device = held!(device, UsbDevice);
        done(lock(&device).detach_kernel_driver(interface))
    }

    fn claim_interface(device: Handle, interface: u8) -> Status {
        let device = held!(device, UsbDevice);
        done(lock(&device).claim_interface(interface))
    }

    fn release_interface(device: Handle, interface: u8) -> Status {
        let device = held!(device, UsbDevice);
        done(lock(&device).release_interface(interface))
    }

    fn set_alternate(device: Handle, interface: u8, alternate: u8) -> Status {
        let device = held!(device, UsbDevice);
        done(lock(&device).set_alternate(interface, alternate))
    }

    fn control_in(
        device: Handle,
        setup: PackageControlSetup,
        data: SliceMut<'_, u8>,
        timeout_ns: u64,
        len: Out<'_, usize>,
    ) -> Status {
        let device = held!(device, UsbDevice);
        let data = data.into_slice();
        let room = data.len();
        answer(
            lock(&device).control_in(setup_in(setup), data, timeout_in(timeout_ns)),
            len,
            |got| got.min(room),
        )
    }

    fn control_out(
        device: Handle,
        setup: PackageControlSetup,
        data: Slice<'_, u8>,
        timeout_ns: u64,
        len: Out<'_, usize>,
    ) -> Status {
        let device = held!(device, UsbDevice);
        let data = data.as_slice();
        answer(
            lock(&device).control_out(setup_in(setup), data, timeout_in(timeout_ns)),
            len,
            |sent| sent.min(data.len()),
        )
    }

    fn bulk_in(
        device: Handle,
        endpoint: u8,
        data: SliceMut<'_, u8>,
        timeout_ns: u64,
        len: Out<'_, usize>,
    ) -> Status {
        let device = held!(device, UsbDevice);
        let data = data.into_slice();
        let room = data.len();
        answer(
            lock(&device).bulk_in(endpoint, data, timeout_in(timeout_ns)),
            len,
            |got| got.min(room),
        )
    }

    fn bulk_out(
        device: Handle,
        endpoint: u8,
        data: Slice<'_, u8>,
        timeout_ns: u64,
        len: Out<'_, usize>,
    ) -> Status {
        let device = held!(device, UsbDevice);
        let data = data.as_slice();
        answer(
            lock(&device).bulk_out(endpoint, data, timeout_in(timeout_ns)),
            len,
            |sent| sent.min(data.len()),
        )
    }

    fn interrupt_in(
        device: Handle,
        endpoint: u8,
        data: SliceMut<'_, u8>,
        timeout_ns: u64,
        len: Out<'_, usize>,
    ) -> Status {
        let device = held!(device, UsbDevice);
        let data = data.into_slice();
        let room = data.len();
        answer(
            lock(&device).interrupt_in(endpoint, data, timeout_in(timeout_ns)),
            len,
            |got| got.min(room),
        )
    }

    fn iso_in_start(
        device: Handle,
        endpoint: u8,
        transfers: u8,
        packets: u16,
        packet_bytes: u32,
    ) -> Status {
        let device = held!(device, UsbDevice);
        if usize::from(packets) > MAX_ISO_PACKETS {
            return Status::INVALID_ARGUMENT;
        }
        done(lock(&device).iso_in_start(endpoint, transfers, packets, packet_bytes))
    }

    fn iso_in_next(
        device: Handle,
        data: SliceMut<'_, u8>,
        mut packets: SliceMut<'_, PackageIsoPacket>,
        timeout_ns: u64,
    ) -> Status {
        let device = held!(device, UsbDevice);
        let (data, carried) = (data.into_slice(), packets.as_mut_slice());
        if carried.is_empty() || carried.len() > MAX_ISO_PACKETS {
            return Status::INVALID_ARGUMENT;
        }
        // Packet `i` starts at `i * capacity`: none had more room than `data` shared out evenly.
        let room = u32::try_from(data.len() / carried.len()).unwrap_or(u32::MAX);
        let mut row = [IsoPacket::default(); MAX_ISO_PACKETS];
        let count = carried.len();
        let result = lock(&device).iso_in_next(data, &mut row[..count], timeout_in(timeout_ns));
        for (carried, packet) in carried.iter_mut().zip(row.iter()) {
            carried.capacity = packet.capacity.min(room);
            carried.received = packet.received.min(carried.capacity);
            carried.ok = u32::from(packet.ok);
        }
        done(result)
    }

    fn iso_in_stop(device: Handle) -> Status {
        let device = held!(device, UsbDevice);
        done(lock(&device).iso_in_stop())
    }
}

impl UsbFunctionHost for Held {
    fn enable(function: Handle, descriptors: Slice<'_, u8>) -> Status {
        let function = held!(function, UsbFunction);
        done(lock(&function).enable(descriptors.as_slice()))
    }

    fn disable(function: Handle) -> Status {
        let function = held!(function, UsbFunction);
        done(lock(&function).disable())
    }

    fn next_event(function: Handle, event: Out<'_, PackageUsbEvent>) -> Status {
        let function = held!(function, UsbFunction);
        answer(lock(&function).next_event(), event, usb_event_out)
    }

    fn control_in(
        function: Handle,
        data: Slice<'_, u8>,
        timeout_ns: u64,
        len: Out<'_, usize>,
    ) -> Status {
        let function = held!(function, UsbFunction);
        let data = data.as_slice();
        answer(
            lock(&function).control_in(data, timeout_in(timeout_ns)),
            len,
            |sent| sent.min(data.len()),
        )
    }

    fn control_out(
        function: Handle,
        data: SliceMut<'_, u8>,
        timeout_ns: u64,
        len: Out<'_, usize>,
    ) -> Status {
        let function = held!(function, UsbFunction);
        let data = data.into_slice();
        let room = data.len();
        answer(
            lock(&function).control_out(data, timeout_in(timeout_ns)),
            len,
            |got| got.min(room),
        )
    }

    fn control_stall(function: Handle) -> Status {
        let function = held!(function, UsbFunction);
        done(lock(&function).control_stall())
    }

    fn set_halt(function: Handle, endpoint: u8, halted: u32) -> Status {
        let function = held!(function, UsbFunction);
        done(lock(&function).set_halt(endpoint, halted != 0))
    }

    fn read(
        function: Handle,
        endpoint: u8,
        data: SliceMut<'_, u8>,
        timeout_ns: u64,
        len: Out<'_, usize>,
    ) -> Status {
        let function = held!(function, UsbFunction);
        let data = data.into_slice();
        let room = data.len();
        answer(
            lock(&function).read(endpoint, data, timeout_in(timeout_ns)),
            len,
            |got| got.min(room),
        )
    }

    fn write(
        function: Handle,
        endpoint: u8,
        data: Slice<'_, u8>,
        timeout_ns: u64,
        len: Out<'_, usize>,
    ) -> Status {
        let function = held!(function, UsbFunction);
        let data = data.as_slice();
        answer(
            lock(&function).write(endpoint, data, timeout_in(timeout_ns)),
            len,
            |sent| sent.min(data.len()),
        )
    }
}

/// The one table every package's driver calls the host through.
static HOST: HostTable = host_table::<Asked<'static, 'static>, Held>();

/// A panic inside the package came back as a status. It goes on as a panic here, so the host's
/// own panic boundary sees what it sees from a built-in driver and does the same about it.
fn panicked() -> ! {
    resume_unwind(Box::new("a package's driver panicked"))
}

/// A package's bound driver, run by the host as any [`Instance`] is.
struct Running {
    // Dropped first: the package drops its driver, which releases what it holds.
    loaded: Loaded,
    _taken: Taken,
}

impl Instance for Running {
    fn describe(&self, out: &mut Description) {
        let mut items = [PackageItem::EMPTY; MAX_ITEMS];
        let count = match self.loaded.describe(&mut items) {
            Ok(count) => count as usize,
            Err(Status::PANIC) => panicked(),
            Err(_) => return,
        };
        let mut last = None;
        for item in items.iter().take(count).filter_map(item_in) {
            out.add(item);
            last = Some(item);
        }
        // Items the package had no room for count as left out here too.
        if let Some(item) = last {
            for _ in MAX_ITEMS..count.min(2 * MAX_ITEMS) {
                out.add(item);
            }
        }
    }

    fn health(&self) -> Health {
        match self.loaded.health() {
            Ok(health) => health_in(health),
            Err(Status::PANIC) => panicked(),
            Err(_) => Health::Failed("the package did not answer"),
        }
    }

    fn dispatch(&mut self, method: MethodId, args: &[u8], reply: &mut Reply<'_>) -> Status {
        let room = reply.remaining();
        let Ok(buffer) = reply.out(room) else {
            return Status::BUFFER_TOO_SMALL;
        };
        let (status, used) = self
            .loaded
            .dispatch(method.contract, method.method, args, buffer);
        if status == Status::PANIC {
            panicked();
        }
        match reply.advance(used) {
            Ok(()) => status,
            Err(status) => status,
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        match self.loaded.tick(now_ns) {
            Ok(next) => next,
            Err(Status::PANIC) => panicked(),
            Err(_) => None,
        }
    }

    fn caller(&mut self, caller: CallerId) {
        if self.loaded.caller(caller.0) == Status::PANIC {
            panicked();
        }
    }

    fn caller_gone(&mut self, caller: CallerId) {
        if self.loaded.caller_gone(caller.0) == Status::PANIC {
            panicked();
        }
    }
}

/// Has a package build its driver, on every bind and every retry.
struct PackageBinder {
    table: &'static DriverTable,
    manifest: &'static Manifest,
}

impl Binder for PackageBinder {
    fn bind(&self, res: &mut Resources<'_>, cfg: &Config<'_>) -> Result<Box<dyn Instance>> {
        let settings = settings_out(cfg);
        let link = res.link().map(str::to_owned);
        let mut asked = Asked {
            resources: res,
            manifest: self.manifest,
            taken: Taken::default(),
        };
        // A panic in the package's bind is `Status::PANIC` here, which is what the host records
        // for a built-in driver that panics while it binds.
        let loaded = Loaded::bind(self.table, &HOST, &mut asked, &settings, link.as_deref())?;
        Ok(Box::new(Running {
            loaded,
            _taken: asked.taken,
        }))
    }
}

/// The entry the host starts a package's driver from. Kept for good, as the package is.
pub(crate) fn entry(driver: &'static PackageDriver, manifest: &'static Manifest) -> DriverEntry {
    let binder: &'static PackageBinder = Box::leak(Box::new(PackageBinder {
        table: &driver.table,
        manifest,
    }));
    DriverEntry::bound_by(
        *manifest,
        driver.table.flags & DRIVER_TICKS != 0,
        driver.table.flags & DRIVER_TRACKS_CALLERS != 0,
        binder,
    )
}

#[cfg(test)]
mod tests {
    use lemnos_abi::package::{Str, TRANSFER_READ};
    use lemnos_sim::SimBoard;

    use super::*;

    static MANIFEST: Manifest = Manifest {
        name: "test",
        needs: &[],
        provides: &[],
    };

    /// Takes `kind` under `need` from `sim`, as a binding driver would, and keeps it.
    fn take(sim: &mut SimBoard, kind: u32, need: &str) -> Result<Handle> {
        let mut resources = Resources::new(sim, None);
        let mut asked = Asked {
            resources: &mut resources,
            manifest: &MANIFEST,
            taken: Taken::default(),
        };
        let handle = asked.take(kind, need, "");
        // Kept past the bind, as a bound driver's are.
        core::mem::take(&mut asked.taken.0);
        handle
    }

    #[test]
    fn a_handle_is_good_only_for_its_own_kind_and_only_until_it_is_released() {
        let mut sim = SimBoard::new();
        sim.add_line("line");
        sim.add_i2c("bus");
        let line = take(&mut sim, TAKE_LINE, "line").unwrap();
        let bus = take(&mut sim, TAKE_I2C, "bus").unwrap();

        let mut value = 7;
        assert_eq!((HOST.line.get)(line, Out::new(&mut value)), Status::OK);
        assert_eq!(value, 0);
        // An I2C bus is not a line, a UART or a stream, whatever the driver says.
        assert_eq!(
            (HOST.line.get)(bus, Out::new(&mut value)),
            Status::INVALID_ARGUMENT
        );
        assert_eq!((HOST.uart.set_baud)(bus, 9600), Status::INVALID_ARGUMENT);
        let mut fd = Fd::NONE;
        assert_eq!(
            (HOST.stream.buffer_handle)(line, 0, Out::new(&mut fd)),
            Status::INVALID_ARGUMENT
        );
        assert!(fd.is_none());
        // A number that was never a handle, and the null one.
        assert_eq!(
            (HOST.line.set)(Handle::new(4000, 9), 1),
            Status::STALE_HANDLE
        );
        assert_eq!((HOST.line.set)(Handle::NULL, 1), Status::STALE_HANDLE);

        // Released, the handle is dead, and the line is free to be taken again.
        assert!(sim.is_claimed("line"));
        assert_eq!((HOST.release)(line), Status::OK);
        assert!(!sim.is_claimed("line"));
        assert_eq!(
            (HOST.line.get)(line, Out::new(&mut value)),
            Status::STALE_HANDLE
        );
        assert_eq!((HOST.release)(line), Status::STALE_HANDLE);
        assert_eq!((HOST.release)(bus), Status::OK);

        // A kind this host does not know, and a name the board does not have.
        assert_eq!(take(&mut sim, 99, "line"), Err(Status::NOT_SUPPORTED));
        assert_eq!(take(&mut sim, TAKE_PWM, "nothing"), Err(Status::NOT_FOUND));
        // What the board has under a name is what it is: a line is not a PWM channel.
        assert_eq!(
            take(&mut sim, TAKE_PWM, "line"),
            Err(Status::INVALID_ARGUMENT)
        );
        // No binding at all is nobody to ask.
        assert_eq!((HOST.provided)(core::ptr::null_mut(), Str::EMPTY), 0);
    }

    #[test]
    fn counts_and_kinds_are_checked_before_a_resource_is_touched() {
        let mut sim = SimBoard::new();
        let i2c = sim.add_i2c("bus");
        let spi_probe = sim.add_spi("spi");
        let raw_probe = sim.add_raw_spi("raw");
        let line_probe = sim.add_line("line");
        let pwm_probe = sim.add_pwm("pwm");
        let usb_probe = sim.add_usb_device("usb", &[9, 2]);
        let bus = take(&mut sim, TAKE_I2C, "bus").unwrap();
        let spi = take(&mut sim, TAKE_SPI, "spi").unwrap();
        let raw = take(&mut sim, TAKE_RAW_SPI, "raw").unwrap();
        let line = take(&mut sim, TAKE_LINE, "line").unwrap();
        let pwm = take(&mut sim, TAKE_PWM, "pwm").unwrap();
        let usb = take(&mut sim, TAKE_USB_DEVICE, "usb").unwrap();

        // One step too many.
        let mut steps: [PackageTransfer<'_>; MAX_TRANSFER_STEPS + 1] =
            core::array::from_fn(|_| PackageTransfer::write(&[1]));
        let before = i2c.transactions();
        assert_eq!(
            (HOST.i2c.transaction)(bus, 0x20, SliceMut::new(&mut steps)),
            Status::INVALID_ARGUMENT
        );
        assert_eq!(
            (HOST.spi.transaction)(spi, SliceMut::new(&mut steps)),
            Status::INVALID_ARGUMENT
        );
        // An address that is not one, and a step I2C does not have.
        let mut one = [PackageTransfer::write(&[1])];
        assert_eq!(
            (HOST.i2c.transaction)(bus, 0x100, SliceMut::new(&mut one)),
            Status::INVALID_ARGUMENT
        );
        let mut delay = [PackageTransfer::delay(5)];
        assert_eq!(
            (HOST.i2c.transaction)(bus, 0x20, SliceMut::new(&mut delay)),
            Status::INVALID_ARGUMENT
        );
        // A kind of step nobody has.
        let mut unknown = [PackageTransfer::write(&[1])];
        unknown[0].kind = TRANSFER_READ + 40;
        assert_eq!(
            (HOST.spi.transaction)(spi, SliceMut::new(&mut unknown)),
            Status::INVALID_ARGUMENT
        );
        assert_eq!(i2c.transactions(), before);
        assert!(spi_probe.written().is_empty());

        let mut segments: [PackageSpiSegment<'_>; MAX_TRANSFER_STEPS + 1] =
            core::array::from_fn(|_| PackageSpiSegment {
                mode: 0,
                bits_per_word: 0,
                cs_change: 0,
                reserved: 0,
                speed_hz: 0,
                delay_us: 0,
                reserved2: [0; 3],
                tx: Slice::EMPTY,
                rx: SliceMut::empty(),
            });
        assert_eq!(
            (HOST.spi.transfer)(raw, SliceMut::new(&mut segments)),
            Status::INVALID_ARGUMENT
        );
        assert!(raw_probe.transactions().is_empty());
        // An SPI device taken one way is not used the other.
        assert_eq!(
            (HOST.spi.transfer)(spi, SliceMut::new(&mut segments[..1])),
            Status::INVALID_ARGUMENT
        );

        // Values of a configuration that this host does not know.
        let config = PackageLineConfig {
            edge: 9,
            ..PackageLineConfig::default()
        };
        assert_eq!(
            (HOST.line.configure)(line, config),
            Status::INVALID_ARGUMENT
        );
        assert!(line_probe.configs().is_empty());
        let config = PackagePwmConfig {
            polarity: 2,
            ..PackagePwmConfig::default()
        };
        assert_eq!((HOST.pwm.configure)(pwm, config), Status::INVALID_ARGUMENT);
        assert!(pwm_probe.history().is_empty());

        // Isochronous: a stream too wide for the boundary, a transfer with no stream, packets
        // that don't fit in the data, or too many.
        let mut data = [0u8; 12];
        let mut packets = [PackageIsoPacket::default(); 2];
        assert_eq!(
            (HOST.usb_device.iso_in_start)(usb, 0x81, 2, MAX_ISO_PACKETS as u16 + 1, 8),
            Status::INVALID_ARGUMENT
        );
        assert_eq!(usb_probe.iso_stream(), None);
        assert_eq!(
            (HOST.usb_device.iso_in_next)(
                usb,
                SliceMut::new(&mut data),
                SliceMut::new(&mut packets),
                0
            ),
            Status::INVALID_ARGUMENT
        );
        assert_eq!(
            (HOST.usb_device.iso_in_start)(usb, 0x81, 2, 2, 8),
            Status::OK
        );
        assert_eq!(usb_probe.iso_stream(), Some((0x81, 2, 2, 8)));
        usb_probe.queue_iso(&[1; 8]);
        assert_eq!(
            (HOST.usb_device.iso_in_next)(
                usb,
                SliceMut::new(&mut data),
                SliceMut::new(&mut packets),
                0
            ),
            Status::INVALID_ARGUMENT
        );
        assert_eq!(data, [0; 12]);
        let mut many = [PackageIsoPacket::default(); MAX_ISO_PACKETS + 1];
        assert_eq!(
            (HOST.usb_device.iso_in_next)(
                usb,
                SliceMut::new(&mut data),
                SliceMut::new(&mut many),
                0
            ),
            Status::INVALID_ARGUMENT
        );
        // One that fits is carried, and what it says it received is within its capacity.
        let mut data = [0u8; 16];
        assert_eq!(
            (HOST.usb_device.iso_in_next)(
                usb,
                SliceMut::new(&mut data),
                SliceMut::new(&mut packets),
                0
            ),
            Status::OK
        );
        assert_eq!(
            (packets[0].capacity, packets[0].received, packets[0].ok),
            (8, 8, 1)
        );
        assert_eq!((HOST.usb_device.iso_in_stop)(usb), Status::OK);
        assert_eq!(usb_probe.iso_stream(), None);

        for handle in [bus, spi, raw, line, pwm, usb] {
            assert_eq!((HOST.release)(handle), Status::OK);
        }
    }
}
