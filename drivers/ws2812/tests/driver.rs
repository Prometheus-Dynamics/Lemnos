//! The driver bound to a simulated SPI bus, reached the way a host reaches it.

use std::sync::{Arc, Mutex};

use lemnos_contract_pixels::{MAX_LOCAL_LEDS, Pixels};
use lemnos_core::{
    Bound, Caller, Config, ConfigValue, Description, Driver, ErrorKind, Health, LOCAL_ARGS,
    LocalCaller, MethodId, Provider, Reply, Resources, Spi, Status, Tag,
};
use lemnos_driver_ws2812::{Ws2812, encode_spi_byte};
use lemnos_hal::spi::{self, Operation, SpiDevice};
use lemnos_sim::SimSpi;

/// The simulated bus, failing the next `failures` transactions with `Busy`.
#[derive(Clone)]
struct Flaky {
    inner: SimSpi,
    failures: Arc<Mutex<usize>>,
}

impl Flaky {
    fn new(inner: SimSpi) -> Self {
        Self {
            inner,
            failures: Arc::new(Mutex::new(0)),
        }
    }

    fn fail_next(&self, n: usize) {
        *self.failures.lock().unwrap() = n;
    }
}

impl spi::ErrorType for Flaky {
    type Error = ErrorKind;
}

impl SpiDevice for Flaky {
    fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), ErrorKind> {
        let mut failures = self.failures.lock().unwrap();
        if *failures > 0 {
            *failures -= 1;
            return Err(ErrorKind::Busy);
        }
        drop(failures);
        self.inner.transaction(operations)
    }
}

/// Hands the driver its one SPI device.
struct Board(Option<Spi>);

impl Provider for Board {
    fn provided(&self, need: &str) -> bool {
        need == "spi"
    }

    fn device(&mut self, _need: &str, _tag: Tag) -> lemnos_core::Result<LocalCaller> {
        Err(Status::NOT_FOUND)
    }

    fn spi(&mut self, _need: &str) -> lemnos_core::Result<Spi> {
        self.0.take().ok_or(Status::NOT_FOUND)
    }
}

fn bind(entries: &[(&str, ConfigValue<'_>)], bus: Flaky) -> lemnos_core::Result<Ws2812> {
    let mut board = Board(Some(Spi(Box::new(bus))));
    let mut res = Resources::new(&mut board, None);
    Ws2812::bind(&mut res, &Config::new(entries))
}

/// Reaches the driver the way a host does: argument bytes in, reply bytes out. The argument buffer
/// is the size a local caller has, so anything bigger than a local call fails here.
struct Direct<'a> {
    driver: &'a mut Ws2812,
    args: [u8; LOCAL_ARGS],
}

impl Caller for Direct<'_> {
    fn args(&mut self) -> &mut [u8] {
        &mut self.args
    }

    fn call(
        &mut self,
        method: u16,
        args_len: usize,
        reply: &mut [u8],
    ) -> lemnos_core::Result<usize> {
        let mut writer = Reply::new(reply);
        let id = MethodId {
            contract: 0,
            method,
        };
        let status = self
            .driver
            .dispatch(id, &self.args[..args_len], &mut writer);
        if status.is_ok() {
            Ok(writer.len())
        } else {
            Err(status)
        }
    }
}

fn client(driver: &mut Ws2812) -> Bound<dyn Pixels, Direct<'_>> {
    Bound::new(Direct {
        driver,
        args: [0; LOCAL_ARGS],
    })
}

/// `leds` LEDs off, as RGB SPI bytes: 3 SPI bytes per data byte, 3 data bytes per LED.
fn led_off(leds: usize) -> Vec<u8> {
    (0..leds * 3).flat_map(|_| encode_spi_byte(0)).collect()
}

/// One RGB LED with the given colour, as SPI bytes in G, R, B order.
fn rgb_led(r: u8, g: u8, b: u8) -> Vec<u8> {
    [g, r, b].into_iter().flat_map(encode_spi_byte).collect()
}

/// One RGB LED at full red.
fn red_led() -> Vec<u8> {
    rgb_led(0xff, 0, 0)
}

const RED: [u8; 4] = [0xff, 0, 0, 0];

fn refused(entries: &[(&str, ConfigValue<'_>)]) -> Option<Status> {
    bind(entries, Flaky::new(SimSpi::new())).err()
}

#[test]
fn binds_and_reports_its_pixel_count() {
    let mut driver = bind(&[("count", ConfigValue::Int(4))], Flaky::new(SimSpi::new())).unwrap();
    let mut out = Description::new();
    Driver::describe(&driver, &mut out);
    assert!(out.find("pixels").is_some());
    assert_eq!(Driver::health(&driver), Health::Ok);
    assert_eq!(client(&mut driver).pixel_count(), Ok(4));
}

#[test]
fn start_turns_the_strip_off_in_one_transaction() {
    let sim = SimSpi::new();
    let _driver = bind(&[("count", ConfigValue::Int(2))], Flaky::new(sim.clone())).unwrap();
    assert_eq!(sim.written(), vec![led_off(2)]);
}

#[test]
fn set_sends_nothing_and_show_sends_the_stored_frame_once() {
    let sim = SimSpi::new();
    let mut driver = bind(&[("count", ConfigValue::Int(2))], Flaky::new(sim.clone())).unwrap();
    client(&mut driver).set(0, &RED).unwrap();
    assert_eq!(sim.written().len(), 1, "set must not touch the bus");
    client(&mut driver).show().unwrap();
    let written = sim.written();
    assert_eq!(written.len(), 2);
    // Red on LED 0; LED 1 was never set, so it is off.
    let mut expected = red_led();
    expected.extend(led_off(1));
    assert_eq!(written[1], expected);
}

#[test]
fn a_stored_frame_keeps_leds_that_a_later_set_does_not_touch() {
    let sim = SimSpi::new();
    let mut driver = bind(&[("count", ConfigValue::Int(2))], Flaky::new(sim.clone())).unwrap();
    client(&mut driver).set(0, &RED).unwrap();
    client(&mut driver).set(1, &[0, 0, 0xff, 0]).unwrap();
    client(&mut driver).show().unwrap();
    let mut expected = red_led();
    expected.extend(rgb_led(0, 0, 0xff));
    assert_eq!(sim.written()[1], expected);
}

#[test]
fn set_ignores_leds_past_the_end_and_drops_a_partial_led() {
    let sim = SimSpi::new();
    let mut driver = bind(&[("count", ConfigValue::Int(1))], Flaky::new(sim.clone())).unwrap();
    // Two whole LEDs for a one-LED strip, then two stray bytes.
    client(&mut driver)
        .set(0, &[0xff, 0, 0, 0, 0, 0xff, 0, 0, 1, 2])
        .unwrap();
    // An offset past the end stores nothing.
    client(&mut driver).set(5, &[0xff, 0, 0, 0]).unwrap();
    client(&mut driver).show().unwrap();
    assert_eq!(sim.written()[1], red_led());
}

#[test]
fn offset_and_direction_place_the_led_on_the_bus() {
    let sim = SimSpi::new();
    let entries = [
        ("count", ConfigValue::Int(3)),
        ("offset", ConfigValue::Int(1)),
        ("direction", ConfigValue::Str("ccw")),
    ];
    let mut driver = bind(&entries, Flaky::new(sim.clone())).unwrap();
    // Logical 0 lands on physical 1, so the middle slot is red and the others are off.
    client(&mut driver).set(0, &RED).unwrap();
    client(&mut driver).show().unwrap();
    let mut expected = led_off(1);
    expected.extend(red_led());
    expected.extend(led_off(1));
    assert_eq!(sim.written()[1], expected);
}

#[test]
fn a_24_led_frame_in_local_sized_chunks_is_one_transaction_on_show() {
    let sim = SimSpi::new();
    let mut driver = bind(&[("count", ConfigValue::Int(24))], Flaky::new(sim.clone())).unwrap();
    // Each LED's colour depends on its index, so a misplaced chunk shows.
    let frame: Vec<u8> = (0..24u8)
        .flat_map(|i| [i, 255 - i, i.wrapping_mul(7), 0])
        .collect();
    for (chunk, leds) in frame.chunks(MAX_LOCAL_LEDS * 4).enumerate() {
        client(&mut driver)
            .set((chunk * MAX_LOCAL_LEDS) as u16, leds)
            .unwrap();
    }
    assert_eq!(sim.written().len(), 1, "set must not touch the bus");
    client(&mut driver).show().unwrap();
    let written = sim.written();
    assert_eq!(written.len(), 2, "one transaction on show");
    let expected: Vec<u8> = (0..24u8)
        .flat_map(|i| rgb_led(i, 255 - i, i.wrapping_mul(7)))
        .collect();
    assert_eq!(written[1], expected);
}

#[test]
fn a_failed_show_is_reported_and_the_next_show_sends_the_same_frame() {
    let sim = SimSpi::new();
    let flaky = Flaky::new(sim.clone());
    let mut driver = bind(&[("count", ConfigValue::Int(1))], flaky.clone()).unwrap();
    client(&mut driver).set(0, &RED).unwrap();
    flaky.fail_next(1);
    assert_eq!(client(&mut driver).show(), Err(Status::BUSY));
    assert!(matches!(Driver::health(&driver), Health::Failed(_)));
    // The failed frame never reached the bus. The retry sends the same stored frame.
    client(&mut driver).show().unwrap();
    assert_eq!(Driver::health(&driver), Health::Ok);
    let written = sim.written();
    assert_eq!(written.len(), 2);
    assert_eq!(written[1], red_led());
}

#[test]
fn a_failed_start_write_does_not_fail_the_bind() {
    let flaky = Flaky::new(SimSpi::new());
    flaky.fail_next(1);
    let mut driver = bind(&[("count", ConfigValue::Int(2))], flaky).unwrap();
    assert!(matches!(Driver::health(&driver), Health::Failed(_)));
    client(&mut driver).set(0, &RED).unwrap();
    client(&mut driver).show().unwrap();
    assert_eq!(Driver::health(&driver), Health::Ok);
}

#[test]
fn bad_config_is_refused() {
    assert_eq!(refused(&[]), Some(Status::INVALID_ARGUMENT));
    assert_eq!(
        refused(&[("count", ConfigValue::Int(0))]),
        Some(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        refused(&[
            ("count", ConfigValue::Int(2)),
            ("wire", ConfigValue::Str("xyz")),
        ]),
        Some(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        refused(&[
            ("count", ConfigValue::Int(2)),
            ("direction", ConfigValue::Str("sideways")),
        ]),
        Some(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        refused(&[
            ("count", ConfigValue::Int(2)),
            ("brightness", ConfigValue::Int(300)),
        ]),
        Some(Status::INVALID_ARGUMENT)
    );
}
