//! A board whose resources do their work without allocating, so a test can count what the
//! boundary itself allocates. The simulated board's mocks keep a log of every call, which a
//! count would see. I2C buses are the simulated board's own, which keep none.

use std::fs::File;
use std::os::fd::AsFd;
use std::time::Duration;

use lemnos_core::{
    ClocksHandle, DmaHandle, ErrorKind, I2c, InterruptHandle, Line, LocalCaller, Provider, Pwm,
    RawSpiBus, RegistersHandle, ResetHandle, Result, Spi, Status, StreamHandle, Tag, Uart,
    UsbDeviceHandle, UsbFunctionHandle,
};
use lemnos_hal::blocks::{
    BufferHandle, BufferKind, ControlSetup, DmaBuffer, InterruptEvent, IsoPacket, StreamFrame,
    UsbFunctionEvent,
};
use lemnos_hal::raw::{Edge, LineConfig, PwmConfig, RawLine, RawPwm, RawSpi, SpiSegment};
use lemnos_hal::spi::{self, Operation, SpiDevice};
use lemnos_hal::{
    Clocks, DmaBuffers, Interrupt, Registers, Reset, StreamEndpoint, Uart as HalUart, UsbDevice,
    UsbFunction,
};
use lemnos_sim::SimBoard;

type Hal<T> = core::result::Result<T, ErrorKind>;

/// One resource of any kind. Reads come back as `0x5a`; registers, DMA memory and the stream's
/// buffer are small arrays; nothing is ever logged.
pub struct Quiet {
    registers: [u32; 16],
    memory: [u8; 64],
    pwm: PwmConfig,
    /// The memory behind the stream's one buffer.
    file: File,
    held: bool,
    events: u32,
}

impl Quiet {
    fn new(file: File) -> Self {
        Self {
            registers: [0; 16],
            memory: [0; 64],
            pwm: PwmConfig::default(),
            file,
            held: false,
            events: 0,
        }
    }

    fn range(&mut self, offset: u64, len: usize) -> Hal<&mut [u8]> {
        let start = usize::try_from(offset).map_err(|_| ErrorKind::InvalidInput)?;
        let end = start.checked_add(len).ok_or(ErrorKind::InvalidInput)?;
        self.memory
            .get_mut(start..end)
            .ok_or(ErrorKind::InvalidInput)
    }

    fn register(&mut self, offset: usize) -> Hal<&mut u32> {
        self.registers
            .get_mut(offset / 4)
            .ok_or(ErrorKind::InvalidInput)
    }
}

fn fill(out: &mut [u8]) -> usize {
    out.fill(0x5a);
    out.len()
}

impl spi::ErrorType for Quiet {
    type Error = ErrorKind;
}

impl SpiDevice for Quiet {
    fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Hal<()> {
        for operation in operations {
            match operation {
                Operation::Read(out) | Operation::Transfer(out, _) => {
                    fill(out);
                }
                Operation::TransferInPlace(data) => {
                    fill(data);
                }
                Operation::Write(_) | Operation::DelayNs(_) => {}
            }
        }
        Ok(())
    }
}

impl RawSpi for Quiet {
    fn transfer(&mut self, segments: &mut [SpiSegment<'_>]) -> Hal<()> {
        for segment in segments {
            fill(segment.rx);
        }
        Ok(())
    }
}

impl RawLine for Quiet {
    fn configure(&mut self, _config: &LineConfig) -> Hal<()> {
        Ok(())
    }

    fn get(&mut self) -> Hal<bool> {
        Ok(true)
    }

    fn set(&mut self, _value: bool) -> Hal<()> {
        Ok(())
    }

    fn read_edge(&mut self) -> Hal<Option<Edge>> {
        self.events += 1;
        Ok(Some(Edge {
            rising: self.events.is_multiple_of(2),
            timestamp_ns: u64::from(self.events),
            seq: self.events,
        }))
    }
}

impl RawPwm for Quiet {
    fn configure(&mut self, config: &PwmConfig) -> Hal<()> {
        self.pwm = *config;
        Ok(())
    }

    fn config(&mut self) -> Hal<PwmConfig> {
        Ok(self.pwm)
    }
}

impl HalUart for Quiet {
    type Error = ErrorKind;

    fn set_baud(&mut self, _baud: u32) -> Hal<()> {
        Ok(())
    }

    fn write(&mut self, data: &[u8]) -> Hal<usize> {
        Ok(data.len())
    }

    fn read(&mut self, out: &mut [u8], _timeout: Duration) -> Hal<usize> {
        Ok(fill(&mut out[..4]))
    }
}

impl Registers for Quiet {
    type Error = ErrorKind;

    fn size(&self) -> usize {
        64
    }

    fn read32(&mut self, offset: usize) -> Hal<u32> {
        self.register(offset).map(|value| *value)
    }

    fn write32(&mut self, offset: usize, value: u32) -> Hal<()> {
        *self.register(offset)? = value;
        Ok(())
    }
}

impl Interrupt for Quiet {
    type Error = ErrorKind;

    fn enable(&mut self) -> Hal<()> {
        Ok(())
    }

    fn disable(&mut self) -> Hal<()> {
        Ok(())
    }

    fn wait(&mut self, _timeout: Option<Duration>) -> Hal<Option<InterruptEvent>> {
        Ok(Some(InterruptEvent {
            count: 1,
            timestamp_ns: 5,
        }))
    }
}

impl DmaBuffers for Quiet {
    type Error = ErrorKind;

    fn allocate(&mut self, len: u64) -> Hal<DmaBuffer> {
        Ok(DmaBuffer {
            id: 1,
            len,
            device_address: 0x1000,
        })
    }

    fn release(&mut self, _id: u32) -> Hal<()> {
        Ok(())
    }

    fn write(&mut self, _id: u32, offset: u64, data: &[u8]) -> Hal<()> {
        self.range(offset, data.len())?.copy_from_slice(data);
        Ok(())
    }

    fn read(&mut self, _id: u32, offset: u64, out: &mut [u8]) -> Hal<()> {
        out.copy_from_slice(self.range(offset, out.len())?);
        Ok(())
    }

    fn sync_for_device(&mut self, _id: u32) -> Hal<()> {
        Ok(())
    }

    fn sync_for_cpu(&mut self, _id: u32) -> Hal<()> {
        Ok(())
    }
}

impl Clocks for Quiet {
    type Error = ErrorKind;

    fn enable(&mut self, _clock: u32) -> Hal<()> {
        Ok(())
    }

    fn disable(&mut self, _clock: u32) -> Hal<()> {
        Ok(())
    }

    fn rate_hz(&mut self, _clock: u32) -> Hal<u64> {
        Ok(24_000_000)
    }

    fn set_rate_hz(&mut self, _clock: u32, hz: u64) -> Hal<u64> {
        Ok(hz)
    }
}

impl Reset for Quiet {
    type Error = ErrorKind;

    fn assert(&mut self, _line: u32) -> Hal<()> {
        Ok(())
    }

    fn deassert(&mut self, _line: u32) -> Hal<()> {
        Ok(())
    }
}

impl StreamEndpoint for Quiet {
    type Error = ErrorKind;

    fn buffer_count(&self) -> u32 {
        1
    }

    fn publish(&mut self, _frame: StreamFrame) -> Hal<()> {
        self.held = false;
        Ok(())
    }

    fn next_buffer(&mut self) -> Hal<Option<u32>> {
        Ok((!core::mem::replace(&mut self.held, true)).then_some(0))
    }

    fn next_job(&mut self, out: &mut [u8]) -> Hal<Option<usize>> {
        Ok(Some(fill(&mut out[..4])))
    }

    fn write(&mut self, _buffer: u32, offset: usize, data: &[u8]) -> Hal<bool> {
        self.range(offset as u64, data.len())?.copy_from_slice(data);
        Ok(true)
    }

    fn read(&mut self, _buffer: u32, offset: usize, out: &mut [u8]) -> Hal<bool> {
        out.copy_from_slice(self.range(offset as u64, out.len())?);
        Ok(true)
    }

    fn buffer_handle(&self, _buffer: u32) -> Hal<Option<BufferHandle<'_>>> {
        Ok(self.held.then(|| BufferHandle::from(self.file.as_fd())))
    }

    fn buffer_current(&self, _buffer: u32) -> Hal<bool> {
        Ok(self.held)
    }

    fn buffer_kind(&self) -> BufferKind {
        BufferKind::Handles
    }

    fn fault_consumer(&mut self) -> Hal<()> {
        Ok(())
    }
}

impl UsbDevice for Quiet {
    type Error = ErrorKind;

    fn config_descriptor(&mut self, _index: u8, out: &mut [u8]) -> Hal<usize> {
        Ok(fill(&mut out[..9]))
    }

    fn reset(&mut self) -> Hal<()> {
        Ok(())
    }

    fn detach_kernel_driver(&mut self, _interface: u8) -> Hal<()> {
        Ok(())
    }

    fn claim_interface(&mut self, _interface: u8) -> Hal<()> {
        Ok(())
    }

    fn release_interface(&mut self, _interface: u8) -> Hal<()> {
        Ok(())
    }

    fn set_alternate(&mut self, _interface: u8, _alternate: u8) -> Hal<()> {
        Ok(())
    }

    fn control_in(&mut self, _: ControlSetup, data: &mut [u8], _: Duration) -> Hal<usize> {
        Ok(fill(&mut data[..2]))
    }

    fn control_out(&mut self, _: ControlSetup, data: &[u8], _: Duration) -> Hal<usize> {
        Ok(data.len())
    }

    fn bulk_in(&mut self, _endpoint: u8, data: &mut [u8], _: Duration) -> Hal<usize> {
        Ok(fill(&mut data[..8]))
    }

    fn bulk_out(&mut self, _endpoint: u8, data: &[u8], _: Duration) -> Hal<usize> {
        Ok(data.len())
    }

    fn interrupt_in(&mut self, _endpoint: u8, data: &mut [u8], _: Duration) -> Hal<usize> {
        Ok(fill(&mut data[..1]))
    }

    fn iso_in_start(&mut self, _: u8, _: u8, _: u16, _: u32) -> Hal<()> {
        Ok(())
    }

    /// Every packet full, at the room `data` shares out to it.
    fn iso_in_next(&mut self, data: &mut [u8], packets: &mut [IsoPacket], _: Duration) -> Hal<()> {
        fill(data);
        let room = (data.len() / packets.len().max(1)) as u32;
        for packet in packets {
            packet.capacity = room;
            packet.received = room;
            packet.ok = true;
        }
        Ok(())
    }

    fn iso_in_stop(&mut self) -> Hal<()> {
        Ok(())
    }
}

impl UsbFunction for Quiet {
    type Error = ErrorKind;

    fn enable(&mut self, _descriptors: &[u8]) -> Hal<()> {
        Ok(())
    }

    fn disable(&mut self) -> Hal<()> {
        Ok(())
    }

    fn next_event(&mut self) -> Hal<Option<UsbFunctionEvent>> {
        Ok(Some(UsbFunctionEvent::Setup {
            setup: ControlSetup {
                request_type: 0xc0,
                request: 1,
                value: 2,
                index: 3,
            },
            length: 4,
        }))
    }

    fn control_in(&mut self, data: &[u8], _: Duration) -> Hal<usize> {
        Ok(data.len())
    }

    fn control_out(&mut self, data: &mut [u8], _: Duration) -> Hal<usize> {
        Ok(fill(data))
    }

    fn control_stall(&mut self) -> Hal<()> {
        Ok(())
    }

    fn set_halt(&mut self, _endpoint: u8, _halted: bool) -> Hal<()> {
        Ok(())
    }

    fn read(&mut self, _endpoint: u8, data: &mut [u8], _: Duration) -> Hal<usize> {
        Ok(fill(&mut data[..3]))
    }

    fn write(&mut self, _endpoint: u8, data: &[u8], _: Duration) -> Hal<usize> {
        Ok(data.len())
    }
}

/// A board that has every kind of resource under any name that starts with `q-`. Its I2C
/// buses are `sim`'s.
pub struct QuietBoard {
    pub sim: SimBoard,
    /// The file behind every stream's buffer.
    pub file: File,
}

impl QuietBoard {
    fn quiet(&self) -> Result<Box<Quiet>> {
        let file = self.file.try_clone().map_err(|_| Status::IO)?;
        Ok(Box::new(Quiet::new(file)))
    }
}

macro_rules! quiet {
    ($($method:ident -> $handle:ty;)*) => {$(
        fn $method(&mut self, _need: &str) -> Result<$handle> {
            Ok(lemnos_core::Held::boxed(self.quiet()?))
        }
    )*};
}

impl Provider for QuietBoard {
    fn provided(&self, need: &str) -> bool {
        need.starts_with("q-") || self.sim.provided(need)
    }

    fn device(&mut self, _need: &str, _tag: Tag) -> Result<LocalCaller> {
        Err(Status::NOT_SUPPORTED)
    }

    fn i2c(&mut self, need: &str) -> Result<I2c> {
        self.sim.i2c(need)
    }

    fn spi(&mut self, _need: &str) -> Result<Spi> {
        Ok(Spi(lemnos_core::Held::boxed(self.quiet()?)))
    }

    quiet! {
        raw_spi -> RawSpiBus;
        line -> Line;
        pwm -> Pwm;
        uart -> Uart;
        usb_device -> UsbDeviceHandle;
        usb_function -> UsbFunctionHandle;
        registers -> RegistersHandle;
        interrupt -> InterruptHandle;
        dma -> DmaHandle;
        clocks -> ClocksHandle;
        reset -> ResetHandle;
        stream -> StreamHandle;
    }
}
