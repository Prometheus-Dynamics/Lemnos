//! The driver against the mock function, which keeps to what `UsbFunction` says and checks the
//! descriptors it is given. Time is simulated: the rig ticks at exactly the time the driver
//! asked for.

extern crate std;

use std::boxed::Box;
use std::vec::Vec;

use lemnos_core::{ConfigValue, Driver, LocalCaller, Provider, Resources, Tag};
use lemnos_hal::blocks::ControlSetup;
use lemnos_hal::mock::{ControlAnswer, MockUsbFunction};
use lemnos_hal::usb_descriptors::{self, UsbDescriptors, UsbEndpoint};
use lemnos_measure::test_alloc::{Counting, allocations};

use super::*;
use crate::description::{MAX_ENDPOINTS, MAX_INTERFACES, MAX_NAME, build};
use crate::{NO_SERIAL, NO_SERIAL_READ};
use lemnos_hal::usb_descriptors::GadgetFunction;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const SECOND: u64 = 1_000_000_000;

const INTERFACES: &str = "
# One vendor interface with a pipe each way and a status endpoint.
interface 0xff 0x01 2 data pipe
endpoint in bulk
endpoint out bulk
endpoint in interrupt 16 10   # every 10 ms

interface 3 0 0
";

const IDENTITY: Identity<'static> = Identity {
    vendor: 0x1209,
    product: 0x0001,
    release: 0x0203,
    class: 0xef,
    subclass: 2,
    protocol: 1,
    manufacturer: Some("Maker"),
    product_name: None,
    serial: Some("S1"),
    max_power_ma: 99,
    self_powered: true,
};

#[test]
fn a_description_becomes_the_set_byte_for_byte() {
    let set = build(&IDENTITY, INTERFACES).unwrap();
    #[rustfmt::skip]
    let expected: Vec<u8> = [
        // Device: USB 2.0, the class triple, 64-byte control packets, strings 1, none, 2.
        &[18, 1, 0x00, 0x02, 0xef, 2, 1, 64, 0x09, 0x12, 0x01, 0x00, 0x03, 0x02, 1, 0, 2, 1][..],
        // Full speed: 48 bytes, two interfaces, self powered, 50 units of 2 mA.
        &[9, 2, 48, 0, 2, 1, 0, 0xc0, 50],
        &[9, 4, 0, 0, 3, 0xff, 1, 2, 3],
        &[7, 5, 0x81, 2, 64, 0, 0],
        &[7, 5, 0x02, 2, 64, 0, 0],
        &[7, 5, 0x83, 3, 16, 0, 10],
        &[9, 4, 1, 0, 0, 3, 0, 0, 0],
        // High speed: the same, with 512-byte bulk packets and the interval as a power of two.
        &[9, 2, 48, 0, 2, 1, 0, 0xc0, 50],
        &[9, 4, 0, 0, 3, 0xff, 1, 2, 3],
        &[7, 5, 0x81, 2, 0, 2, 0],
        &[7, 5, 0x02, 2, 0, 2, 0],
        &[7, 5, 0x83, 3, 16, 0, 7],
        &[9, 4, 1, 0, 0, 3, 0, 0, 0],
        // Strings: US English, then in index order.
        &[4, 3, 0x09, 0x04],
        &[12, 3, b'M', 0, b'a', 0, b'k', 0, b'e', 0, b'r', 0],
        &[6, 3, b'S', 0, b'1', 0],
        &[20, 3, b'd', 0, b'a', 0, b't', 0, b'a', 0, b' ', 0, b'p', 0, b'i', 0, b'p', 0, b'e', 0],
    ]
    .concat();
    assert_eq!(set.bytes(), expected);

    let parsed = UsbDescriptors::parse(set.bytes()).unwrap();
    assert_eq!(parsed.identity().vendor, 0x1209);
    let high: Vec<UsbEndpoint> = usb_descriptors::endpoints(parsed.high_speed().unwrap()).collect();
    assert_eq!(high.len(), 3);
}

#[test]
fn alternate_settings_and_isochronous_endpoints_become_the_set_byte_for_byte() {
    let text = "
interface 0x0e 1 0 control
alternate
interface 0x0e 2 0
alternate frames
endpoint in isochronous 1023 1
endpoint out isochronous 0x40 8
interface 0xff 0 0
";
    let identity = Identity {
        manufacturer: None,
        serial: None,
        ..IDENTITY
    };
    let set = build(&identity, text).unwrap();
    #[rustfmt::skip]
    let speed = |interval_in: u8, interval_out: u8| -> Vec<u8> {
        [
            // Three interfaces, five settings.
            &[9, 2, 68, 0, 3, 1, 0, 0xc0, 50][..],
            &[9, 4, 0, 0, 0, 0x0e, 1, 0, 1],
            &[9, 4, 0, 1, 0, 0x0e, 1, 0, 0],
            &[9, 4, 1, 0, 0, 0x0e, 2, 0, 0],
            &[9, 4, 1, 1, 2, 0x0e, 2, 0, 2],
            &[7, 5, 0x81, 0x05, 0xff, 0x03, interval_in],
            &[7, 5, 0x02, 0x05, 0x40, 0, interval_out],
            &[9, 4, 2, 0, 0, 0xff, 0, 0, 0],
        ]
        .concat()
    };
    // 1 ms is `bInterval` 1 at full speed and 4 at high speed; 8 ms is 4 and 7.
    let expected: Vec<u8> = [
        &set.bytes()[..18],
        &speed(1, 4),
        &speed(4, 7),
        &[4, 3, 0x09, 0x04],
        &[
            16, 3, b'c', 0, b'o', 0, b'n', 0, b't', 0, b'r', 0, b'o', 0, b'l', 0,
        ],
        &[14, 3, b'f', 0, b'r', 0, b'a', 0, b'm', 0, b'e', 0, b's', 0],
    ]
    .concat();
    assert_eq!(set.bytes(), expected);
    let parsed = UsbDescriptors::parse(set.bytes()).unwrap();
    assert_eq!(parsed.configuration().interfaces, 3);
}

#[test]
fn a_description_without_names_has_no_string_table() {
    let identity = Identity {
        manufacturer: None,
        serial: None,
        self_powered: false,
        max_power_ma: 500,
        ..IDENTITY
    };
    let set = build(&identity, "interface 0xff 0 0\nendpoint out bulk").unwrap();
    let parsed = UsbDescriptors::parse(set.bytes()).unwrap();
    assert_eq!(parsed.string_count(), 0);
    assert_eq!(parsed.configuration().attributes, 0x80);
    assert_eq!(parsed.configuration().max_power, 250);
    assert_eq!(set.bytes().len(), 18 + 2 * (9 + 9 + 7));
}

#[test]
fn an_interrupt_interval_rounds_down_to_a_power_of_two_at_high_speed() {
    for (ms, high) in [
        (1, 4),
        (2, 5),
        (3, 5),
        (4, 6),
        (10, 7),
        (128, 11),
        (255, 11),
    ] {
        let text = std::format!("interface 3 0 0\nendpoint in interrupt 8 {ms}");
        let set = build(&IDENTITY, &text).unwrap();
        let parsed = UsbDescriptors::parse(set.bytes()).unwrap();
        let interval = |config| usb_descriptors::endpoints(config).next().unwrap().interval;
        assert_eq!(interval(parsed.full_speed()), ms, "{ms} ms at full speed");
        assert_eq!(
            interval(parsed.high_speed().unwrap()),
            high,
            "{ms} ms at high speed"
        );
    }
}

#[test]
fn a_description_that_does_not_add_up_is_refused_with_the_reason() {
    let fault = |text: &str| build(&IDENTITY, text).err();
    assert_eq!(fault(""), Some(Fault::Empty));
    assert_eq!(fault("# nothing\n\n"), Some(Fault::Empty));
    assert_eq!(fault("endpoint in bulk"), Some(Fault::NoInterface(1)));
    assert_eq!(fault("\nalternate"), Some(Fault::NoInterface(2)));
    let lines = [
        "interface",
        "interface 0xff 0",
        "interface 256 0 0",
        "interface 0xff 0 zero",
        "interface 0xff 0 0\nendpoint",
        "interface 0xff 0 0\nendpoint up bulk",
        "interface 0xff 0 0\nendpoint in",
        "interface 0xff 0 0\nendpoint in control",
        "interface 0xff 0 0\nendpoint in isochronous",
        "interface 0xff 0 0\nendpoint in bulk 64",
        "interface 0xff 0 0\nendpoint in interrupt",
        "interface 0xff 0 0\nendpoint in interrupt 0 1",
        "interface 0xff 0 0\nendpoint in interrupt 65 1",
        "interface 0xff 0 0\nendpoint in interrupt 8 0",
        "interface 0xff 0 0\nendpoint in interrupt 8 256",
        "interface 0xff 0 0\nendpoint in interrupt 8 1 1",
        "interface 0xff 0 0\nendpoint in isochronous 0 1",
        "interface 0xff 0 0\nendpoint in isochronous 1024 1",
        "interface 0xff 0 0\nendpoint in isochronous 64 0",
        "interface 0xff 0 0\nendpoint in isochronous 64 3",
        "interface 0xff 0 0\nendpoint in isochronous 64 8192",
        "interface 0xff 0 0\nendpoint in isochronous 64 1 1",
        "interface 0xff 0 0\ninterfaces 1 2 3",
    ];
    for text in lines {
        let line = text.lines().count();
        assert_eq!(fault(text), Some(Fault::Line(line)), "{text}");
    }

    let many = "interface 1 0 0\n".repeat(MAX_INTERFACES);
    assert!(fault(&many).is_none());
    assert_eq!(
        fault(&std::format!("{many}interface 1 0 0")),
        Some(Fault::TooManyInterfaces)
    );
    let full = std::format!(
        "interface 1 0 0\n{}",
        "endpoint in bulk\n".repeat(MAX_ENDPOINTS)
    );
    assert!(fault(&full).is_none());
    assert_eq!(
        fault(&std::format!("{full}endpoint out bulk")),
        Some(Fault::TooManyEndpoints)
    );

    let long = "n".repeat(MAX_NAME + 1);
    assert!(fault(&std::format!("interface 1 0 0 {}", &long[1..])).is_none());
    assert_eq!(
        fault(&std::format!("interface 1 0 0 {long}")),
        Some(Fault::Name)
    );
    let named = |name| Identity {
        product_name: Some(name),
        ..IDENTITY
    };
    assert_eq!(
        build(&named(&long), "interface 1 0 0").err(),
        Some(Fault::Name)
    );
    assert_eq!(
        build(&named("a\0b"), "interface 1 0 0").err(),
        Some(Fault::Name)
    );
    let hungry = Identity {
        max_power_ma: 501,
        ..IDENTITY
    };
    assert_eq!(build(&hungry, "interface 1 0 0").err(), Some(Fault::Power));
}

#[test]
fn a_description_past_the_buffer_is_refused_as_too_long() {
    // Every interface and endpoint there can be, with names of a usual length.
    let name = "n".repeat(20);
    let identity = Identity {
        manufacturer: Some(&name),
        product_name: Some(&name),
        serial: Some(&name),
        ..IDENTITY
    };
    let mut text = "interface 1 0 0\n".repeat(MAX_INTERFACES);
    text.push_str(&"endpoint in bulk\n".repeat(MAX_ENDPOINTS));
    let set = build(&identity, &text).unwrap();
    assert_eq!(
        set.bytes().len(),
        18 + 2 * (9 + 8 * 9 + 15 * 7) + 4 + 3 * 42
    );
    assert!(UsbDescriptors::parse(set.bytes()).is_ok());

    // The same with every name at its longest: 11 strings of 128 bytes.
    let name = "n".repeat(MAX_NAME);
    let identity = Identity {
        manufacturer: Some(&name),
        product_name: Some(&name),
        serial: Some(&name),
        ..IDENTITY
    };
    let mut text = std::format!("interface 1 0 0 {name}\n").repeat(MAX_INTERFACES);
    text.push_str(&"endpoint in bulk\n".repeat(MAX_ENDPOINTS));
    assert_eq!(build(&identity, &text).err(), Some(Fault::TooLong));
}

struct Board(Option<MockUsbFunction>);

impl Provider for Board {
    fn provided(&self, need: &str) -> bool {
        need == "function"
    }

    fn device(&mut self, _need: &str, _tag: Tag) -> Result<LocalCaller> {
        Err(Status::NOT_SUPPORTED)
    }

    fn usb_function(&mut self, _need: &str) -> Result<UsbFunctionHandle> {
        let function = self.0.take().ok_or(Status::NOT_FOUND)?;
        Ok(lemnos_core::Held::boxed(Box::new(function)))
    }
}

fn settings() -> Vec<(&'static str, ConfigValue<'static>)> {
    std::vec![
        ("vendor", ConfigValue::Int(0x1209)),
        ("product", ConfigValue::Int(1)),
        ("manufacturer", ConfigValue::Str("Maker")),
        ("serial", ConfigValue::Str("S0")),
        ("interfaces", ConfigValue::Str(INTERFACES)),
    ]
}

/// The base settings with no serial at all: the gadget is up, and its health says so.
fn settings_without_serial() -> Vec<(&'static str, ConfigValue<'static>)> {
    settings()
        .into_iter()
        .filter(|(k, _)| *k != "serial")
        .collect()
}

fn bind(function: &MockUsbFunction, entries: &[(&str, ConfigValue<'_>)]) -> Result<UsbGadget> {
    let mut board = Board(Some(function.clone()));
    let mut resources = Resources::new(&mut board, None);
    UsbGadget::bind(&mut resources, &Config::new(entries))
}

/// The driver and the host's side of its function at a simulated time.
struct Rig {
    driver: UsbGadget,
    host: MockUsbFunction,
    now: u64,
}

impl Rig {
    fn new() -> Self {
        let host = MockUsbFunction::new();
        let driver = bind(&host, &settings()).map_err(|_| ()).expect("bind");
        Self {
            driver,
            host,
            now: 0,
        }
    }

    /// One tick, then the clock goes to the time the driver asked for. Returns the wait.
    fn step(&mut self) -> u64 {
        let next = Driver::tick(&mut self.driver, self.now).expect("the driver always ticks");
        let wait = next - self.now;
        self.now = next;
        wait
    }

    fn health(&self) -> Health {
        Driver::health(&self.driver)
    }
}

#[test]
fn binding_checks_the_settings_and_touches_no_function() {
    let function = MockUsbFunction::new();
    let driver = bind(&function, &settings()).map_err(|_| ()).unwrap();
    assert_eq!(Driver::health(&driver), Health::Degraded(STARTING));
    assert_eq!((function.enables(), function.descriptors()), (0, None));
    drop(driver);

    let replace = |key: &'static str, value: Option<ConfigValue<'static>>| {
        let mut entries = settings();
        entries.retain(|(k, _)| *k != key);
        if let Some(value) = value {
            entries.push((key, value));
        }
        entries
    };
    let refused: [(&str, Option<ConfigValue<'static>>); 12] = [
        ("interfaces", None),
        ("vendor", Some(ConfigValue::Int(0x10000))),
        ("vendor", Some(ConfigValue::Int(-1))),
        ("vendor", Some(ConfigValue::Str("0x1209"))),
        ("product", Some(ConfigValue::Float(1.0))),
        ("release", Some(ConfigValue::Int(0x10000))),
        ("class", Some(ConfigValue::Int(256))),
        ("max_power_ma", Some(ConfigValue::Int(501))),
        ("self_powered", Some(ConfigValue::Int(1))),
        ("serial", Some(ConfigValue::Int(7))),
        ("interfaces", Some(ConfigValue::Str("endpoint in bulk"))),
        ("interfaces", Some(ConfigValue::Int(1))),
    ];
    for (key, value) in refused {
        let entries = replace(key, value);
        assert_eq!(
            bind(&function, &entries).map(|_| ()).unwrap_err(),
            Status::INVALID_ARGUMENT,
            "{key} = {value:?}"
        );
    }
    assert_eq!(function.enables(), 0);

    // Every optional setting, given.
    let mut entries = settings();
    entries.extend([
        ("release", ConfigValue::Int(0x0203)),
        ("class", ConfigValue::Int(0xef)),
        ("subclass", ConfigValue::Int(2)),
        ("protocol", ConfigValue::Int(1)),
        ("product_name", ConfigValue::Str("Thing")),
        ("serial", ConfigValue::Str("S1")),
        ("max_power_ma", ConfigValue::Int(500)),
        ("self_powered", ConfigValue::Bool(true)),
    ]);
    let mut driver = bind(&function, &entries).map_err(|_| ()).unwrap();
    Driver::tick(&mut driver, 0);
    let bytes = function.descriptors().unwrap();
    let set = UsbDescriptors::parse(&bytes).unwrap();
    assert_eq!(
        (
            set.identity().release,
            set.identity().class,
            set.identity().protocol
        ),
        (0x0203, 0xef, 1)
    );
    assert_eq!(set.configuration().attributes, 0xc0);
    assert_eq!(set.configuration().max_power, 250);
    assert_eq!(set.string_count(), 5);
}

#[test]
fn the_first_tick_enables_the_function_with_the_set_and_then_polls() {
    let mut rig = Rig::new();
    assert_eq!(rig.step(), POLL_NS);
    assert_eq!(rig.health(), Health::Ok);
    assert_eq!(rig.host.enables(), 1);
    let bytes = rig.host.descriptors().unwrap();
    let set = UsbDescriptors::parse(&bytes).unwrap();
    assert_eq!((set.identity().vendor, set.identity().product), (0x1209, 1));
    assert_eq!(set.configuration().interfaces, 2);
    for _ in 0..100 {
        assert_eq!(rig.step(), POLL_NS);
    }
    assert_eq!(rig.host.enables(), 1, "enabled once");
    // A tick that comes early does nothing and names the same time.
    let due = rig.now;
    assert_eq!(Driver::tick(&mut rig.driver, due - 1), Some(due));
}

#[test]
fn a_function_that_cannot_be_enabled_is_retried_at_1_2_4_8_8_seconds_for_ever() {
    let mut rig = Rig::new();
    rig.host.refuse_enable(Some(ErrorKind::NotFound));
    let mut waits = Vec::new();
    for _ in 0..40 {
        waits.push(rig.step() / SECOND);
        assert_eq!(rig.health(), Health::Failed(NOT_THERE));
    }
    assert_eq!(waits[..6], [1, 2, 4, 8, 8, 8]);
    assert!(
        waits[6..].iter().all(|wait| *wait == 8),
        "never longer, never off"
    );
    assert_eq!(rig.host.enables(), 0);

    rig.host.refuse_enable(None);
    assert_eq!(rig.step(), POLL_NS);
    assert_eq!(rig.health(), Health::Ok);
    assert_eq!(rig.host.enables(), 1);
}

#[test]
fn an_endpoint_file_busy_at_enable_is_retried_with_its_reason_and_comes_up_when_it_frees() {
    // ep0 busy: an outside process still has the function's file open. Named as in use, retried
    // with the usual backoff, never given up, and up at the first try after it frees.
    let mut rig = Rig::new();
    rig.host.refuse_enable(Some(ErrorKind::Busy));
    let mut waits = Vec::new();
    for _ in 0..12 {
        waits.push(rig.step() / SECOND);
        assert_eq!(rig.health(), Health::Failed(IN_USE));
    }
    assert_eq!(waits[..5], [1, 2, 4, 8, 8]);
    assert!(
        waits[5..].iter().all(|wait| *wait == 8),
        "never longer, never off"
    );

    rig.host.refuse_enable(None);
    assert_eq!(rig.step(), POLL_NS);
    assert_eq!(rig.health(), Health::Ok);
    assert_eq!(rig.host.enables(), 1);
}

#[test]
fn each_way_an_enable_fails_has_its_reason() {
    let reasons = [
        (ErrorKind::NotFound, NOT_THERE),
        (ErrorKind::Unavailable, NOT_THERE),
        (ErrorKind::InvalidInput, REFUSED),
        (ErrorKind::Unsupported, REFUSED),
        (ErrorKind::PermissionDenied, DENIED),
        (ErrorKind::Busy, IN_USE),
        (ErrorKind::Failed, ENABLE_FAILED),
        (ErrorKind::Timeout, ENABLE_FAILED),
    ];
    for (kind, reason) in reasons {
        let mut rig = Rig::new();
        rig.host.refuse_enable(Some(kind));
        rig.step();
        assert_eq!(rig.health(), Health::Failed(reason), "{kind:?}");
    }
}

#[test]
fn a_function_left_enabled_is_taken_down_and_enabled_afresh() {
    let mut rig = Rig::new();
    // Whoever held the function before left it enabled with something else.
    let mut other = rig.host.clone();
    lemnos_hal::UsbFunction::enable(&mut other, rig.driver.set.bytes()).unwrap();
    assert_eq!(rig.step(), SECOND);
    assert_eq!(rig.health(), Health::Failed(IN_USE));
    assert_eq!(rig.host.descriptors(), None, "taken down");
    assert_eq!(rig.step(), POLL_NS);
    assert_eq!(rig.health(), Health::Ok);
    assert_eq!(rig.host.enables(), 2);
}

#[test]
fn a_controller_that_lets_go_is_enabled_again_after_the_backoff() {
    let mut rig = Rig::new();
    rig.step();
    rig.host.release();
    assert_eq!(rig.step(), SECOND);
    assert_eq!(rig.health(), Health::Failed(LET_GO));
    assert_eq!(
        rig.host.descriptors(),
        None,
        "disabled before the next attempt"
    );
    assert_eq!(rig.step(), POLL_NS);
    assert_eq!(rig.health(), Health::Ok);
    assert_eq!(rig.host.enables(), 2);

    // Again straight away: the wait doubles, since the gadget did not stay up.
    rig.host.release();
    assert_eq!(rig.step(), 2 * SECOND);
    rig.step();
    rig.host.release();
    assert_eq!(rig.step(), 4 * SECOND);
    assert_eq!(rig.host.enables(), 3);
}

#[test]
fn the_backoff_clears_once_the_gadget_has_stayed_up_for_8_seconds() {
    let mut rig = Rig::new();
    rig.host.refuse_enable(Some(ErrorKind::Failed));
    assert_eq!([rig.step(), rig.step()], [SECOND, 2 * SECOND]);
    rig.host.refuse_enable(None);
    rig.step();
    let up_at = rig.now - POLL_NS;

    // Up for just under 8 s: the next failure carries on where the waits left off.
    while rig.now + POLL_NS < up_at + BACKOFF_CAP_NS {
        rig.step();
    }
    rig.host.release();
    assert_eq!(rig.step(), 4 * SECOND);

    // Up for 8 s: it starts from 1 s again.
    rig.step();
    let up_at = rig.now - POLL_NS;
    while rig.now < up_at + BACKOFF_CAP_NS {
        rig.step();
    }
    rig.step();
    rig.host.release();
    assert_eq!(rig.step(), SECOND);
}

#[test]
fn a_reset_or_a_pulled_cable_needs_no_new_enable() {
    let mut rig = Rig::new();
    rig.step();
    assert!(!rig.driver.configured());
    rig.host.event(UsbFunctionEvent::Configured);
    rig.step();
    assert!(rig.driver.configured());
    rig.host.event(UsbFunctionEvent::Suspended);
    rig.host.event(UsbFunctionEvent::Resumed);
    rig.host.event(UsbFunctionEvent::Deconfigured);
    assert_eq!(rig.step(), POLL_NS);
    assert!(!rig.driver.configured());
    assert_eq!(rig.health(), Health::Ok, "no host is not a fault");
    rig.host.event(UsbFunctionEvent::Configured);
    rig.step();
    assert!(rig.driver.configured());
    assert_eq!(rig.host.enables(), 1);
}

#[test]
fn every_control_request_is_refused_with_a_stall() {
    let mut rig = Rig::new();
    rig.step();
    let request = |request_type, request| ControlSetup {
        request_type,
        request,
        value: 0,
        index: 0,
    };
    // To the host, from the host with data, and with no data stage.
    let requests = [request(0xc0, 1), request(0x40, 2), request(0x21, 3)];
    for (setup, length) in requests.into_iter().zip([8, 4, 0]) {
        rig.host.request(setup, length, &[1, 2, 3, 4]);
    }
    assert_eq!(rig.step(), POLL_NS);
    assert_eq!(
        rig.host.answers(),
        requests.map(|setup| (setup, ControlAnswer::Stalled))
    );
    assert_eq!(rig.health(), Health::Ok);
}

#[test]
fn a_flood_of_requests_is_dealt_with_a_bounded_number_per_tick() {
    let mut rig = Rig::new();
    rig.step();
    let setup = ControlSetup {
        request_type: 0xc0,
        request: 1,
        value: 0,
        index: 0,
    };
    for _ in 0..40 {
        rig.host.request(setup, 1, &[]);
    }
    rig.step();
    assert_eq!(rig.host.answers().len(), EVENTS_PER_TICK);
    rig.step();
    rig.step();
    assert_eq!(rig.host.answers().len(), 40);
}

#[test]
fn a_driver_that_goes_takes_the_gadget_down() {
    let mut rig = Rig::new();
    rig.step();
    assert!(rig.host.descriptors().is_some());
    let host = rig.host.clone();
    drop(rig);
    assert_eq!(host.descriptors(), None);
}

#[test]
fn keeping_the_gadget_up_allocates_nothing() {
    let mut rig = Rig::new();
    rig.step();
    rig.host.event(UsbFunctionEvent::Configured);
    let ((), count) = allocations(|| {
        for _ in 0..1000 {
            rig.step();
        }
    });
    assert_eq!(count, 0);
    assert_eq!(rig.health(), Health::Ok);
}

#[test]
fn the_functions_are_listed_with_ffs_first_and_the_others_in_file_order() {
    use crate::description::functions;
    use lemnos_hal::usb_descriptors::GadgetFunction::{Acm, Ffs, Ncm};

    let ncm = "function ncm host=02:00:00:00:00:01 device=02:00:00:00:00:02\n";
    let interfaces = "interface 0xff 0 0 orion-link\nendpoint in bulk\nendpoint out bulk\n";
    let listed = |text: &str| functions(text, None).map(|f| f.as_slice().to_vec());
    let as_ncm = Ncm {
        host: [2, 0, 0, 0, 0, 1],
        device: [2, 0, 0, 0, 0, 2],
        ifname: None,
    };

    // No `function` line: FunctionFS alone.
    assert_eq!(listed(interfaces), Ok(std::vec![Ffs]));
    // `ffs` listed after the others is still first; the others keep the order they were given.
    assert_eq!(
        listed(&std::format!(
            "function acm\n{ncm}function ffs\n{interfaces}"
        )),
        Ok(std::vec![Ffs, Acm, as_ncm])
    );
    // `ffs` not listed but the others are: FunctionFS still comes first.
    assert_eq!(
        listed(&std::format!("{ncm}{interfaces}")),
        Ok(std::vec![Ffs, as_ncm])
    );
    // The MAC words may come in either order.
    assert!(
        listed(&std::format!(
            "function ncm device=02:00:00:00:00:02 host=02:00:00:00:00:01\n{interfaces}"
        ))
        .is_ok()
    );
}

#[test]
fn a_function_line_that_is_not_one_of_the_three_is_refused_by_its_line() {
    use crate::description::functions;
    let interfaces = "interface 0xff 0 0\nendpoint in bulk\nendpoint out bulk\n";
    let fault = |line: &str| functions(&std::format!("{line}\n{interfaces}"), None).err();
    let good_ncm = "ncm host=02:00:00:00:00:01 device=02:00:00:00:00:02";
    assert_eq!(fault("function"), Some(Fault::Function(1)));
    assert_eq!(fault("function rndis"), Some(Fault::Function(1)));
    assert_eq!(fault("function ffs extra"), Some(Fault::Function(1)));
    assert_eq!(fault("function acm 1"), Some(Fault::Function(1)));
    assert_eq!(fault("function ncm"), Some(Fault::Function(1)));
    assert_eq!(
        fault("function ncm host=02:00:00:00:00:01"),
        Some(Fault::Function(1))
    );
    assert_eq!(
        fault("function ncm host=02:00:00:00:00:01 device=02:00:00:00:00:01"),
        Some(Fault::Function(1)),
        "one address for both sides"
    );
    // A multicast address (low bit of the first octet set) is refused.
    assert_eq!(
        fault("function ncm host=03:00:00:00:00:01 device=02:00:00:00:00:02"),
        Some(Fault::Function(1))
    );
    assert_eq!(
        fault("function ncm host=02:00:00:00:00 device=02:00:00:00:00:02"),
        Some(Fault::Function(1))
    );
    assert_eq!(
        fault("function ncm host=2:0:0:0:0:1 device=02:00:00:00:00:02"),
        Some(Fault::Function(1))
    );
    assert_eq!(
        fault("function ncm host=02:00:00:00:00:01 host=02:00:00:00:00:03"),
        Some(Fault::Function(1))
    );
    assert_eq!(
        fault("function ncm device=02:00:00:00:00:02 derive"),
        Some(Fault::Function(1))
    );
    assert_eq!(
        fault("function acm\nfunction acm"),
        Some(Fault::Function(2))
    );
    assert_eq!(
        fault(&std::format!("function {good_ncm}\nfunction {good_ncm}")),
        Some(Fault::Function(2)),
        "one NCM"
    );
    assert_eq!(
        functions(&std::format!("function {good_ncm}\n{interfaces}"), None)
            .map(|f| f.as_slice().len()),
        Ok(2)
    );
}

/// Binds on the board `board`, with the given settings.
fn bind_on(
    function: &MockUsbFunction,
    entries: &[(&str, ConfigValue<'_>)],
    board: &str,
) -> Result<UsbGadget> {
    let mut board_handle = Board(Some(function.clone()));
    let mut resources = Resources::new(&mut board_handle, None);
    UsbGadget::bind(&mut resources, &Config::new(entries).on_board(board))
}

/// UTF-16LE, as a string descriptor holds it.
fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn the_serial_string_is_the_explicit_one_or_else_the_board_serial() {
    let board = "0123456789abcdef";
    let with = |extra: &[(&'static str, ConfigValue<'static>)]| {
        let mut entries: Vec<(&str, ConfigValue<'_>)> = settings_without_serial();
        entries.extend_from_slice(extra);
        settings_of(&entries, "alpha")
    };
    // No serial given: the board's serial is the string.
    let (set, _) = with(&[("board_serial", ConfigValue::Str(board))]).unwrap();
    assert!(contains(set.bytes(), &utf16(board)));
    // An explicit serial overrides it.
    let (set, _) = with(&[
        ("board_serial", ConfigValue::Str(board)),
        ("serial", ConfigValue::Str("S1")),
    ])
    .unwrap();
    assert!(contains(set.bytes(), &utf16("S1")));
    assert!(!contains(set.bytes(), &utf16(board)));
    // Neither: no serial string, and health says so once the gadget is up.
    let (set, _) = with(&[]).unwrap();
    assert!(!contains(set.bytes(), &utf16(board)));
    let function = MockUsbFunction::new();
    let mut driver = bind_on(&function, &settings_without_serial(), "alpha")
        .map_err(|_| ())
        .unwrap();
    Driver::tick(&mut driver, 0);
    assert_eq!(Lifecycle::health(&driver), Health::Degraded(NO_SERIAL));
    // The explicit serial is in the set and health is `Ok`.
    let mut entries: Vec<(&str, ConfigValue<'_>)> = settings_without_serial();
    entries.push(("serial", ConfigValue::Str("S1")));
    let function = MockUsbFunction::new();
    let mut driver = bind_on(&function, &entries, "alpha")
        .map_err(|_| ())
        .unwrap();
    Driver::tick(&mut driver, 0);
    assert_eq!(Lifecycle::health(&driver), Health::Ok);
    // A board serial that could not be read says so in its own words.
    let mut entries: Vec<(&str, ConfigValue<'_>)> = settings_without_serial();
    entries.push(("board_serial_error", ConfigValue::Str("no serial source")));
    let function = MockUsbFunction::new();
    let mut driver = bind_on(&function, &entries, "alpha")
        .map_err(|_| ())
        .unwrap();
    Driver::tick(&mut driver, 0);
    assert_eq!(Lifecycle::health(&driver), Health::Degraded(NO_SERIAL_READ));
}

/// The settings for the board `board`, through the same function the daemon's root phase uses.
fn settings_of(
    entries: &[(&str, ConfigValue<'_>)],
    board: &str,
) -> Result<(Set, crate::description::Functions)> {
    crate::settings(&Config::new(entries).on_board(board))
}

#[test]
fn derive_takes_the_addresses_from_the_board_identity_and_fails_without_one() {
    use crate::derive::macs;
    let interfaces = std::format!("function ncm derive\n{INTERFACES}");
    let mut entries: Vec<(&str, ConfigValue<'_>)> = settings();
    entries.retain(|(key, _)| *key != "interfaces");
    entries.push(("interfaces", ConfigValue::Str(&interfaces)));

    // With the board's identity: the NCM function carries the derived pair.
    let mut with_serial = entries.clone();
    with_serial.push(("board_serial", ConfigValue::Str("00000001")));
    let (_, functions) = settings_of(&with_serial, "alpha").unwrap();
    let pair = macs("alpha", "00000001").unwrap();
    assert!(functions.as_slice().contains(&GadgetFunction::Ncm {
        host: pair.host,
        device: pair.device,
        ifname: None,
    }));
    assert_ne!(pair.host, pair.device);

    // Without a serial, a serial outside the format, or no board id: the setting is refused.
    assert_eq!(
        settings_of(&entries, "alpha").map(|_| ()).unwrap_err(),
        Status::INVALID_ARGUMENT
    );
    let mut bad = entries.clone();
    bad.push(("board_serial", ConfigValue::Str("0000001G")));
    assert_eq!(
        settings_of(&bad, "alpha").map(|_| ()).unwrap_err(),
        Status::INVALID_ARGUMENT
    );
    assert_eq!(
        settings_of(&with_serial, "").map(|_| ()).unwrap_err(),
        Status::INVALID_ARGUMENT
    );
    // And the bind refuses it, so nothing is enabled.
    let function = MockUsbFunction::new();
    assert_eq!(
        bind_on(&function, &entries, "alpha")
            .map(|_| ())
            .unwrap_err(),
        Status::INVALID_ARGUMENT
    );
    assert_eq!(function.enables(), 0);
}

/// A rig whose function lends a socket pair for each endpoint file.
#[cfg(unix)]
fn lending_rig() -> Rig {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    let (out, _host_out) = UnixStream::pair().unwrap();
    let (input, _host_in) = UnixStream::pair().unwrap();
    let host = MockUsbFunction::with_endpoint_files(OwnedFd::from(out), OwnedFd::from(input));
    let driver = bind(&host, &settings()).map_err(|_| ()).expect("bind");
    Rig {
        driver,
        host,
        now: 0,
    }
}

#[cfg(unix)]
fn lent(rig: &mut Rig, caller: u64) -> lemnos_core::Result<()> {
    Driver::endpoints(&mut rig.driver, CallerId(caller)).map(|_| ())
}

#[cfg(unix)]
#[test]
fn the_endpoints_are_lent_once_the_gadget_is_up_and_not_before() {
    let mut rig = lending_rig();
    assert_eq!(lent(&mut rig, 1), Err(Status::NO_DEVICE));
    rig.step();
    assert_eq!(lent(&mut rig, 1), Ok(()));
}

#[cfg(unix)]
#[test]
fn one_caller_holds_the_endpoints_and_another_is_refused_busy() {
    let mut rig = lending_rig();
    rig.step();
    assert_eq!(lent(&mut rig, 1), Ok(()));
    assert_eq!(lent(&mut rig, 2), Err(Status::BUSY));
    // The holder asking again is given them again.
    assert_eq!(lent(&mut rig, 1), Ok(()));
    assert_eq!(lent(&mut rig, 2), Err(Status::BUSY));
}

#[cfg(unix)]
#[test]
fn the_hold_is_freed_when_its_caller_goes() {
    let mut rig = lending_rig();
    rig.step();
    assert_eq!(lent(&mut rig, 1), Ok(()));
    Lifecycle::caller_gone(&mut rig.driver, CallerId(2));
    assert_eq!(
        lent(&mut rig, 2),
        Err(Status::BUSY),
        "a stranger going frees nothing"
    );
    Lifecycle::caller_gone(&mut rig.driver, CallerId(1));
    assert_eq!(lent(&mut rig, 2), Ok(()));
}

#[cfg(unix)]
#[test]
fn only_the_holder_can_give_the_endpoints_up() {
    let mut rig = lending_rig();
    rig.step();
    assert_eq!(lent(&mut rig, 1), Ok(()));
    assert_eq!(
        Driver::release_endpoints(&mut rig.driver, CallerId(2)),
        Err(Status::NOT_FOUND)
    );
    assert_eq!(
        Driver::release_endpoints(&mut rig.driver, CallerId(1)),
        Ok(())
    );
    assert_eq!(
        Driver::release_endpoints(&mut rig.driver, CallerId(1)),
        Err(Status::NOT_FOUND),
        "nothing left to give up"
    );
    assert_eq!(lent(&mut rig, 2), Ok(()));
}

#[cfg(unix)]
#[test]
fn the_hold_survives_a_let_go_until_its_holder_gives_it_up() {
    let mut rig = lending_rig();
    rig.step();
    assert_eq!(lent(&mut rig, 1), Ok(()));
    rig.host.release();
    rig.step();
    assert_eq!(rig.health(), Health::Failed(LET_GO));
    // Down for now: nobody is lent the endpoints, the holder keeps its hold, and a stranger is
    // refused as before.
    assert_eq!(lent(&mut rig, 2), Err(Status::BUSY));
    // Enabled again: the holder gives the hold up, and then a new hold is possible for anyone.
    rig.step();
    assert_eq!(
        Driver::release_endpoints(&mut rig.driver, CallerId(1)),
        Ok(())
    );
    assert_eq!(lent(&mut rig, 2), Ok(()));
}

#[test]
fn an_ncm_line_may_name_its_interface_with_a_template() {
    use crate::description::{Fault, functions};
    use lemnos_hal::usb_descriptors::GadgetFunction::{Ffs, Ncm};
    use lemnos_hal::usb_descriptors::IfName;

    let ncm = |line: &str| {
        functions(
            &std::format!("{line}\ninterface 0xff 0 0 link\nendpoint in bulk\n"),
            None,
        )
        .map(|f| f.as_slice().to_vec())
    };
    let named = IfName::new("lemnos%d");
    assert_eq!(
        ncm("function ncm host=02:00:00:00:00:01 device=02:00:00:00:00:02 ifname=lemnos%d"),
        Ok(std::vec![
            Ffs,
            Ncm {
                host: [2, 0, 0, 0, 0, 1],
                device: [2, 0, 0, 0, 0, 2],
                ifname: named,
            }
        ])
    );
    // Any order of the words, and the name may come first.
    assert_eq!(
        ncm("function ncm ifname=lemnos%d device=02:00:00:00:00:02 host=02:00:00:00:00:01"),
        ncm("function ncm host=02:00:00:00:00:01 device=02:00:00:00:00:02 ifname=lemnos%d")
    );
    // A name on a line without one is unchanged: no name, the kernel's `usb<N>`.
    assert_eq!(
        ncm("function ncm host=02:00:00:00:00:01 device=02:00:00:00:00:02"),
        Ok(std::vec![
            Ffs,
            Ncm {
                host: [2, 0, 0, 0, 0, 1],
                device: [2, 0, 0, 0, 0, 2],
                ifname: None,
            }
        ])
    );
    // A bad name is refused by its line, as a name.
    assert_eq!(
        ncm("function ncm host=02:00:00:00:00:01 device=02:00:00:00:00:02 ifname=lemnos0"),
        Err(Fault::InterfaceName(1))
    );
    // A name is given once, and `derive` takes it too.
    for bad in [
        "function ncm host=02:00:00:00:00:01 device=02:00:00:00:00:02 ifname=a%d ifname=b%d",
        "function ncm host=02:00:00:00:00:01 device=02:00:00:00:00:02 ifname=",
        "function ncm derive host=02:00:00:00:00:01",
    ] {
        assert!(ncm(bad).is_err(), "{bad}");
    }
    let pair = pair_for_alpha();
    let derived = functions(
        "function ncm derive ifname=usb%d\ninterface 0xff 0 0 link\nendpoint in bulk\n",
        Some(pair),
    )
    .map(|f| f.as_slice().to_vec());
    assert_eq!(
        derived,
        Ok(std::vec![
            Ffs,
            Ncm {
                host: pair.host,
                device: pair.device,
                ifname: IfName::new("usb%d"),
            }
        ])
    );
}

/// The addresses of a fixed identity, for the name tests.
fn pair_for_alpha() -> crate::derive::Macs {
    crate::derive::macs("alpha", "00000001").unwrap()
}

#[test]
fn the_ncm_interface_is_described_once_the_kernel_has_named_it() {
    use lemnos_contract_usb_function::UsbFunction as Contract;
    use lemnos_core::{Description, Status};

    let host = MockUsbFunction::new();
    host.set_interface_name(Some("lemnos0"));
    let mut entries = settings();
    entries.retain(|(k, _)| *k != "interfaces");
    entries.push((
        "interfaces",
        ConfigValue::Str(
            "function ffs\ninterface 0xff 0 0 link\nendpoint in bulk\nendpoint out bulk\n\
             function ncm host=02:00:00:00:00:01 device=02:00:00:00:00:02 ifname=lemnos%d\n",
        ),
    ));
    let mut driver = bind(&host, &entries).expect("bind");

    // Not bound yet: nothing is named.
    let mut described = Description::new();
    Driver::describe(&driver, &mut described);
    assert!(described.find("ifname").is_none());
    let mut buffer = [0u8; 16];
    assert_eq!(
        Contract::ifname(&mut driver, &mut buffer),
        Err(Status::NOT_FOUND)
    );

    // Bound: the name the kernel gave is described and readable through the contract.
    Driver::tick(&mut driver, 0).expect("the first tick enables it");
    let mut described = Description::new();
    Driver::describe(&driver, &mut described);
    assert!(described.find("ifname").is_some());
    assert_eq!(Contract::ifname(&mut driver, &mut buffer), Ok(7));
    assert_eq!(&buffer[..7], b"lemnos0");
    assert_eq!(
        Contract::ifname(&mut driver, &mut [0u8; 3]),
        Err(Status::BUFFER_TOO_SMALL)
    );
}

#[test]
fn a_gadget_without_an_ncm_function_describes_no_interface_name() {
    use lemnos_core::Description;

    let host = MockUsbFunction::new();
    // The backend names nothing when the gadget has no NCM function.
    let mut driver = bind(&host, &settings()).expect("bind");
    Driver::tick(&mut driver, 0).expect("the first tick enables it");
    let mut described = Description::new();
    Driver::describe(&driver, &mut described);
    assert!(described.find("ifname").is_none());
}

/// `interfaces` is the text in every build: the same settings are accepted and give one set.
#[test]
fn the_interfaces_text_means_the_same_in_every_build() {
    let entries = settings();
    assert_eq!(crate::check(&Config::new(&entries)), Ok(()));
    let (set, _) = settings_of(&entries, "").expect("the text is accepted");
    assert!(!set.bytes().is_empty());
}

/// Both interface keys name the interfaces twice: refused by name, in every build.
#[test]
fn both_interface_keys_are_refused_by_name() {
    let mut entries = settings();
    entries.push((
        "interfaces_file",
        ConfigValue::Str("/etc/lemnos/interfaces.txt"),
    ));
    assert_eq!(
        crate::check(&Config::new(&entries)).map_err(|error| (error.key, error.reason)),
        Err((
            "interfaces_file",
            "is given with interfaces: give one of them, not both"
        ))
    );
    assert_eq!(
        settings_of(&entries, "").err(),
        Some(Status::INVALID_ARGUMENT)
    );
}

/// `interfaces_file` is a path: without a filesystem it is refused by name, never read as text.
#[cfg(not(feature = "std"))]
#[test]
fn an_interfaces_file_is_refused_by_name_without_a_filesystem() {
    let entries = [
        ("vendor", ConfigValue::Int(0x1209)),
        ("product", ConfigValue::Int(1)),
        ("interfaces_file", ConfigValue::Str(INTERFACES)),
    ];
    assert_eq!(
        crate::check(&Config::new(&entries)).map_err(|error| (error.key, error.reason)),
        Err((
            "interfaces_file",
            "needs a build with a filesystem: give the interfaces as interfaces"
        ))
    );
    assert_eq!(
        settings_of(&entries, "").err(),
        Some(Status::INVALID_ARGUMENT)
    );
}

/// With a filesystem, `interfaces_file` reads the file once, and gives the set the text gives.
#[cfg(feature = "std")]
#[test]
fn an_interfaces_file_gives_the_set_its_text_gives() {
    let path = std::env::temp_dir().join(std::format!("lemnos-interfaces-{}", std::process::id()));
    std::fs::write(&path, INTERFACES).unwrap();
    let text = path.to_str().unwrap();
    let mut from_file = settings();
    from_file.retain(|(key, _)| *key != "interfaces");
    from_file.push(("interfaces_file", ConfigValue::Str(text)));
    let (file_set, _) = settings_of(&from_file, "").expect("the file is read");
    let (text_set, _) = settings_of(&settings(), "").expect("the text is accepted");
    let _ = std::fs::remove_file(&path);
    assert_eq!(file_set.bytes(), text_set.bytes());
}

#[test]
fn the_link_is_the_first_function_whatever_order_the_board_lists_them_in() {
    use lemnos_hal::usb_descriptors::GadgetFunction::{Acm, Ffs, Ncm};
    use std::format;
    let ncm = "function ncm host=02:00:00:00:00:01 device=02:00:00:00:00:02 ifname=lemnos%d\n";
    let link = "function ffs\ninterface 0xff 0 0 link\nendpoint in bulk\nendpoint out bulk\n";
    let as_ncm = Ncm {
        host: [2, 0, 0, 0, 0, 1],
        device: [2, 0, 0, 0, 0, 2],
        ifname: Some(lemnos_hal::usb_descriptors::IfName::new("lemnos%d").unwrap()),
    };
    // The link first; the others after it in the order the file gives them.
    for (listed, others) in [
        (format!("{ncm}{link}function acm\n"), [as_ncm, Acm]),
        (format!("function acm\n{link}{ncm}"), [Acm, as_ncm]),
        (format!("{link}{ncm}function acm\n"), [as_ncm, Acm]),
    ] {
        let functions = description::functions(&listed, None).unwrap();
        assert_eq!(
            functions.as_slice(),
            &[Ffs, others[0], others[1]],
            "{listed}"
        );
    }
}

#[test]
fn the_link_has_no_interface_association_so_it_is_its_own_interface() {
    // An interface association descriptor is type 11 (0x0b). Nothing of the link's may carry one:
    // the composite groups an interface behind an association only when a descriptor says so.
    let interfaces = "function ffs\ninterface 0xff 0 0 link\nendpoint in bulk\nendpoint out bulk\n";
    let set = build(&identity_for_layout(), interfaces).unwrap();
    let bytes = set.bytes();
    let mut at = 0;
    while at + 2 <= bytes.len() {
        let len = usize::from(bytes[at]);
        assert!(len >= 2 && at + len <= bytes.len());
        assert_ne!(
            bytes[at + 1],
            11,
            "no interface association in the link's descriptors"
        );
        at += len;
    }
    // The link's interface is number 0.
    assert!(bytes.windows(4).any(|w| w == [9, 4, 0, 0]));
}

fn identity_for_layout() -> Identity<'static> {
    Identity {
        vendor: crate::LINK_VENDOR,
        product: crate::LINK_PRODUCT,
        release: crate::revision::RELEASE,
        class: 0xef,
        subclass: 0x02,
        protocol: 0x01,
        manufacturer: None,
        product_name: None,
        serial: None,
        max_power_ma: 100,
        self_powered: false,
    }
}
