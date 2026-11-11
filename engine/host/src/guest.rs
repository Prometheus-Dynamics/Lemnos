//! What a host-owned device's driver is given in place of its needs: the same resources, with
//! everything that would change the hardware refused.
//!
//! A board file marks a device `owner = "host"` when the operating system's own driver owns it.
//! Refusing the methods that change it is not enough: a driver also acts from `start` and from
//! `tick`, and a consumer reaches it through a stream's jobs. So the refusal is here, between
//! the driver and the bus, where no path through the driver gets round it. Each wrapper passes
//! what only reads and answers `PermissionDenied` to the rest, without touching the bus.
//!
//! What counts as a change, bus by bus:
//!
//! - I2C: any write. A read of a register that first writes the register's number is a write,
//!   so only chips that answer a plain read can be read; the rest are the operating system's to
//!   read (its own files say what they hold).
//! - SPI: anything that sends bytes. Only a pure read and a delay pass.
//! - A line: configuring it and setting it. Its level and its edges are read.
//! - PWM: configuring it. Its configuration is read.
//! - A serial port: sending, and changing its speed.
//! - Registers: any write. Interrupts: masking and unmasking; waiting for one passes.
//! - Clocks and resets: everything but reading a rate.
//! - DMA memory: allocating, releasing, writing, and handing it to the device.
//! - USB, as a host: reset, detaching the kernel's driver, claiming, releasing, alternates, and
//!   every transfer out. USB, as a function: everything but reading its events.
//! - A stream: the consumer's jobs never reach the driver (a job is a caller asking for a
//!   change, as a method is), and no buffer is lent as a handle, so hardware is never handed
//!   one; the stream says its buffers are plain memory. Frames the driver publishes pass.
//! - Another device (a `uses` entry): only the methods that device's description names as
//!   values, as for any caller of a host-owned device.

use core::time::Duration;

use lemnos_core::{
    ClocksHandle, DmaHandle, I2c, InterruptHandle, Line, Pwm, RawSpiBus, RegistersHandle,
    ResetHandle, Spi, StreamHandle, Uart, UsbDeviceHandle, UsbFunctionHandle,
};
use lemnos_hal::blocks::{
    BufferHandle, BufferKind, ControlSetup, DmaBuffer, InterruptEvent, IsoPacket, StreamFrame,
    UsbFunctionEvent,
};
use lemnos_hal::i2c::{self, I2c as HalI2c};
use lemnos_hal::raw::{Edge, LineConfig, PwmConfig, RawLine, RawPwm, RawSpi, SpiSegment};
use lemnos_hal::spi::{self, SpiDevice};
use lemnos_hal::{
    Clocks, DmaBuffers, ErrorKind, Interrupt, Registers, Reset, StreamEndpoint, Uart as UartTrait,
    UsbDevice, UsbFunction,
};

/// What every refused change answers.
const REFUSED: ErrorKind = ErrorKind::PermissionDenied;

type Refusable<T> = Result<T, ErrorKind>;

/// A resource of a host-owned device: `T` with its changes refused.
pub(crate) struct ReadOnly<T>(T);

pub(crate) fn i2c(bus: I2c) -> I2c {
    I2c(lemnos_core::Held::boxed(Box::new(ReadOnly(bus))))
}

pub(crate) fn spi(bus: Spi) -> Spi {
    Spi(lemnos_core::Held::boxed(Box::new(ReadOnly(bus))))
}

pub(crate) fn raw_spi(bus: RawSpiBus) -> RawSpiBus {
    lemnos_core::Held::boxed(Box::new(ReadOnly(bus)))
}

pub(crate) fn line(line: Line) -> Line {
    lemnos_core::Held::boxed(Box::new(ReadOnly(line)))
}

pub(crate) fn pwm(pwm: Pwm) -> Pwm {
    lemnos_core::Held::boxed(Box::new(ReadOnly(pwm)))
}

pub(crate) fn uart(uart: Uart) -> Uart {
    lemnos_core::Held::boxed(Box::new(ReadOnly(uart)))
}

pub(crate) fn usb_device(device: UsbDeviceHandle) -> UsbDeviceHandle {
    lemnos_core::Held::boxed(Box::new(ReadOnly(device)))
}

pub(crate) fn usb_function(function: UsbFunctionHandle) -> UsbFunctionHandle {
    lemnos_core::Held::boxed(Box::new(ReadOnly(function)))
}

pub(crate) fn registers(registers: RegistersHandle) -> RegistersHandle {
    lemnos_core::Held::boxed(Box::new(ReadOnly(registers)))
}

pub(crate) fn interrupt(interrupt: InterruptHandle) -> InterruptHandle {
    lemnos_core::Held::boxed(Box::new(ReadOnly(interrupt)))
}

pub(crate) fn dma(dma: DmaHandle) -> DmaHandle {
    lemnos_core::Held::boxed(Box::new(ReadOnly(dma)))
}

pub(crate) fn clocks(clocks: ClocksHandle) -> ClocksHandle {
    lemnos_core::Held::boxed(Box::new(ReadOnly(clocks)))
}

pub(crate) fn reset(reset: ResetHandle) -> ResetHandle {
    lemnos_core::Held::boxed(Box::new(ReadOnly(reset)))
}

pub(crate) fn stream(stream: StreamHandle) -> StreamHandle {
    lemnos_core::Held::boxed(Box::new(ReadOnly(stream)))
}

impl i2c::ErrorType for ReadOnly<I2c> {
    type Error = ErrorKind;
}

impl HalI2c for ReadOnly<I2c> {
    fn transaction(&mut self, address: u8, operations: &mut [i2c::Operation<'_>]) -> Refusable<()> {
        if operations
            .iter()
            .any(|operation| matches!(operation, i2c::Operation::Write(_)))
        {
            return Err(REFUSED);
        }
        self.0.transaction(address, operations)
    }
}

impl spi::ErrorType for ReadOnly<Spi> {
    type Error = ErrorKind;
}

impl SpiDevice for ReadOnly<Spi> {
    fn transaction(&mut self, operations: &mut [spi::Operation<'_, u8>]) -> Refusable<()> {
        let only_reads = operations.iter().all(|operation| {
            matches!(
                operation,
                spi::Operation::Read(_) | spi::Operation::DelayNs(_)
            )
        });
        if !only_reads {
            return Err(REFUSED);
        }
        self.0.transaction(operations)
    }
}

impl RawSpi for ReadOnly<RawSpiBus> {
    fn transfer(&mut self, segments: &mut [SpiSegment<'_>]) -> Refusable<()> {
        if segments.iter().any(|segment| !segment.tx.is_empty()) {
            return Err(REFUSED);
        }
        self.0.transfer(segments)
    }
}

impl RawLine for ReadOnly<Line> {
    fn configure(&mut self, _: &LineConfig) -> Refusable<()> {
        Err(REFUSED)
    }

    fn get(&mut self) -> Refusable<bool> {
        self.0.get()
    }

    fn set(&mut self, _: bool) -> Refusable<()> {
        Err(REFUSED)
    }

    fn read_edge(&mut self) -> Refusable<Option<Edge>> {
        self.0.read_edge()
    }
}

impl RawPwm for ReadOnly<Pwm> {
    fn configure(&mut self, _: &PwmConfig) -> Refusable<()> {
        Err(REFUSED)
    }

    fn config(&mut self) -> Refusable<PwmConfig> {
        self.0.config()
    }
}

impl UartTrait for ReadOnly<Uart> {
    type Error = ErrorKind;

    fn set_baud(&mut self, _: u32) -> Refusable<()> {
        Err(REFUSED)
    }

    fn write(&mut self, _: &[u8]) -> Refusable<usize> {
        Err(REFUSED)
    }

    fn read(&mut self, out: &mut [u8], timeout: Duration) -> Refusable<usize> {
        self.0.read(out, timeout)
    }
}

impl Registers for ReadOnly<RegistersHandle> {
    type Error = ErrorKind;

    fn size(&self) -> usize {
        self.0.size()
    }

    fn read32(&mut self, offset: usize) -> Refusable<u32> {
        self.0.read32(offset)
    }

    // `modify32` and `write_block` are made of this, so they are refused with it.
    fn write32(&mut self, _: usize, _: u32) -> Refusable<()> {
        Err(REFUSED)
    }
}

impl Interrupt for ReadOnly<InterruptHandle> {
    type Error = ErrorKind;

    fn enable(&mut self) -> Refusable<()> {
        Err(REFUSED)
    }

    fn disable(&mut self) -> Refusable<()> {
        Err(REFUSED)
    }

    fn wait(&mut self, timeout: Option<Duration>) -> Refusable<Option<InterruptEvent>> {
        self.0.wait(timeout)
    }
}

impl DmaBuffers for ReadOnly<DmaHandle> {
    type Error = ErrorKind;

    fn allocate(&mut self, _: u64) -> Refusable<DmaBuffer> {
        Err(REFUSED)
    }

    fn release(&mut self, _: u32) -> Refusable<()> {
        Err(REFUSED)
    }

    fn write(&mut self, _: u32, _: u64, _: &[u8]) -> Refusable<()> {
        Err(REFUSED)
    }

    fn read(&mut self, id: u32, offset: u64, out: &mut [u8]) -> Refusable<()> {
        self.0.read(id, offset, out)
    }

    fn sync_for_device(&mut self, _: u32) -> Refusable<()> {
        Err(REFUSED)
    }

    fn sync_for_cpu(&mut self, id: u32) -> Refusable<()> {
        self.0.sync_for_cpu(id)
    }
}

impl Clocks for ReadOnly<ClocksHandle> {
    type Error = ErrorKind;

    fn enable(&mut self, _: u32) -> Refusable<()> {
        Err(REFUSED)
    }

    fn disable(&mut self, _: u32) -> Refusable<()> {
        Err(REFUSED)
    }

    fn rate_hz(&mut self, clock: u32) -> Refusable<u64> {
        self.0.rate_hz(clock)
    }

    fn set_rate_hz(&mut self, _: u32, _: u64) -> Refusable<u64> {
        Err(REFUSED)
    }
}

impl Reset for ReadOnly<ResetHandle> {
    type Error = ErrorKind;

    fn assert(&mut self, _: u32) -> Refusable<()> {
        Err(REFUSED)
    }

    fn deassert(&mut self, _: u32) -> Refusable<()> {
        Err(REFUSED)
    }
}

impl UsbDevice for ReadOnly<UsbDeviceHandle> {
    type Error = ErrorKind;

    fn config_descriptor(&mut self, index: u8, out: &mut [u8]) -> Refusable<usize> {
        self.0.config_descriptor(index, out)
    }

    fn reset(&mut self) -> Refusable<()> {
        Err(REFUSED)
    }

    fn detach_kernel_driver(&mut self, _: u8) -> Refusable<()> {
        Err(REFUSED)
    }

    fn claim_interface(&mut self, _: u8) -> Refusable<()> {
        Err(REFUSED)
    }

    fn release_interface(&mut self, _: u8) -> Refusable<()> {
        Err(REFUSED)
    }

    fn set_alternate(&mut self, _: u8, _: u8) -> Refusable<()> {
        Err(REFUSED)
    }

    fn control_in(
        &mut self,
        setup: ControlSetup,
        data: &mut [u8],
        timeout: Duration,
    ) -> Refusable<usize> {
        // A setup that says it reads and is sent as a write would be one.
        if !setup.is_in() {
            return Err(REFUSED);
        }
        self.0.control_in(setup, data, timeout)
    }

    fn control_out(&mut self, _: ControlSetup, _: &[u8], _: Duration) -> Refusable<usize> {
        Err(REFUSED)
    }

    fn bulk_in(&mut self, endpoint: u8, data: &mut [u8], timeout: Duration) -> Refusable<usize> {
        self.0.bulk_in(endpoint, data, timeout)
    }

    fn bulk_out(&mut self, _: u8, _: &[u8], _: Duration) -> Refusable<usize> {
        Err(REFUSED)
    }

    fn interrupt_in(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        timeout: Duration,
    ) -> Refusable<usize> {
        self.0.interrupt_in(endpoint, data, timeout)
    }

    fn iso_in(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        packets: &mut [IsoPacket],
        timeout: Duration,
    ) -> Refusable<()> {
        self.0.iso_in(endpoint, data, packets, timeout)
    }
}

impl UsbFunction for ReadOnly<UsbFunctionHandle> {
    type Error = ErrorKind;

    fn enable(&mut self, _: &[u8]) -> Refusable<()> {
        Err(REFUSED)
    }

    fn disable(&mut self) -> Refusable<()> {
        Err(REFUSED)
    }

    fn next_event(&mut self) -> Refusable<Option<UsbFunctionEvent>> {
        self.0.next_event()
    }

    fn control_in(&mut self, _: &[u8], _: Duration) -> Refusable<usize> {
        Err(REFUSED)
    }

    fn control_out(&mut self, _: &mut [u8], _: Duration) -> Refusable<usize> {
        Err(REFUSED)
    }

    fn control_stall(&mut self) -> Refusable<()> {
        Err(REFUSED)
    }

    fn set_halt(&mut self, _: u8, _: bool) -> Refusable<()> {
        Err(REFUSED)
    }

    fn read(&mut self, _: u8, _: &mut [u8], _: Duration) -> Refusable<usize> {
        Err(REFUSED)
    }

    fn write(&mut self, _: u8, _: &[u8], _: Duration) -> Refusable<usize> {
        Err(REFUSED)
    }
}

impl StreamEndpoint for ReadOnly<StreamHandle> {
    type Error = ErrorKind;

    fn buffer_count(&self) -> u32 {
        self.0.buffer_count()
    }

    /// Plain, whatever the board file says: no buffer is lent as a handle, so a driver that
    /// would hand buffers to hardware refuses to bind.
    fn buffer_kind(&self) -> BufferKind {
        BufferKind::Plain
    }

    fn publish(&mut self, frame: StreamFrame) -> Refusable<()> {
        self.0.publish(frame)
    }

    fn next_buffer(&mut self) -> Refusable<Option<u32>> {
        self.0.next_buffer()
    }

    /// Never a job: what a consumer posts stays on the ring, unread.
    fn next_job(&mut self, _: &mut [u8]) -> Refusable<Option<usize>> {
        Ok(None)
    }

    fn write(&mut self, buffer: u32, offset: usize, data: &[u8]) -> Refusable<bool> {
        self.0.write(buffer, offset, data)
    }

    fn read(&mut self, buffer: u32, offset: usize, out: &mut [u8]) -> Refusable<bool> {
        self.0.read(buffer, offset, out)
    }

    fn buffer_handle(&self, _: u32) -> Refusable<Option<BufferHandle<'_>>> {
        Err(ErrorKind::Unsupported)
    }

    fn buffer_current(&self, buffer: u32) -> Refusable<bool> {
        self.0.buffer_current(buffer)
    }

    fn fault_consumer(&mut self) -> Refusable<()> {
        self.0.fault_consumer()
    }
}

#[cfg(test)]
mod tests {
    use lemnos_hal::mock::{
        MockClocks, MockDma, MockI2c, MockInterrupt, MockLine, MockPwm, MockRawSpi, MockRegisters,
        MockReset, MockStream, MockUart, MockUsbDevice, MockUsbFunction,
    };
    use lemnos_hal::raw::SpiConfig;

    use super::*;

    const SOON: Duration = Duration::from_millis(1);

    #[test]
    fn an_i2c_bus_passes_a_plain_read_and_refuses_anything_with_a_write_in_it() {
        let mock = MockI2c::new().with_raw_target(0x20);
        mock.set_logging(true);
        let mut bus = i2c(I2c(lemnos_core::Held::boxed(Box::new(mock.clone()))));
        let mut byte = [0u8; 1];
        assert_eq!(bus.write(0x20, &[1, 2]), Err(REFUSED));
        // Naming a register before reading it is a write.
        assert_eq!(bus.write_read(0x20, &[1], &mut byte), Err(REFUSED));
        assert!(mock.transfers().is_empty(), "nothing went out");
        let _ = bus.read(0x20, &mut byte);
        assert_eq!(mock.transfers().len(), 1, "a plain read does");
    }

    #[test]
    fn a_raw_spi_bus_refuses_every_segment_that_sends() {
        let mock = MockRawSpi::new();
        let mut bus = raw_spi(lemnos_core::Held::boxed(Box::new(mock.clone())));
        let mut rx = [0u8; 2];
        let mut sending = [SpiSegment {
            config: SpiConfig::default(),
            tx: &[1, 2],
            rx: &mut rx,
            cs_change: false,
            delay_us: 0,
        }];
        assert_eq!(bus.transfer(&mut sending), Err(REFUSED));
        assert!(mock.transactions().is_empty());
        let mut reading = [SpiSegment {
            config: SpiConfig::default(),
            tx: &[],
            rx: &mut rx,
            cs_change: false,
            delay_us: 0,
        }];
        assert_eq!(bus.transfer(&mut reading), Ok(()));
        assert_eq!(mock.transactions().len(), 1);
    }

    #[test]
    fn a_line_is_read_and_never_configured_or_set() {
        let mock = MockLine::new();
        mock.drive(true);
        let mut wrapped = line(lemnos_core::Held::boxed(Box::new(mock.clone())));
        assert_eq!(wrapped.configure(&LineConfig::output(true)), Err(REFUSED));
        assert_eq!(wrapped.configure(&LineConfig::input()), Err(REFUSED));
        assert_eq!(wrapped.set(false), Err(REFUSED));
        assert!(mock.configs().is_empty());
        assert_eq!(wrapped.get(), Ok(true));
        assert_eq!(wrapped.read_edge(), Ok(None));
    }

    #[test]
    fn a_pwm_channel_is_read_and_never_configured() {
        let mock = MockPwm::new();
        let mut wrapped = pwm(lemnos_core::Held::boxed(Box::new(mock.clone())));
        let config = PwmConfig {
            period_ns: 1000,
            duty_ns: 500,
            enabled: true,
            ..PwmConfig::default()
        };
        assert_eq!(wrapped.configure(&config), Err(REFUSED));
        assert!(mock.history().is_empty());
        assert_eq!(wrapped.config(), Ok(PwmConfig::default()));
    }

    #[test]
    fn a_serial_port_receives_and_never_sends_or_changes_speed() {
        let mock = MockUart::new();
        let before = mock.baud();
        mock.receive(b"in");
        let mut wrapped = uart(lemnos_core::Held::boxed(Box::new(mock.clone())));
        assert_eq!(wrapped.write(b"out"), Err(REFUSED));
        assert_eq!(wrapped.set_baud(9600), Err(REFUSED));
        assert!(mock.sent().is_empty());
        assert_eq!(mock.baud(), before);
        let mut got = [0u8; 2];
        assert_eq!(wrapped.read(&mut got, SOON), Ok(2));
        assert_eq!(&got, b"in");
    }

    #[test]
    fn registers_are_read_and_no_write_of_any_shape_lands() {
        let mock = MockRegisters::new(64);
        mock.set(8, 0xABCD);
        let mut wrapped = registers(lemnos_core::Held::boxed(Box::new(mock.clone())));
        assert_eq!(wrapped.write32(8, 1), Err(REFUSED));
        assert_eq!(wrapped.modify32(8, 0xF, 0x1), Err(REFUSED));
        assert_eq!(wrapped.write_block(0, &[1, 2, 3]), Err(REFUSED));
        assert!(mock.writes().is_empty());
        assert_eq!(wrapped.read32(8), Ok(0xABCD));
        assert_eq!(wrapped.size(), 64);
    }

    #[test]
    fn an_interrupt_is_waited_for_and_never_masked_or_unmasked() {
        let mock = MockInterrupt::new();
        let mut wrapped = interrupt(lemnos_core::Held::boxed(Box::new(mock.clone())));
        assert_eq!(wrapped.enable(), Err(REFUSED));
        assert!(!mock.enabled());
        assert_eq!(wrapped.disable(), Err(REFUSED));
        assert!(wrapped.wait(Some(SOON)).is_ok());
    }

    #[test]
    fn clocks_resets_and_dma_memory_are_left_as_they_are() {
        let mock = MockClocks::new();
        let mut wrapped = clocks(lemnos_core::Held::boxed(Box::new(mock.clone())));
        assert_eq!(wrapped.enable(1), Err(REFUSED));
        assert_eq!(wrapped.disable(1), Err(REFUSED));
        assert_eq!(wrapped.set_rate_hz(1, 48_000_000), Err(REFUSED));
        assert!(!mock.enabled(1));
        assert!(wrapped.rate_hz(1).is_ok());

        let mock = MockReset::new();
        let mut wrapped = reset(lemnos_core::Held::boxed(Box::new(mock.clone())));
        assert_eq!(wrapped.assert(0), Err(REFUSED));
        assert_eq!(wrapped.deassert(0), Err(REFUSED));
        assert!(mock.history().is_empty());

        let mock = MockDma::new();
        let mut wrapped = dma(lemnos_core::Held::boxed(Box::new(mock.clone())));
        assert_eq!(wrapped.allocate(4096).map(|_| ()), Err(REFUSED));
        assert_eq!(wrapped.write(0, 0, &[1]), Err(REFUSED));
        assert_eq!(wrapped.sync_for_device(0), Err(REFUSED));
        assert_eq!(wrapped.release(0), Err(REFUSED));
        assert_eq!(mock.live_buffers(), 0);
    }

    #[test]
    fn a_usb_device_is_read_and_nothing_is_sent_claimed_or_reset() {
        let mock = MockUsbDevice::new(&[9, 2, 9, 0, 1, 1, 0, 0x80, 50]);
        mock.bind_kernel_driver(0);
        mock.queue_in(&[7, 7]);
        let mut wrapped = usb_device(lemnos_core::Held::boxed(Box::new(mock.clone())));
        let out = ControlSetup {
            request_type: 0x21,
            request: 1,
            value: 0,
            index: 0,
        };
        let input = ControlSetup {
            request_type: 0xA1,
            ..out
        };
        let mut data = [0u8; 16];
        assert_eq!(wrapped.reset(), Err(REFUSED));
        assert_eq!(wrapped.detach_kernel_driver(0), Err(REFUSED));
        assert_eq!(wrapped.claim_interface(0), Err(REFUSED));
        assert_eq!(wrapped.release_interface(0), Err(REFUSED));
        assert_eq!(wrapped.set_alternate(0, 1), Err(REFUSED));
        assert_eq!(wrapped.control_out(out, &[1], SOON), Err(REFUSED));
        assert_eq!(wrapped.bulk_out(1, &[1], SOON), Err(REFUSED));
        // A setup that goes out, sent through the call that reads.
        assert_eq!(wrapped.control_in(out, &mut data, SOON), Err(REFUSED));
        assert_eq!(mock.resets(), 0);
        assert!(mock.detaches().is_empty() && mock.kernel_driver_bound(0));
        assert!(mock.claimed().is_empty());
        assert_eq!(mock.alternate(0), None);
        assert!(mock.written().is_empty() && mock.controls().is_empty());
        // What only reads goes through to the device.
        assert!(wrapped.config_descriptor(0, &mut data).is_ok());
        let _ = wrapped.control_in(input, &mut data, SOON);
        assert_eq!(mock.controls(), [input]);
    }

    #[test]
    fn a_usb_function_is_never_enabled_or_answered() {
        let mock = MockUsbFunction::new();
        let mut wrapped = usb_function(lemnos_core::Held::boxed(Box::new(mock.clone())));
        let mut data = [0u8; 8];
        assert_eq!(wrapped.enable(&[1, 2, 3]), Err(REFUSED));
        assert_eq!(wrapped.disable(), Err(REFUSED));
        assert_eq!(wrapped.control_in(&[1], SOON), Err(REFUSED));
        assert_eq!(wrapped.control_out(&mut data, SOON), Err(REFUSED));
        assert_eq!(wrapped.control_stall(), Err(REFUSED));
        assert_eq!(wrapped.set_halt(0x81, true), Err(REFUSED));
        assert_eq!(wrapped.write(0x81, &[1], SOON), Err(REFUSED));
        assert_eq!(wrapped.read(0x01, &mut data, SOON), Err(REFUSED));
        assert_eq!(mock.enables(), 0);
        assert!(mock.descriptors().is_none());
        assert!(mock.answers().is_empty() && mock.host_received().is_empty());
        // Reading its events is passed on: the answer is the function's own, not a refusal.
        assert_eq!(wrapped.next_event(), Err(ErrorKind::Unavailable));
    }

    #[test]
    fn a_stream_gives_no_job_and_no_handle_and_still_carries_frames() {
        let mock = MockStream::with_bytes(2, 16);
        mock.post_job(b"a change");
        let mut wrapped = stream(lemnos_core::Held::boxed(Box::new(mock.clone())));
        let mut job = [0u8; 16];
        assert_eq!(wrapped.next_job(&mut job), Ok(None));
        assert_eq!(wrapped.buffer_kind(), BufferKind::Plain);
        let buffer = wrapped.next_buffer().unwrap().unwrap();
        assert_eq!(
            wrapped.buffer_handle(buffer).map(|_| ()),
            Err(ErrorKind::Unsupported)
        );
        assert_eq!(wrapped.write(buffer, 0, &[1, 2]), Ok(true));
        let frame = StreamFrame {
            buffer,
            bytes: 2,
            sequence: 1,
            identity: 0,
            timestamp_ns: 0,
        };
        wrapped.publish(frame).unwrap();
        assert_eq!(mock.take_frame(), Some(frame));
    }
}
