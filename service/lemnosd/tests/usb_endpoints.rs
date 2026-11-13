//! A USB function's bulk endpoints lent to a client through the service's socket: the two
//! descriptors come over `SCM_RIGHTS`, one connection holds them at a time, they are freed when the
//! holder goes, and the bus is reported as the host drives it. The simulated board's function
//! stands in for the controller, with a socket pair for each endpoint file: the test plays the
//! host at the far end of each.

mod common;

use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use common::start_board;
use lemnos_client::{Error, Session};
use lemnos_core::Status;
use lemnos_driver_usb_gadget::UsbGadget;
use lemnos_hal::blocks::UsbFunctionEvent;
use lemnos_hal::mock::MockUsbFunction;
use lemnos_host::entry;
use lemnos_linux_stream::LinuxStreams;
use lemnos_sim::SimBoard;

const WAIT: Duration = Duration::from_secs(5);
/// The `state` method of `usb.function/1`: `up`, `configured`, `suspended`, `request_pending`.
const STATE: u16 = 4;

const DEVICES: &str = r#"
[[devices]]
id = "gadget"
driver = "usb-gadget"
uses = { function = "udc0" }
config = { vendor = 0x1209, product = 1, interfaces = "interface 0xff 0 0 pipe\nendpoint in bulk\nendpoint out bulk\n" }
"#;

/// The test's ends: what it writes to the OUT endpoint's far side, and what it reads from the IN
/// endpoint's far side. The service's sim function holds the other ends.
struct Host {
    to_device: UnixStream,
    from_device: UnixStream,
    function: MockUsbFunction,
}

fn start() -> (common::Running, Host) {
    let (out_device, out_host) = UnixStream::pair().unwrap();
    let (in_device, in_host) = UnixStream::pair().unwrap();
    let function =
        MockUsbFunction::with_endpoint_files(OwnedFd::from(out_device), OwnedFd::from(in_device));
    let sim = SimBoard::new();
    sim.add_usb_function_of("udc0", function.clone());
    let running = start_board(DEVICES, vec![entry::<UsbGadget>()], sim, None);
    let host = Host {
        to_device: out_host,
        from_device: in_host,
        function,
    };
    (running, host)
}

fn connect(socket: &std::path::Path, name: &str) -> Session {
    Session::connect(socket, name).unwrap()
}

fn device_index(session: &mut Session) -> u32 {
    session
        .find("usb.function/1")
        .unwrap()
        .expect("the gadget")
        .index
}

/// Asks until the pair is lent: the gadget may still be coming up, and a holder that just went
/// may not have been noticed yet.
fn take_pair(session: &mut Session, device: u32) -> Result<(UnixStream, UnixStream), Error> {
    let deadline = Instant::now() + WAIT;
    loop {
        match session.usb_endpoints(device, std::sync::Arc::new(LinuxStreams)) {
            Ok(pair) => return Ok((UnixStream::from(pair.out), UnixStream::from(pair.input))),
            Err(Error::Refused { status, .. })
                if matches!(status, Status::NO_DEVICE | Status::BUSY)
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    }
}

fn refused(error: Error) -> Status {
    match error {
        Error::Refused { status, .. } => status,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn read_exact(stream: &mut UnixStream, len: usize) -> Vec<u8> {
    stream.set_read_timeout(Some(WAIT)).unwrap();
    let mut buf = vec![0; len];
    stream.read_exact(&mut buf).unwrap();
    buf
}

/// The next state value a subscriber gets, as its four bools.
fn next_state(session: &mut Session) -> [bool; 4] {
    // The state record is a block: its length (4 bytes, 4), then its four bools.
    let mut buf = [0u8; 64];
    let info = session
        .next_value(&mut buf, WAIT)
        .unwrap()
        .expect("a state value");
    assert_eq!(info.len, 8);
    assert_eq!(buf[..4], [4, 0, 0, 0]);
    [buf[4], buf[5], buf[6], buf[7]].map(|byte| byte != 0)
}

#[test]
fn a_client_gets_the_pair_and_bytes_flow_both_ways() {
    let (running, mut host) = start();
    let mut one = connect(&running.socket, "one");
    let device = device_index(&mut one);
    let (out, input) = take_pair(&mut one, device).unwrap();

    // The host sends to the OUT endpoint; the client reads it from its own copy.
    let mut out = out;
    host.to_device.write_all(b"from the host").unwrap();
    assert_eq!(read_exact(&mut out, 13), b"from the host");

    // The client writes the IN endpoint; the host reads it at the far end.
    let mut input = input;
    input.write_all(b"from the client").unwrap();
    assert_eq!(read_exact(&mut host.from_device, 15), b"from the client");
}

#[test]
fn one_connection_holds_the_pair_and_a_second_is_refused_busy() {
    let (running, _host) = start();
    let mut one = connect(&running.socket, "one");
    let device = device_index(&mut one);
    take_pair(&mut one, device).unwrap();

    let mut two = connect(&running.socket, "two");
    let status = refused(
        two.usb_endpoints(device, std::sync::Arc::new(LinuxStreams))
            .err()
            .unwrap(),
    );
    assert_eq!(status, Status::BUSY);

    // The holder asking again gets its own hold back, not a refusal.
    take_pair(&mut one, device).unwrap();

    // Released: the second connection gets it.
    one.usb_endpoints_release(device).unwrap();
    take_pair(&mut two, device).unwrap();
}

#[test]
fn a_holder_that_goes_frees_the_pair() {
    let (running, _host) = start();
    let mut one = connect(&running.socket, "one");
    let device = device_index(&mut one);
    take_pair(&mut one, device).unwrap();
    drop(one);

    let mut two = connect(&running.socket, "two");
    take_pair(&mut two, device).unwrap();
}

#[test]
fn giving_up_with_no_hold_is_refused() {
    let (running, _host) = start();
    let mut one = connect(&running.socket, "one");
    let device = device_index(&mut one);
    take_pair(&mut one, device).unwrap();
    one.usb_endpoints_release(device).unwrap();
    let status = refused(one.usb_endpoints_release(device).err().unwrap());
    assert_eq!(status, Status::NOT_FOUND);
}

#[test]
fn the_bus_is_reported_to_a_subscriber_and_the_pair_outlives_a_reconnect() {
    let (running, mut host) = start();
    let mut watch = connect(&running.socket, "watch");
    let device = device_index(&mut watch);
    let mut handle = watch.open("gadget", "usb.function/1").unwrap();
    watch.subscribe(&mut handle, STATE, 10_000, true).unwrap();
    // Up, and not configured yet: the first value that says so.
    assert_eq!(
        until(&mut watch, |state| state[0]),
        [true, false, false, false]
    );

    let mut holder = connect(&running.socket, "holder");
    let (mut out, _input) = take_pair(&mut holder, device).unwrap();

    host.function.event(UsbFunctionEvent::Configured);
    assert_eq!(next_state(&mut watch), [true, true, false, false]);
    host.function.event(UsbFunctionEvent::Suspended);
    assert_eq!(next_state(&mut watch), [true, true, true, false]);
    host.function.event(UsbFunctionEvent::Resumed);
    assert_eq!(next_state(&mut watch), [true, true, false, false]);
    host.function.event(UsbFunctionEvent::Deconfigured);
    assert_eq!(next_state(&mut watch), [true, false, false, false]);

    // The host comes back: the same descriptors carry bytes again, with no new hand-off.
    host.function.event(UsbFunctionEvent::Configured);
    assert_eq!(next_state(&mut watch), [true, true, false, false]);
    host.to_device.write_all(b"again").unwrap();
    assert_eq!(read_exact(&mut out, 5), b"again");
}

/// Reads the state values until one satisfies `want`, and returns it.
fn until(session: &mut Session, want: impl Fn([bool; 4]) -> bool) -> [bool; 4] {
    let deadline = Instant::now() + WAIT;
    loop {
        let state = next_state(session);
        if want(state) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "the gadget never reached {state:?}"
        );
    }
}

#[test]
fn the_hold_ends_when_the_gadget_goes_down_and_a_new_hand_off_comes_once_it_is_up() {
    let (running, mut host) = start();
    let mut one = connect(&running.socket, "one");
    let device = device_index(&mut one);
    let mut watch = connect(&running.socket, "watch");
    let mut handle = watch.open("gadget", "usb.function/1").unwrap();
    watch.subscribe(&mut handle, STATE, 10_000, true).unwrap();
    until(&mut watch, |state| state[0]);
    take_pair(&mut one, device).unwrap();

    // The controller lets go: the gadget comes down, and the hold with it.
    host.function.release();
    until(&mut watch, |state| !state[0]);
    let status = refused(one.usb_endpoints_release(device).err().unwrap());
    assert_eq!(status, Status::NOT_FOUND);

    // Once the gadget is enabled again, another connection gets a fresh pair.
    until(&mut watch, |state| state[0]);
    let mut two = connect(&running.socket, "two");
    let (mut out, _input) = take_pair(&mut two, device).unwrap();
    host.to_device.write_all(b"back").unwrap();
    assert_eq!(read_exact(&mut out, 4), b"back");
}
