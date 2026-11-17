//! A backend resource name a board names that the SoC map does not have is refused, by the device
//! or line that names it, the name, and what the SoC has (`lemnos_devtree::resources::refusals`).
//! The forms the map describes are checked; the rest are left to the bind.

use lemnos_devtree::SocMap;
use lemnos_devtree::resources::refusals;
use lemnos_host::board;

const RK3588_MAP: &str = include_str!("../../../socs/rk3588.toml");

/// A board on the RK3588 with one device, naming `name` in its `bus`.
fn with_bus(name: &str) -> String {
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\nsoc = \"rk3588\"\n\
         [[devices]]\nid = \"imu\"\ndriver = \"bmi088\"\nbus = \"{name}\"\n"
    )
}

/// A board on the RK3588 with one device, naming `name` in its `uses` under `need`.
fn with_use(need: &str, name: &str) -> String {
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\nsoc = \"rk3588\"\n\
         [[devices]]\nid = \"d\"\ndriver = \"x\"\nuses = {{ {need} = \"{name}\" }}\n"
    )
}

fn refused(text: &str) -> Vec<String> {
    let board = board::parse(text).expect("the board parses");
    let map = SocMap::parse(RK3588_MAP).expect("the map parses");
    refusals(&board, &map)
}

#[test]
fn resource_names_a_bus_the_soc_has_passes() {
    assert!(refused(&with_bus("i2c-0")).is_empty());
    assert!(refused(&with_bus("spidev0.0")).is_empty());
    assert!(refused(&with_use("function", "udc:fc000000.usb")).is_empty());
}

#[test]
fn resource_names_a_missing_i2c_bus_is_refused_by_name() {
    let found = refused(&with_bus("i2c-10"));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(
        found[0],
        "device \"imu\": i2c-10: the rk3588 SoC has no I2C bus i2c-10 (it has: i2c0, i2c1, i2c2, i2c3)"
    );
}

#[test]
fn resource_names_a_missing_spi_bus_udc_watchdog_pwm_and_gpio_chip_are_refused() {
    let spi = refused(&with_bus("spidev9.0"));
    assert!(
        spi[0].contains("no SPI bus spidev9.0 (it has: spi0, spi1, spi2, spi3, spi4)"),
        "{spi:?}"
    );
    let udc = refused(&with_use("function", "udc:nope.usb"));
    assert!(
        udc[0].contains("no UDC nope.usb (it has: fc000000.usb)"),
        "{udc:?}"
    );
    let wdt = refused(&with_use("watchdog", "watchdog:/watchdog@0"));
    assert!(
        wdt[0].contains("no watchdog node /watchdog@0 (it has: /watchdog@feaf0000)"),
        "{wdt:?}"
    );
    let pwm = refused(&with_use("pwm", "pwm:/pwm@0:1"));
    assert!(pwm[0].contains("no PWM node /pwm@0 (it has:"), "{pwm:?}");
    let gpio = refused(&with_bus("gpio:nochip:3"));
    assert!(
        gpio[0].contains("no GPIO chip labelled \"nochip\""),
        "{gpio:?}"
    );
}

#[test]
fn resource_names_a_line_the_soc_lacks_is_refused_by_name() {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\nsoc = \"rk3588\"\n\
                [[lines]]\nline = \"gpio:nochip:4\"\nreason = \"test\"\n";
    let found = refused(text);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].starts_with("[[lines]] gpio:nochip:4: "),
        "{found:?}"
    );
}

#[test]
fn resource_names_forms_the_map_does_not_describe_are_not_checked() {
    for name in [
        "usb:1-1.4",
        "pio-i2c:sda=8,scl=7",
        "dma:a/b",
        "gpiochip0:17",
        "pwmchip0:1",
        "watchdog0",
        "i2c-x",
    ] {
        assert!(refused(&with_bus(name)).is_empty(), "{name} was checked");
    }
}
