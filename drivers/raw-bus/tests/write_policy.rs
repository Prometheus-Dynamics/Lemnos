//! The write policy, end to end: what the board file's `config` allows, and that a refused call never
//! reaches a bus. The policy is closed by default, so these tests start from nothing allowed and add
//! what each one needs.
//!
//! Each refusal is checked against a bus that is down (`all_buses_down`). A refused call returns
//! its refusal, and a call that reached the bus would return `BUSY` instead. Where it matters, the bus
//! counter is checked too.

mod common;

use common::{all_buses_down, bind, client, wired};
use lemnos_contract_raw_bus::RawBus as _;
use lemnos_core::{ConfigValue, Driver, Health, Status};
use lemnos_hal::raw::LineConfig;

const REFUSED: Status = Status::NOT_PERMITTED;

#[test]
fn with_no_settings_every_call_that_writes_is_refused_and_no_bus_is_reached() {
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &[]).unwrap();
    let before = wiring.i2c.transactions();
    all_buses_down(&wiring);

    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[0x20, 0x42], 0, &mut []),
        Err(REFUSED)
    );
    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[], 2, &mut [0; 2]),
        Err(REFUSED)
    );
    assert_eq!(
        client(&mut driver).i2c_transfer(0x18, &[1], 0, &mut []),
        Err(REFUSED)
    );
    assert_eq!(
        client(&mut driver).spi_transfer(&[1, 2], 0, &mut []),
        Err(REFUSED)
    );
    assert_eq!(client(&mut driver).line_set(true), Err(REFUSED));
    assert_eq!(client(&mut driver).pwm_set(1_000, 500, true), Err(REFUSED));
    assert_eq!(wiring.i2c.transactions(), before);
    assert!(wiring.spi.written().is_empty());
    assert!(wiring.pwm.history().is_empty());
    assert_eq!(
        Driver::health(&driver),
        Health::Ok,
        "refusals are not bus failures"
    );

    // Reading a line is not a write, and the line is bound, so it reaches the (down) bus.
    assert_eq!(client(&mut driver).line_get(), Err(Status::BUSY));
}

#[test]
fn with_no_settings_an_output_line_is_refused_at_bind_and_an_input_is_not() {
    let mut wiring = wired();
    assert_eq!(
        bind(
            &mut wiring.board,
            &[("line_direction", ConfigValue::Str("output"))]
        )
        .err(),
        Some(Status::INVALID_ARGUMENT)
    );
    assert!(wiring.line.configs().is_empty(), "the line was not touched");

    let mut wiring = wired();
    bind(
        &mut wiring.board,
        &[("line_direction", ConfigValue::Str("input"))],
    )
    .unwrap();
    assert_eq!(wiring.line.configs(), [LineConfig::input()]);
}

#[test]
fn an_allow_list_with_writes_off_permits_reads_and_no_writes() {
    let mut wiring = wired();
    let mut driver = bind(
        &mut wiring.board,
        &[("allow_i2c", ConfigValue::Str("0x50"))],
    )
    .unwrap();

    // A read of an allowed address reaches the bus.
    let before = wiring.i2c.transactions();
    let mut into = [0; 2];
    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[], 2, &mut into),
        Ok(2)
    );
    assert_eq!(wiring.i2c.transactions(), before + 1);

    all_buses_down(&wiring);
    // Writes to it are refused, alone or in a combined transfer, whatever the bus would do.
    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[0x20, 0x42], 0, &mut []),
        Err(REFUSED)
    );
    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[0x10], 2, &mut into),
        Err(REFUSED)
    );
    // A read of an address not on the list is refused too.
    assert_eq!(
        client(&mut driver).i2c_transfer(0x18, &[], 1, &mut [0; 1]),
        Err(REFUSED)
    );
    // The allowed read reaches the down bus.
    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[], 2, &mut into),
        Err(Status::BUSY)
    );
    assert_eq!(
        wiring.i2c.transactions(),
        before + 1,
        "refused transfers reached no bus"
    );
}

#[test]
fn writes_on_admits_writes_to_allowed_addresses_and_the_spi_line_and_pwm_the_board_bound() {
    let mut wiring = wired();
    let mut driver = bind(
        &mut wiring.board,
        &[
            ("writes", ConfigValue::Bool(true)),
            ("allow_i2c", ConfigValue::Str("0x50")),
            ("line_direction", ConfigValue::Str("output")),
        ],
    )
    .unwrap();
    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[0x20, 0x42], 0, &mut []),
        Ok(0)
    );
    assert_eq!(wiring.i2c.byte(0x50, 0x20), Some(0x42));
    assert_eq!(client(&mut driver).spi_transfer(&[0x9f], 0, &mut []), Ok(0));
    client(&mut driver).line_set(true).unwrap();
    assert_eq!(wiring.line.level(), Some(true));
    client(&mut driver).pwm_set(1_000, 500, true).unwrap();
    assert_eq!(wiring.pwm.history().len(), 1);

    // Writes still do not reach an address the list does not name.
    all_buses_down(&wiring);
    assert_eq!(
        client(&mut driver).i2c_transfer(0x18, &[0x20], 0, &mut []),
        Err(REFUSED)
    );
}

#[test]
fn with_writes_on_an_address_not_on_the_list_is_refused_for_reads_and_writes_alike() {
    let mut wiring = wired();
    let mut driver = bind(
        &mut wiring.board,
        &[
            ("writes", ConfigValue::Bool(true)),
            ("allow_i2c", ConfigValue::Str("0x50")),
        ],
    )
    .unwrap();
    all_buses_down(&wiring);
    assert_eq!(
        client(&mut driver).i2c_transfer(0x68, &[1], 0, &mut []),
        Err(REFUSED)
    );
    assert_eq!(
        client(&mut driver).i2c_transfer(0x68, &[], 1, &mut [0; 1]),
        Err(REFUSED)
    );
}

#[test]
fn the_allow_list_is_exact_across_the_bitmap_words() {
    let mut wiring = wired();
    let mut driver = bind(
        &mut wiring.board,
        &[
            ("writes", ConfigValue::Bool(true)),
            ("allow_i2c", ConfigValue::Str("0x3f, 0x40, 0x7f")),
        ],
    )
    .unwrap();
    all_buses_down(&wiring);
    // The listed addresses reach the (down) bus.
    for listed in [0x3f, 0x40, 0x7f] {
        assert_eq!(
            client(&mut driver).i2c_transfer(listed, &[1], 0, &mut []),
            Err(Status::BUSY),
            "{listed:#x} is listed"
        );
    }
    // Their neighbours do not.
    for unlisted in [0x3e, 0x41, 0x7e, 0x00] {
        assert_eq!(
            client(&mut driver).i2c_transfer(unlisted, &[1], 0, &mut []),
            Err(REFUSED),
            "{unlisted:#x} is not listed"
        );
    }
}

#[test]
fn the_list_takes_one_integer_or_decimal_and_hex_entries() {
    let mut wiring = wired();
    let mut driver = bind(
        &mut wiring.board,
        &[
            ("writes", ConfigValue::Bool(true)),
            ("allow_i2c", ConfigValue::Int(0x50)),
        ],
    )
    .unwrap();
    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[1], 0, &mut []),
        Ok(0)
    );

    // Decimal 104 is 0x68, and upper-case hex is accepted too.
    let mut wiring = wired();
    let mut driver = bind(
        &mut wiring.board,
        &[
            ("writes", ConfigValue::Bool(true)),
            ("allow_i2c", ConfigValue::Str("80, 104, 0X7F")),
        ],
    )
    .unwrap();
    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[1], 0, &mut []),
        Ok(0)
    );
    assert_eq!(
        client(&mut driver).i2c_transfer(0x68, &[1], 0, &mut []),
        Ok(0)
    );
    assert_eq!(
        client(&mut driver).i2c_transfer(0x7f, &[1], 0, &mut []),
        Ok(0)
    );
    assert_eq!(
        client(&mut driver).i2c_transfer(0x19, &[1], 0, &mut []),
        Err(REFUSED)
    );
}

#[test]
fn writes_off_refuses_each_write_on_a_bound_device_and_a_refusal_leaves_it_working() {
    let mut wiring = wired();
    let mut driver = bind(
        &mut wiring.board,
        &[
            ("allow_i2c", ConfigValue::Str("0x50")),
            ("line_direction", ConfigValue::Str("input")),
        ],
    )
    .unwrap();
    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[0x20, 0x42], 0, &mut []),
        Err(REFUSED)
    );
    assert_eq!(
        client(&mut driver).spi_transfer(&[1], 0, &mut []),
        Err(REFUSED)
    );
    assert_eq!(client(&mut driver).line_set(true), Err(REFUSED));
    assert_eq!(client(&mut driver).pwm_set(1_000, 500, true), Err(REFUSED));
    assert_eq!(
        Driver::health(&driver),
        Health::Ok,
        "refusals do not mark health"
    );
    assert!(wiring.i2c.byte(0x50, 0x20).is_none(), "nothing was written");
    assert!(
        wiring
            .line
            .configs()
            .iter()
            .all(|c| *c == LineConfig::input())
    );

    // The read that is allowed still works: no write part, an address on the list.
    let mut into = [0; 2];
    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[], 2, &mut into),
        Ok(2)
    );
    // And the same address with a write part is refused, so the read above sent no pointer.
    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[0x10], 2, &mut into),
        Err(REFUSED)
    );
}

#[test]
fn a_bad_setting_is_refused_at_bind() {
    let cases: [(&str, ConfigValue<'_>); 7] = [
        ("writes", ConfigValue::Str("true")),
        ("writes", ConfigValue::Int(1)),
        ("allow_i2c", ConfigValue::Str("0x80")),
        ("allow_i2c", ConfigValue::Str("zz")),
        ("allow_i2c", ConfigValue::Int(-1)),
        ("allow_i2c", ConfigValue::Bool(true)),
        ("allow_i2c", ConfigValue::Str("0x")),
    ];
    for (key, value) in cases {
        let mut wiring = wired();
        assert_eq!(
            bind(&mut wiring.board, &[(key, value)]).err(),
            Some(Status::INVALID_ARGUMENT),
            "{key} = {value:?}"
        );
    }
}
