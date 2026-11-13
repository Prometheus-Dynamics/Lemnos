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
        ("interfaces", ConfigValue::Str(INTERFACES)),
    ]
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
    let refused: [(&str, Option<ConfigValue<'static>>); 14] = [
        ("vendor", None),
        ("product", None),
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
