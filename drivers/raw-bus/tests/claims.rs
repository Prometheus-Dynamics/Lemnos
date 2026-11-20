//! Claims: one caller takes the device; its line is configured and its edges read; the claim ends
//! with a release or when its caller goes, and the line and PWM channel are put in their safe state.

mod common;

use common::{bind, client, wired};
use lemnos_contract_raw_bus::RawBus as _;
use lemnos_contract_raw_bus::{
    BIAS_PULL_UP, DRIVE_PUSH_PULL, EDGE_BOTH, EDGE_BYTES, EDGE_NONE, LINE_INPUT, LINE_OUTPUT,
    MAX_EDGES, decode_edge,
};
use lemnos_core::{CallerId, ConfigValue, Driver, ErrorKind, Status};
use lemnos_driver_raw_bus::RawBus;
use lemnos_hal::raw::{Bias, Direction, EdgeDetect, LineConfig};

const A: CallerId = CallerId(5);
const B: CallerId = CallerId(7);

const WRITES: [(&str, ConfigValue<'static>); 1] = [("writes", ConfigValue::Bool(true))];

fn as_caller(driver: &mut RawBus, caller: CallerId) -> &mut RawBus {
    driver.caller(caller);
    driver
}

fn input_with_edges(driver: &mut RawBus) -> lemnos_core::Result<()> {
    client(driver).line_configure(
        LINE_INPUT,
        false,
        false,
        BIAS_PULL_UP,
        DRIVE_PUSH_PULL,
        EDGE_BOTH,
        0,
    )
}

fn edges(driver: &mut RawBus) -> Vec<(bool, u32, u64)> {
    let mut into = [0u8; MAX_EDGES * EDGE_BYTES];
    let n = client(driver).read_edges(&mut into).unwrap();
    into[..n]
        .chunks(EDGE_BYTES)
        .map(|bytes| {
            let edge = decode_edge(bytes).unwrap();
            (edge.rising, edge.seq, edge.timestamp_ns)
        })
        .collect()
}

#[test]
fn a_claim_is_one_callers_and_others_are_busy_until_it_ends() {
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &WRITES).unwrap();
    assert_eq!(client(as_caller(&mut driver, A)).claim(), Ok(()));
    // Claiming again while holding is fine.
    assert_eq!(client(&mut driver).claim(), Ok(()));
    assert_eq!(client(as_caller(&mut driver, B)).claim(), Err(Status::BUSY));
    // Another caller can't drive or use the device...
    assert_eq!(
        client(&mut driver).pwm_set(1000, 500, true),
        Err(Status::BUSY)
    );
    assert_eq!(client(&mut driver).uart_write(b"x"), Err(Status::BUSY));
    // ...nor configure the line or release the claim; reading the line stays open.
    assert_eq!(
        input_with_edges(&mut driver),
        Err(Status::FAILED_PRECONDITION)
    );
    assert_eq!(
        client(&mut driver).release(),
        Err(Status::FAILED_PRECONDITION)
    );
    assert!(client(&mut driver).line_get().is_ok());
    // The holder can.
    assert_eq!(
        client(as_caller(&mut driver, A)).pwm_set(1000, 500, true),
        Ok(())
    );
    assert_eq!(client(&mut driver).release(), Ok(()));
    assert_eq!(client(as_caller(&mut driver, B)).claim(), Ok(()));
}

#[test]
fn lemnos_itself_cant_claim() {
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &WRITES).unwrap();
    assert_eq!(
        client(as_caller(&mut driver, CallerId::LOCAL)).claim(),
        Err(Status::NOT_PERMITTED)
    );
}

/// The claim ends when its caller goes: the line returns to how the board set it at bind and the PWM
/// channel is turned off. The next caller then claims.
#[test]
fn a_claim_ends_with_its_caller_and_the_device_goes_to_its_safe_state() {
    let mut wiring = wired();
    let mut driver = bind(
        &mut wiring.board,
        &[
            ("writes", ConfigValue::Bool(true)),
            ("line_direction", ConfigValue::Str("output")),
        ],
    )
    .unwrap();
    client(as_caller(&mut driver, A)).claim().unwrap();
    client(&mut driver)
        .line_configure(
            LINE_OUTPUT,
            true,
            false,
            BIAS_PULL_UP,
            DRIVE_PUSH_PULL,
            EDGE_NONE,
            0,
        )
        .unwrap();
    client(&mut driver).pwm_set(1000, 400, true).unwrap();
    assert_eq!(wiring.line.level(), Some(true));
    assert!(wiring.pwm.current().enabled);

    driver.caller_gone(B);
    assert_eq!(client(as_caller(&mut driver, B)).claim(), Err(Status::BUSY));
    driver.caller_gone(A);
    assert_eq!(wiring.line.config(), LineConfig::output(false));
    assert_eq!(wiring.line.level(), Some(false));
    let pwm = wiring.pwm.current();
    assert!(!pwm.enabled);
    assert_eq!((pwm.period_ns, pwm.duty_ns), (1000, 400));
    assert_eq!(client(as_caller(&mut driver, B)).claim(), Ok(()));
}

#[test]
fn the_board_names_the_safe_state() {
    for (safe, expect) in [
        ("low", Some(LineConfig::output(false))),
        ("high", Some(LineConfig::output(true))),
        ("input", Some(LineConfig::high_impedance())),
    ] {
        let mut wiring = wired();
        let mut driver = bind(
            &mut wiring.board,
            &[
                ("writes", ConfigValue::Bool(true)),
                ("line_safe", ConfigValue::Str(safe)),
            ],
        )
        .unwrap();
        client(as_caller(&mut driver, A)).claim().unwrap();
        input_with_edges(&mut driver).unwrap();
        client(&mut driver).release().unwrap();
        assert_eq!(Some(wiring.line.config()), expect, "{safe}");
    }
    // keep: left as the holder set it; the PWM channel too with pwm_safe = keep.
    let mut wiring = wired();
    let mut driver = bind(
        &mut wiring.board,
        &[
            ("writes", ConfigValue::Bool(true)),
            ("line_safe", ConfigValue::Str("keep")),
            ("pwm_safe", ConfigValue::Str("keep")),
        ],
    )
    .unwrap();
    client(as_caller(&mut driver, A)).claim().unwrap();
    input_with_edges(&mut driver).unwrap();
    client(&mut driver).pwm_set(1000, 400, true).unwrap();
    client(&mut driver).release().unwrap();
    assert_eq!(wiring.line.config().edge, EdgeDetect::Both);
    assert!(wiring.pwm.current().enabled);
}

/// With no line settings, a claim's end leaves the line an input with no bias.
#[test]
fn with_no_line_settings_the_safe_state_is_high_impedance() {
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &WRITES).unwrap();
    client(as_caller(&mut driver, A)).claim().unwrap();
    client(&mut driver)
        .line_configure(
            LINE_OUTPUT,
            true,
            false,
            BIAS_PULL_UP,
            DRIVE_PUSH_PULL,
            EDGE_NONE,
            0,
        )
        .unwrap();
    driver.caller_gone(A);
    assert_eq!(wiring.line.config(), LineConfig::high_impedance());
}

#[test]
fn line_configure_needs_the_claim_writes_for_an_output_and_known_values() {
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &[]).unwrap();
    assert_eq!(
        input_with_edges(as_caller(&mut driver, A)),
        Err(Status::FAILED_PRECONDITION)
    );
    client(&mut driver).claim().unwrap();
    assert_eq!(
        client(&mut driver).line_configure(LINE_OUTPUT, true, false, 0, 0, 0, 0),
        Err(Status::NOT_PERMITTED)
    );
    assert_eq!(
        client(&mut driver).line_configure(2, false, false, 0, 0, 0, 0),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        client(&mut driver).line_configure(LINE_INPUT, false, false, 4, 0, 0, 0),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        client(&mut driver).line_configure(LINE_INPUT, false, false, 0, 3, 0, 0),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        client(&mut driver).line_configure(LINE_INPUT, false, false, 0, 0, 4, 0),
        Err(Status::INVALID_ARGUMENT)
    );
    input_with_edges(&mut driver).unwrap();
    let config = wiring.line.config();
    assert_eq!(config.direction, Direction::Input);
    assert_eq!(config.bias, Bias::PullUp);
    assert_eq!(config.edge, EdgeDetect::Both);
}

/// Edges are kept, not consumed: every reader sees the last ones, and the sequence numbers show a
/// gap when more came than the ring holds.
#[test]
fn edges_are_read_by_anyone_oldest_first_and_a_full_ring_drops_the_oldest() {
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &[]).unwrap();
    client(as_caller(&mut driver, A)).claim().unwrap();
    input_with_edges(&mut driver).unwrap();
    wiring.line.edge(true, 100);
    wiring.line.edge(false, 200);
    assert_eq!(edges(&mut driver), [(true, 1, 100), (false, 2, 200)]);
    // Another caller, and Lemnos's own subscription reads, see the same.
    assert_eq!(
        edges(as_caller(&mut driver, CallerId::LOCAL)),
        [(true, 1, 100), (false, 2, 200)]
    );
    for n in 0..(MAX_EDGES as u64 + 5) {
        wiring.line.edge(n % 2 == 0, 1000 + n);
    }
    let seen = edges(&mut driver);
    assert_eq!(seen.len(), MAX_EDGES);
    // 2 + 37 edges came, the ring holds 32: the oldest kept is number 8.
    assert_eq!(seen.first().unwrap().1, 8);
    assert_eq!(seen.last().unwrap().1, 2 + MAX_EDGES as u32 + 5);
    // A buffer for two takes the newest two.
    let mut into = [0u8; 2 * EDGE_BYTES + 3];
    assert_eq!(
        client(&mut driver).read_edges(&mut into),
        Ok(2 * EDGE_BYTES)
    );
    assert_eq!(
        decode_edge(&into[EDGE_BYTES..2 * EDGE_BYTES]).unwrap().seq,
        39
    );
    // A claim's end clears them.
    client(as_caller(&mut driver, A)).release().unwrap();
    assert!(edges(&mut driver).is_empty());
}

/// A safe state that can't be set is owed: health says so, and each call tries again first.
#[test]
fn a_safe_state_that_fails_is_tried_again_by_the_next_call() {
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &WRITES).unwrap();
    client(as_caller(&mut driver, A)).claim().unwrap();
    client(&mut driver)
        .line_configure(LINE_OUTPUT, true, false, 0, 0, EDGE_NONE, 0)
        .unwrap();
    wiring.line.fail(Some(ErrorKind::Busy));
    driver.caller_gone(A);
    assert!(matches!(driver.health(), lemnos_core::Health::Failed(_)));
    wiring.line.fail(None);
    let _ = client(as_caller(&mut driver, B)).line_get();
    assert_eq!(driver.health(), lemnos_core::Health::Ok);
    assert_eq!(wiring.line.config(), LineConfig::high_impedance());
}
