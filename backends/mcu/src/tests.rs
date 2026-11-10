//! Each adapter against a peripheral double.

use core::convert::Infallible;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use std::boxed::Box;
use std::collections::VecDeque;
use std::vec;
use std::vec::Vec;

use embedded_hal::digital::{self, InputPin, OutputPin, StatefulOutputPin};
use embedded_hal::i2c::{self, I2c as _, NoAcknowledgeSource, Operation};
use embedded_hal::pwm::{self, SetDutyCycle};
use lemnos_core::{HostClock, I2c, Line, Provider, Status};
use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::{
    ControlSetup, IsoPacket, UsbDevice, UsbDeviceId, UsbDeviceInfo, UsbFunction, UsbFunctionEvent,
    UsbHost, UsbSpeed,
};
use lemnos_hal::raw::{EdgeDetect, LineConfig, Polarity, PwmConfig, RawLine, RawPwm};

use crate::usb::{Endpoint, Function, FunctionPort, Host, HostPort, PortEvent, TransferKind};
use crate::{Buses, I2cBus, InputLine, Named, OutputLine, PwmChannel};

/// Makes a bus handle whichever way `lemnos-core` holds them: boxed when the build has an
/// allocator, `&'static mut` when it doesn't.
trait Hold<B> {
    fn hold(bus: B) -> Self;
}

macro_rules! hold {
    ($($bound:path),*) => {$(
        impl<B: $bound + Send + 'static> Hold<B> for &'static mut (dyn $bound + Send) {
            fn hold(bus: B) -> Self {
                Box::leak(Box::new(bus))
            }
        }
        impl<B: $bound + Send + 'static> Hold<B> for Box<dyn $bound + Send> {
            fn hold(bus: B) -> Self {
                Box::new(bus)
            }
        }
    )*};
}

hold!(lemnos_hal::i2c::I2c<Error = ErrorKind>, RawLine);

/// One I2C target with 256 registers, which stops acknowledging when told to.
#[derive(Debug)]
struct Registers {
    address: u8,
    values: [u8; 256],
    pointer: u8,
    deaf: bool,
}

#[derive(Debug)]
struct Nack;

impl i2c::Error for Nack {
    fn kind(&self) -> i2c::ErrorKind {
        i2c::ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address)
    }
}

impl i2c::ErrorType for Registers {
    type Error = Nack;
}

impl i2c::I2c for Registers {
    fn transaction(&mut self, address: u8, ops: &mut [Operation<'_>]) -> Result<(), Nack> {
        if self.deaf || address != self.address {
            return Err(Nack);
        }
        for op in ops {
            match op {
                Operation::Write(bytes) => {
                    if let Some((first, rest)) = bytes.split_first() {
                        self.pointer = *first;
                        for byte in rest {
                            self.values[usize::from(self.pointer)] = *byte;
                            self.pointer = self.pointer.wrapping_add(1);
                        }
                    }
                }
                Operation::Read(out) => {
                    for byte in out.iter_mut() {
                        *byte = self.values[usize::from(self.pointer)];
                        self.pointer = self.pointer.wrapping_add(1);
                    }
                }
            }
        }
        Ok(())
    }
}

fn registers() -> Registers {
    Registers {
        address: 0x40,
        values: [0; 256],
        pointer: 0,
        deaf: false,
    }
}

#[test]
fn an_i2c_bus_carries_transfers_and_reports_its_errors_as_kinds() {
    let mut bus = I2cBus(registers());
    bus.write(0x40, &[0x10, 0xab, 0xcd]).unwrap();
    let mut out = [0; 2];
    bus.write_read(0x40, &[0x10], &mut out).unwrap();
    assert_eq!(out, [0xab, 0xcd]);
    assert_eq!(bus.write(0x41, &[0]), Err(ErrorKind::Nack));
    bus.0.deaf = true;
    assert_eq!(bus.read(0x40, &mut out), Err(ErrorKind::Nack));
}

#[derive(Debug, Default)]
struct Pin {
    high: bool,
}

impl digital::ErrorType for Pin {
    type Error = Infallible;
}

impl OutputPin for Pin {
    fn set_low(&mut self) -> Result<(), Infallible> {
        self.high = false;
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Infallible> {
        self.high = true;
        Ok(())
    }
}

impl StatefulOutputPin for Pin {
    fn is_set_high(&mut self) -> Result<bool, Infallible> {
        Ok(self.high)
    }

    fn is_set_low(&mut self) -> Result<bool, Infallible> {
        Ok(!self.high)
    }
}

impl InputPin for Pin {
    fn is_high(&mut self) -> Result<bool, Infallible> {
        Ok(self.high)
    }

    fn is_low(&mut self) -> Result<bool, Infallible> {
        Ok(!self.high)
    }
}

#[test]
fn an_output_line_drives_its_pin_with_active_low_applied() {
    let mut line = OutputLine::new(Pin::default());
    line.configure(&LineConfig::output(true).active_low())
        .unwrap();
    assert!(line.get().unwrap());
    line.set(false).unwrap();
    assert!(!line.get().unwrap());
    assert!(
        line.into_inner().high,
        "logical low is a high pin when active low"
    );
}

#[test]
fn a_line_refuses_what_its_pin_type_cannot_do() {
    let mut out = OutputLine::new(Pin::default());
    assert_eq!(
        out.configure(&LineConfig::input()),
        Err(ErrorKind::Unsupported)
    );
    let mut input = InputLine::new(Pin { high: true });
    assert_eq!(
        input.configure(&LineConfig::input().with_edge(EdgeDetect::Both)),
        Err(ErrorKind::Unsupported)
    );
    input.configure(&LineConfig::input()).unwrap();
    assert!(input.get().unwrap());
    assert_eq!(input.set(true), Err(ErrorKind::Unsupported));
    assert_eq!(input.read_edge(), Ok(None));
}

#[derive(Debug, Default)]
struct Duty {
    value: u16,
}

impl pwm::ErrorType for Duty {
    type Error = Infallible;
}

impl SetDutyCycle for Duty {
    fn max_duty_cycle(&self) -> u16 {
        1000
    }

    fn set_duty_cycle(&mut self, duty: u16) -> Result<(), Infallible> {
        self.value = duty;
        Ok(())
    }
}

#[test]
fn a_pwm_channel_scales_duty_to_its_timer() {
    let mut channel = PwmChannel::new(Duty::default(), 40_000);
    let mut config = PwmConfig {
        period_ns: 40_000,
        duty_ns: 10_000,
        polarity: Polarity::Normal,
        enabled: true,
    };
    channel.configure(&config).unwrap();
    assert_eq!(channel.config().unwrap(), config);
    config.polarity = Polarity::Inversed;
    channel.configure(&config).unwrap();
    config.enabled = false;
    let mut channel_off = PwmChannel::new(Duty { value: 7 }, 40_000);
    channel_off.configure(&config).unwrap();
    assert_eq!(channel.into_inner().value, 750);
    assert_eq!(channel_off.into_inner().value, 0);
}

#[test]
fn a_pwm_channel_refuses_another_period_or_too_much_duty() {
    let mut channel = PwmChannel::new(Duty::default(), 40_000);
    let mut config = PwmConfig {
        period_ns: 20_000,
        duty_ns: 0,
        polarity: Polarity::Normal,
        enabled: true,
    };
    assert_eq!(channel.configure(&config), Err(ErrorKind::Unsupported));
    config.period_ns = 40_000;
    config.duty_ns = 40_001;
    assert_eq!(channel.configure(&config), Err(ErrorKind::InvalidInput));
}

#[test]
fn buses_give_each_handle_once_by_name() {
    let mut i2c = [Named::new("i2c0", I2c(Hold::hold(I2cBus(registers()))))];
    let line: Line = Hold::hold(OutputLine::new(Pin::default()));
    let mut lines = [Named::new("led", line)];
    let mut buses = Buses::new().with_i2c(&mut i2c).with_lines(&mut lines);
    assert!(buses.provided("i2c0") && buses.provided("led"));
    assert!(!buses.provided("i2c1"));
    let mut bus = buses.i2c("i2c0").unwrap();
    assert!(!buses.provided("i2c0"));
    assert_eq!(buses.i2c("i2c0").err(), Some(Status::BUSY));
    assert_eq!(buses.i2c("i2c1").err(), Some(Status::NOT_FOUND));
    assert_eq!(buses.spi("i2c0").err(), Some(Status::NOT_FOUND));
    bus.write(0x40, &[0, 1]).unwrap();
    buses.line("led").unwrap().set(true).unwrap();
}

/// A clock that moves 1 ms each time it is read.
struct Ticking(AtomicU64);

impl HostClock for Ticking {
    fn now_ns(&self) -> u64 {
        self.0.fetch_add(1_000_000, Ordering::Relaxed)
    }
}

static CLOCK: Ticking = Ticking(AtomicU64::new(0));

/// A USB device peripheral with a test playing the host.
#[derive(Debug, Default)]
struct DevicePort {
    endpoints: Vec<Endpoint>,
    connected: bool,
    events: VecDeque<PortEvent>,
    answered: Vec<Vec<u8>>,
    stalls: usize,
    /// Polls before the data stage of a control request has come, then the data.
    control_data: Option<(usize, Vec<u8>)>,
    /// Data stages taken, which accepts their requests.
    accepted: usize,
    halts: Vec<(u8, bool)>,
    /// What `configure` answers for this address, if not `Ok`.
    refuse: Option<(u8, ErrorKind)>,
    /// Polls before a packet arrives, then the packet.
    incoming: Option<(usize, Vec<u8>)>,
    written: Vec<(u8, Vec<u8>)>,
}

/// The packet in `slot` once its polls have run out.
fn arrive(slot: &mut Option<(usize, Vec<u8>)>, data: &mut [u8]) -> Option<usize> {
    match slot.as_mut() {
        Some((0, packet)) => {
            let n = packet.len().min(data.len());
            data[..n].copy_from_slice(&packet[..n]);
            *slot = None;
            Some(n)
        }
        Some((waits, _)) => {
            *waits -= 1;
            None
        }
        None => None,
    }
}

impl FunctionPort for DevicePort {
    type Error = ErrorKind;

    fn configure(&mut self, endpoint: Endpoint) -> Result<(), ErrorKind> {
        if let Some((address, kind)) = self.refuse
            && address == endpoint.address
        {
            return Err(kind);
        }
        self.endpoints.push(endpoint);
        Ok(())
    }

    fn connect(&mut self) -> Result<(), ErrorKind> {
        self.connected = true;
        Ok(())
    }

    fn disconnect(&mut self) -> Result<(), ErrorKind> {
        self.connected = false;
        self.endpoints.clear();
        Ok(())
    }

    fn poll(&mut self) -> Result<Option<PortEvent>, ErrorKind> {
        Ok(self.events.pop_front())
    }

    fn control_in(&mut self, data: &[u8]) -> Result<(), ErrorKind> {
        self.answered.push(data.to_vec());
        Ok(())
    }

    fn control_out(&mut self, data: &mut [u8]) -> Result<Option<usize>, ErrorKind> {
        if data.is_empty() {
            self.accepted += 1;
            return Ok(Some(0));
        }
        let got = arrive(&mut self.control_data, data);
        self.accepted += usize::from(got.is_some());
        Ok(got)
    }

    fn stall(&mut self) -> Result<(), ErrorKind> {
        self.stalls += 1;
        Ok(())
    }

    fn set_halt(&mut self, endpoint: u8, halted: bool) -> Result<(), ErrorKind> {
        self.halts.push((endpoint, halted));
        Ok(())
    }

    fn read(&mut self, _endpoint: u8, data: &mut [u8]) -> Result<Option<usize>, ErrorKind> {
        Ok(arrive(&mut self.incoming, data))
    }

    fn write(&mut self, endpoint: u8, data: &[u8]) -> Result<Option<usize>, ErrorKind> {
        self.written.push((endpoint, data.to_vec()));
        Ok(Some(data.len()))
    }
}

/// A device, a configuration with one interface and two bulk endpoints, and two strings.
fn descriptors() -> Vec<u8> {
    let mut out = vec![
        18, 1, 0x00, 0x02, 0, 0, 0, 64, 0x34, 0x12, 0x78, 0x56, 0, 1, 0, 1, 0, 1,
    ];
    out.extend_from_slice(&[9, 2, 32, 0, 1, 1, 0, 0x80, 50]);
    out.extend_from_slice(&[9, 4, 0, 0, 2, 0xff, 0, 0, 0]);
    out.extend_from_slice(&[7, 5, 0x81, 2, 64, 0, 0]);
    out.extend_from_slice(&[7, 5, 0x02, 2, 64, 0, 0]);
    out.extend_from_slice(&[4, 3, 0x09, 0x04]);
    out.extend_from_slice(&[6, 3, b'a', 0, b'b', 0]);
    out
}

/// The same device with the configuration at high speed after the one at full speed.
fn two_speeds() -> Vec<u8> {
    let full = descriptors();
    let mut out = full[..50].to_vec();
    out.extend_from_slice(&[9, 2, 32, 0, 1, 1, 0, 0x80, 50]);
    out.extend_from_slice(&[9, 4, 0, 0, 2, 0xff, 0, 0, 0]);
    out.extend_from_slice(&[7, 5, 0x81, 2, 0, 2, 0]);
    out.extend_from_slice(&[7, 5, 0x02, 2, 0, 2, 0]);
    out.extend_from_slice(&full[50..]);
    out
}

/// A request on the port, with its `wLength`.
fn setup(request_type: u8, request: u8, value: u16, length: u16) -> PortEvent {
    PortEvent::Setup {
        setup: ControlSetup {
            request_type,
            request,
            value,
            index: 0,
        },
        length,
    }
}

/// What the function reports for a request the port saw.
fn reported(event: PortEvent) -> UsbFunctionEvent {
    let PortEvent::Setup { setup, length } = event else {
        panic!("not a request");
    };
    UsbFunctionEvent::Setup { setup, length }
}

fn get_descriptor(kind: u8, index: u8) -> PortEvent {
    setup(0x80, 6, u16::from_le_bytes([index, kind]), 255)
}

type TestFunction = Function<DevicePort, 128>;

fn function() -> TestFunction {
    Function::new(DevicePort::default(), &CLOCK)
}

/// A function the host has configured.
fn configured() -> TestFunction {
    let mut function = function();
    function.enable(&descriptors()).unwrap();
    function.port().events.push_back(PortEvent::Configured);
    assert_eq!(
        function.next_event(),
        Ok(Some(UsbFunctionEvent::Configured))
    );
    function
}

const SHORT: Duration = Duration::from_millis(5);

#[test]
fn a_function_configures_the_endpoints_its_descriptors_name_and_connects() {
    let mut function = function();
    function.enable(&descriptors()).unwrap();
    assert_eq!(function.enable(&descriptors()), Err(ErrorKind::Busy));
    let port = function.port();
    assert!(port.connected);
    assert_eq!(
        port.endpoints,
        [
            Endpoint {
                address: 0x81,
                kind: TransferKind::Bulk,
                max_packet: 64,
                interval: 0
            },
            Endpoint {
                address: 0x02,
                kind: TransferKind::Bulk,
                max_packet: 64,
                interval: 0
            },
        ]
    );
    function.disable().unwrap();
    assert!(!function.port().connected);
    assert_eq!(function.disable(), Ok(()), "already disabled");
    function.enable(&descriptors()).unwrap();
}

#[test]
fn a_function_refuses_descriptors_it_cannot_keep_or_read() {
    let mut small: Function<DevicePort, 32> = Function::new(DevicePort::default(), &CLOCK);
    assert_eq!(small.enable(&descriptors()), Err(ErrorKind::InvalidInput));
    let mut function = function();
    let mut short = descriptors();
    short[20] = 200; // wTotalLength past the end
    assert_eq!(function.enable(&short), Err(ErrorKind::InvalidInput));
    let mut zero = descriptors();
    zero[18 + 9] = 0; // a descriptor of length 0
    assert_eq!(function.enable(&zero), Err(ErrorKind::InvalidInput));
    let mut control = descriptors();
    control[18 + 18 + 3] = 0; // an endpoint that says it is a control endpoint
    assert_eq!(function.enable(&control), Err(ErrorKind::InvalidInput));
    assert!(function.port().endpoints.is_empty(), "nothing configured");
    assert!(!function.port().connected);
    assert_eq!(function.next_event(), Err(ErrorKind::Unavailable));
}

#[test]
fn an_enable_the_port_refuses_part_way_leaves_nothing_behind() {
    let mut function = function();
    function.port().refuse = Some((0x02, ErrorKind::Unsupported));
    assert_eq!(function.enable(&descriptors()), Err(ErrorKind::Unsupported));
    assert!(
        function.port().endpoints.is_empty(),
        "the first endpoint is gone again"
    );
    assert!(!function.port().connected);
    assert_eq!(function.next_event(), Err(ErrorKind::Unavailable));
    function.port().refuse = None;
    function.enable(&descriptors()).unwrap();
    assert_eq!(function.port().endpoints.len(), 2);
}

#[test]
fn a_function_answers_get_descriptor_from_its_copy() {
    let mut function = function();
    let bytes = descriptors();
    function.enable(&bytes).unwrap();
    let port = function.port();
    port.events.extend([
        get_descriptor(1, 0),
        get_descriptor(2, 0),
        get_descriptor(3, 1),
        get_descriptor(3, 7),
        setup(0x80, 6, 0x0200, 9),
        PortEvent::Configured,
    ]);
    assert_eq!(
        function.next_event().unwrap(),
        Some(UsbFunctionEvent::Configured)
    );
    let port = function.port();
    assert_eq!(
        port.answered,
        [
            bytes[..18].to_vec(),
            bytes[18..50].to_vec(),
            bytes[54..60].to_vec(),
            bytes[18..27].to_vec(),
        ]
    );
    assert_eq!(port.stalls, 1, "string 7 doesn't exist");
}

#[test]
fn a_function_serves_the_full_speed_configuration_of_a_set_with_two() {
    let mut function = function();
    let bytes = two_speeds();
    function.enable(&bytes).unwrap();
    assert_eq!(function.port().endpoints.len(), 2);
    assert_eq!(function.port().endpoints[0].max_packet, 64);
    function
        .port()
        .events
        .extend([get_descriptor(2, 0), get_descriptor(3, 1)]);
    assert_eq!(function.next_event(), Ok(None));
    assert_eq!(
        function.port().answered,
        [bytes[18..50].to_vec(), bytes[86..92].to_vec()]
    );
}

#[test]
fn a_function_passes_other_requests_and_bus_events_up() {
    let mut function = function();
    function.enable(&descriptors()).unwrap();
    let vendor = setup(0xc0, 1, 2, 4);
    function.port().events.extend([
        PortEvent::Reset,
        PortEvent::Configured,
        vendor,
        PortEvent::Reset,
        PortEvent::Deconfigured,
        PortEvent::Suspended,
        PortEvent::Resumed,
    ]);
    let mut seen = Vec::new();
    while let Some(event) = function.next_event().unwrap() {
        seen.push(event);
    }
    assert_eq!(
        seen,
        [
            // The reset before any configuration changed nothing the function knew of.
            UsbFunctionEvent::Configured,
            reported(vendor),
            UsbFunctionEvent::Deconfigured,
            UsbFunctionEvent::Suspended,
            UsbFunctionEvent::Resumed,
        ]
    );
    assert_eq!(function.port().stalls, 1, "the request nobody answered");
}

#[test]
fn a_function_answers_a_control_request_with_data_bounded_by_its_length() {
    let mut function = configured();
    let none = Err(ErrorKind::InvalidInput);
    assert_eq!(function.control_in(&[1], SHORT), none, "nothing pending");
    assert_eq!(function.control_stall(), Err(ErrorKind::InvalidInput));

    let request = setup(0xc0, 1, 2, 2);
    function.port().events.push_back(request);
    assert_eq!(function.next_event(), Ok(Some(reported(request))));
    assert_eq!(function.control_out(&mut [0; 2], SHORT), none, "wrong way");
    assert_eq!(function.control_in(&[1, 2, 3], SHORT), Ok(2));
    assert_eq!(function.port().answered, [vec![1, 2]]);
    assert_eq!(function.control_in(&[1], SHORT), none, "answered once");
    assert_eq!(function.next_event(), Ok(None));
    assert_eq!(function.port().stalls, 0);
}

#[test]
fn a_function_takes_the_data_of_a_control_request_or_times_out_and_stalls_it() {
    let mut function = configured();
    let request = setup(0x40, 1, 2, 3);
    let mut data = [0; 8];

    function.port().events.push_back(request);
    function.next_event().unwrap();
    assert_eq!(
        function.control_in(&[1], SHORT),
        Err(ErrorKind::InvalidInput),
        "wrong way"
    );
    assert_eq!(
        function.control_out(&mut data[..2], SHORT),
        Err(ErrorKind::InvalidInput),
        "too small for wLength"
    );
    function.port().control_data = Some((3, vec![7, 8, 9]));
    assert_eq!(
        function.control_out(&mut data, Duration::from_millis(50)),
        Ok(3)
    );
    assert_eq!(data[..4], [7, 8, 9, 0], "no more than wLength is taken");
    assert_eq!(function.port().accepted, 1);

    // No data stage: taking it accepts the request.
    function.port().events.push_back(setup(0x40, 1, 2, 0));
    function.next_event().unwrap();
    assert_eq!(function.control_out(&mut [], SHORT), Ok(0));
    assert_eq!(function.port().accepted, 2);

    // The host never sends its data.
    function.port().events.push_back(request);
    function.next_event().unwrap();
    assert_eq!(
        function.control_out(&mut data, SHORT),
        Err(ErrorKind::Timeout)
    );
    assert_eq!(function.port().stalls, 1);
    assert_eq!(
        function.control_stall(),
        Err(ErrorKind::InvalidInput),
        "nothing is pending after a timeout"
    );
}

#[test]
fn a_function_stalls_a_request_it_refuses_and_one_it_reads_past() {
    let mut function = configured();
    let request = setup(0xc0, 9, 0, 1);
    function.port().events.push_back(request);
    function.next_event().unwrap();
    assert_eq!(function.control_stall(), Ok(()));
    assert_eq!(function.port().stalls, 1);
    assert_eq!(function.next_event(), Ok(None));
    assert_eq!(function.port().stalls, 1, "stalled once");

    function.port().events.push_back(request);
    function.next_event().unwrap();
    assert_eq!(function.next_event(), Ok(None));
    assert_eq!(function.port().stalls, 2, "read past without an answer");
    assert_eq!(
        function.control_in(&[1], SHORT),
        Err(ErrorKind::InvalidInput)
    );
}

#[test]
fn a_reset_ends_the_configuration_and_the_pending_request_without_disabling() {
    let mut function = configured();
    let mut data = [0; 4];
    function
        .port()
        .events
        .extend([setup(0xc0, 1, 0, 1), PortEvent::Reset, PortEvent::Reset]);
    function.next_event().unwrap();
    assert_eq!(
        function.next_event(),
        Ok(Some(UsbFunctionEvent::Deconfigured))
    );
    assert_eq!(
        function.next_event(),
        Ok(None),
        "a second reset says nothing"
    );
    let off = Err(ErrorKind::Unavailable);
    assert_eq!(function.read(2, &mut data, SHORT), off);
    assert_eq!(function.write(0x81, &[1], SHORT), off);
    assert_eq!(function.set_halt(2, true), Err(ErrorKind::Unavailable));
    assert_eq!(
        function.control_in(&[1], SHORT),
        Err(ErrorKind::InvalidInput),
        "the request went with the reset"
    );
    // The host comes back: no enable needed.
    function.port().events.push_back(PortEvent::Configured);
    function.next_event().unwrap();
    assert_eq!(function.write(0x81, &[1], SHORT), Ok(1));
    assert!(function.port().connected);
}

#[test]
fn a_function_halts_only_endpoints_its_descriptors_name() {
    let mut function = configured();
    function.set_halt(0x81, true).unwrap();
    function.set_halt(0x81, false).unwrap();
    function.set_halt(2, true).unwrap();
    assert_eq!(
        function.port().halts,
        [(0x81, true), (0x81, false), (2, true)]
    );
    for endpoint in [0, 0x80, 1, 0x82, 3, 0x12, 0xff] {
        assert_eq!(
            function.set_halt(endpoint, true),
            Err(ErrorKind::InvalidInput),
            "{endpoint:#x}"
        );
    }
    assert_eq!(function.port().halts.len(), 3);

    // An isochronous endpoint has no halt.
    let mut iso = descriptors();
    iso[18 + 18 + 3] = 1;
    let mut function = Function::<DevicePort, 128>::new(DevicePort::default(), &CLOCK);
    function.enable(&iso).unwrap();
    function.port().events.push_back(PortEvent::Configured);
    function.next_event().unwrap();
    assert_eq!(function.set_halt(0x81, true), Err(ErrorKind::Unsupported));
    assert!(function.port().halts.is_empty());
}

#[test]
fn a_function_waits_for_a_packet_until_its_timeout() {
    let mut function = function();
    let mut out = [0; 8];
    assert_eq!(
        function.read(2, &mut out, SHORT),
        Err(ErrorKind::Unavailable),
        "not enabled"
    );
    function.enable(&descriptors()).unwrap();
    assert_eq!(
        function.read(2, &mut out, SHORT),
        Err(ErrorKind::Unavailable),
        "not configured"
    );
    function.port().events.push_back(PortEvent::Configured);
    function.next_event().unwrap();
    function.port().incoming = Some((3, vec![1, 2, 3]));
    assert_eq!(function.read(2, &mut out, Duration::from_millis(50)), Ok(3));
    assert_eq!(out[..3], [1, 2, 3]);
    assert_eq!(function.read(2, &mut out, SHORT), Err(ErrorKind::Timeout));
    assert_eq!(function.write(0x81, &[9], SHORT), Ok(1));
    assert_eq!(function.port().written, [(0x81, vec![9])]);
    let wrong = Err(ErrorKind::InvalidInput);
    assert_eq!(
        function.read(0x81, &mut out, SHORT),
        wrong,
        "an IN endpoint"
    );
    assert_eq!(function.write(2, &[9], SHORT), wrong, "an OUT endpoint");
    assert_eq!(function.write(0x83, &[9], SHORT), wrong, "not named");
    assert_eq!(function.port().written.len(), 1);
}

/// What a [`HostPort`] was asked, shared by every clone.
#[derive(Debug, Default)]
struct HostLog {
    opened: Vec<UsbDeviceId>,
    control: Vec<(UsbDeviceId, ControlSetup, usize)>,
    transfers: Vec<(u8, TransferKind, usize)>,
    resets: Vec<UsbDeviceId>,
    /// What the next reset answers, if not `Ok`.
    reset_error: Option<ErrorKind>,
}

#[derive(Debug, Clone)]
struct HostDouble(std::sync::Arc<std::sync::Mutex<HostLog>>);

const PLUGGED: UsbDeviceInfo = UsbDeviceInfo {
    id: UsbDeviceId { bus: 1, address: 4 },
    vendor: 0x1234,
    product: 0x5678,
    class: 0xff,
    speed: UsbSpeed::Full,
};

impl HostPort for HostDouble {
    type Error = ErrorKind;

    fn enumerate(&mut self, out: &mut [UsbDeviceInfo]) -> Result<usize, ErrorKind> {
        if let Some(first) = out.first_mut() {
            *first = PLUGGED;
        }
        Ok(1)
    }

    fn open(&mut self, device: UsbDeviceId) -> Result<(), ErrorKind> {
        if device != PLUGGED.id {
            return Err(ErrorKind::NotFound);
        }
        self.0.lock().unwrap().opened.push(device);
        Ok(())
    }

    fn reset(&mut self, device: UsbDeviceId) -> Result<(), ErrorKind> {
        let mut log = self.0.lock().unwrap();
        log.resets.push(device);
        log.reset_error.take().map_or(Ok(()), Err)
    }

    fn control_in(
        &mut self,
        device: UsbDeviceId,
        setup: ControlSetup,
        data: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.0
            .lock()
            .unwrap()
            .control
            .push((device, setup, data.len()));
        data.fill(0xee);
        Ok(data.len())
    }

    fn control_out(
        &mut self,
        device: UsbDeviceId,
        setup: ControlSetup,
        data: &[u8],
        _timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.0
            .lock()
            .unwrap()
            .control
            .push((device, setup, data.len()));
        Ok(data.len())
    }

    fn transfer_in(
        &mut self,
        _device: UsbDeviceId,
        endpoint: u8,
        kind: TransferKind,
        data: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.0
            .lock()
            .unwrap()
            .transfers
            .push((endpoint, kind, data.len()));
        Ok(data.len())
    }

    fn transfer_out(
        &mut self,
        _device: UsbDeviceId,
        endpoint: u8,
        kind: TransferKind,
        data: &[u8],
        _timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.0
            .lock()
            .unwrap()
            .transfers
            .push((endpoint, kind, data.len()));
        Ok(data.len())
    }

    fn iso_in(
        &mut self,
        _device: UsbDeviceId,
        _endpoint: u8,
        _data: &mut [u8],
        packets: &mut [IsoPacket],
        _timeout: Duration,
    ) -> Result<(), ErrorKind> {
        for packet in packets {
            packet.received = packet.capacity;
            packet.ok = true;
        }
        Ok(())
    }
}

#[test]
fn a_host_lists_and_opens_through_its_port() {
    let log = HostDouble(Default::default());
    let mut host = Host::new(log.clone());
    let mut found = [PLUGGED; 2];
    assert_eq!(host.enumerate(&mut found).unwrap(), 1);
    assert_eq!(
        host.open(UsbDeviceId { bus: 1, address: 9 }).err(),
        Some(ErrorKind::NotFound)
    );
    let device = host.open(PLUGGED.id).unwrap();
    assert_eq!(device.id(), PLUGGED.id);
    assert_eq!(log.0.lock().unwrap().opened, [PLUGGED.id]);
}

#[test]
fn a_host_device_keeps_claims_and_makes_the_standard_requests() {
    let log = HostDouble(Default::default());
    let mut device = Host::new(log.clone()).open(PLUGGED.id).unwrap();
    let mut config = [0; 9];
    assert_eq!(device.config_descriptor(0, &mut config).unwrap(), 9);
    assert_eq!(device.set_alternate(1, 2), Err(ErrorKind::InvalidInput));
    assert_eq!(device.release_interface(1), Err(ErrorKind::InvalidInput));
    device.claim_interface(1).unwrap();
    device.set_alternate(1, 2).unwrap();
    device.release_interface(1).unwrap();
    assert!(!device.holds(1));
    assert_eq!(device.claim_interface(32), Err(ErrorKind::InvalidInput));
    let log = log.0.lock().unwrap();
    let (_, get, len) = log.control[0];
    assert_eq!(
        (get.request_type, get.request, get.value, len),
        (0x80, 6, 0x0200, 9)
    );
    let (_, set, _) = log.control[1];
    assert_eq!(
        (set.request_type, set.request, set.value, set.index),
        (0x01, 11, 2, 1)
    );
}

#[test]
fn a_host_device_resets_through_its_port_and_has_no_kernel_driver_to_detach() {
    let log = HostDouble(Default::default());
    let mut device = Host::new(log.clone()).open(PLUGGED.id).unwrap();
    device.claim_interface(1).unwrap();
    assert_eq!(device.detach_kernel_driver(1), Ok(()));
    assert_eq!(device.detach_kernel_driver(200), Ok(()));
    assert_eq!(device.reset(), Ok(()));
    assert!(device.holds(1), "claims stand when the device does");
    log.0.lock().unwrap().reset_error = Some(ErrorKind::Unavailable);
    assert_eq!(device.reset(), Err(ErrorKind::Unavailable));
    let log = log.0.lock().unwrap();
    assert_eq!(log.resets, [PLUGGED.id, PLUGGED.id]);
    assert!(log.control.is_empty(), "detaching sends nothing");
}

#[test]
fn a_host_device_carries_each_kind_of_transfer() {
    let log = HostDouble(Default::default());
    let mut device = Host::new(log.clone()).open(PLUGGED.id).unwrap();
    let mut data = [0; 16];
    device
        .bulk_in(0x81, &mut data, Duration::from_millis(5))
        .unwrap();
    device
        .bulk_out(0x02, &data[..4], Duration::from_millis(5))
        .unwrap();
    device
        .interrupt_in(0x83, &mut data[..8], Duration::from_millis(5))
        .unwrap();
    let mut packets = [IsoPacket {
        capacity: 8,
        ..IsoPacket::default()
    }; 2];
    device
        .iso_in(0x84, &mut data, &mut packets, Duration::from_millis(5))
        .unwrap();
    assert!(
        packets
            .iter()
            .all(|packet| packet.ok && packet.received == 8)
    );
    assert_eq!(
        log.0.lock().unwrap().transfers,
        [
            (0x81, TransferKind::Bulk, 16),
            (0x02, TransferKind::Bulk, 4),
            (0x83, TransferKind::Interrupt, 8),
        ]
    );
}
