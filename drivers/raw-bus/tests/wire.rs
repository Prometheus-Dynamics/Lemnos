//! Raw access over the wire: a real service, with this driver bound to a simulated board, reached
//! through its socket by the client. These tests measure what a call can carry: the largest write
//! and read that cross the socket, and the typed client's own limit.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lemnos_client::{Client, Error};
use lemnos_contract_raw_bus::RawBus as RawBusApi;
use lemnos_core::{Encode, Status, Writer};
use lemnos_driver_raw_bus::RawBus;
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_sim::{Layout, SimBoard, SimI2c, SimSpi};
use lemnos_wire::MAX_FRAME;
use lemnosd::{Config, Service};

const TAG: &str = "raw-bus/1";
const I2C: u16 = 1;
const SPI: u16 = 2;

/// One device with every resource. Writes are on, and only 0x50 may be used on the I2C bus.
const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "t"

[[devices]]
id = "raw"
driver = "raw-bus"
uses = { i2c = "i2c", spi = "spi" }
config = { writes = true, allow_i2c = "0x50" }
"#;

/// A running service and the simulated parts it drives.
struct Running {
    service: Option<Service>,
    socket: PathBuf,
    dir: PathBuf,
    i2c: SimI2c,
    spi: SimSpi,
}

impl Drop for Running {
    fn drop(&mut self) {
        drop(self.service.take());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start() -> Running {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "raw-bus-wire-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("sock");

    let board = SimBoard::new();
    let i2c = board.add_i2c("i2c");
    i2c.attach(0x50, Layout::Bytes);
    i2c.attach(0x18, Layout::Bytes);
    let spi = board.add_spi("spi");

    let service = Service::start(Config {
        socket: socket.clone(),
        name: "raw-bus-test".to_string(),
        board: parse(BOARD).unwrap(),
        drivers: vec![entry::<RawBus>()],
        backend: Box::new(board),
        options: Options {
            call_deadline: Duration::from_secs(30),
            ..Options::default()
        },
    })
    .unwrap();
    Running {
        service: Some(service),
        socket,
        dir,
        i2c,
        spi,
    }
}

/// The arguments of `i2c_transfer`, as the generated client encodes them: the address, the write,
/// the read length, then the reply capacity the `#[out]` buffer has.
fn i2c_args(address: u16, write: &[u8], read_len: u32, capacity: u32) -> Vec<u8> {
    let mut buf = vec![0u8; write.len() + 16];
    let mut out = Writer::new(&mut buf);
    address.encode(&mut out).unwrap();
    Encode::encode(&write, &mut out).unwrap();
    read_len.encode(&mut out).unwrap();
    capacity.encode(&mut out).unwrap();
    let len = out.len();
    buf.truncate(len);
    buf
}

/// The arguments of `spi_transfer`.
fn spi_args(write: &[u8], read_len: u32, capacity: u32) -> Vec<u8> {
    let mut buf = vec![0u8; write.len() + 12];
    let mut out = Writer::new(&mut buf);
    Encode::encode(&write, &mut out).unwrap();
    read_len.encode(&mut out).unwrap();
    capacity.encode(&mut out).unwrap();
    let len = out.len();
    buf.truncate(len);
    buf
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed))
        .collect()
}

#[test]
fn a_large_spi_write_and_read_cross_the_wire_in_one_transaction() {
    let running = start();
    running.spi.respond(&pattern(256 * 1024, 3));
    let client = Client::connect(&running.socket, "test").unwrap();
    let mut handle = client.open_tag("raw", TAG).unwrap();

    // Half a mebibyte out, a quarter back: one call, one chip-select assertion.
    let write = pattern(512 * 1024, 9);
    let args = spi_args(&write, 256 * 1024, 256 * 1024);
    let mut reply = vec![0u8; 256 * 1024];
    let n = client.call(&mut handle, SPI, &args, &mut reply).unwrap();

    assert_eq!(n, 256 * 1024);
    assert_eq!(reply, pattern(256 * 1024, 3));
    let sent = running.spi.written();
    assert_eq!(sent.len(), 1, "one transaction");
    assert_eq!(sent[0], write, "every byte of the write, in order");
}

#[test]
fn a_large_i2c_write_and_read_cross_the_wire() {
    let running = start();
    let client = Client::connect(&running.socket, "test").unwrap();
    let mut handle = client.open_tag("raw", TAG).unwrap();

    let before = running.i2c.transactions();
    let write = pattern(300 * 1024, 1);
    let args = i2c_args(0x50, &write, 0, 0);
    let mut reply = [0u8; 0];
    assert_eq!(client.call(&mut handle, I2C, &args, &mut reply).unwrap(), 0);
    assert_eq!(running.i2c.transactions(), before + 1);

    let args = i2c_args(0x50, &[], 200 * 1024, 200 * 1024);
    let mut reply = vec![0u8; 200 * 1024];
    assert_eq!(
        client.call(&mut handle, I2C, &args, &mut reply).unwrap(),
        200 * 1024
    );
    assert_eq!(running.i2c.transactions(), before + 2);
}

#[test]
fn a_refused_transfer_of_any_size_is_refused_before_the_bus() {
    let running = start();
    let client = Client::connect(&running.socket, "test").unwrap();
    let mut handle = client.open_tag("raw", TAG).unwrap();
    let before = running.i2c.transactions();

    // 0x18 is not on the allow list: a write and a read of it are both refused.
    let args = i2c_args(0x18, &pattern(300 * 1024, 2), 0, 0);
    let mut reply = [0u8; 0];
    match client.call(&mut handle, I2C, &args, &mut reply) {
        Err(Error::Refused { status, .. }) => assert_eq!(status, Status::NOT_PERMITTED),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let args = i2c_args(0x18, &[], 1, 1);
    let mut reply = [0u8; 1];
    match client.call(&mut handle, I2C, &args, &mut reply) {
        Err(Error::Refused { status, .. }) => assert_eq!(status, Status::NOT_PERMITTED),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(
        running.i2c.transactions(),
        before,
        "no transfer reached the bus"
    );
}

#[test]
fn a_call_bigger_than_a_frame_is_refused_by_the_client_and_the_connection_goes_on() {
    let running = start();
    let client = Client::connect(&running.socket, "test").unwrap();
    let mut handle = client.open_tag("raw", TAG).unwrap();

    let too_big = i2c_args(0x50, &vec![0u8; MAX_FRAME], 0, 0);
    let mut reply = [0u8; 0];
    assert!(client.call(&mut handle, I2C, &too_big, &mut reply).is_err());

    let args = i2c_args(0x50, &[0xaa], 0, 0);
    assert_eq!(client.call(&mut handle, I2C, &args, &mut reply).unwrap(), 0);
}

#[test]
fn the_typed_client_carries_at_most_242_bytes_of_i2c_write() {
    let running = start();
    let client = Client::connect(&running.socket, "test").unwrap();
    let mut device = client.open::<dyn RawBusApi>("raw").unwrap();

    // A typed client builds its arguments in a 256-byte buffer. An I2C call's own fields take 14
    // bytes (address, write length, read length, reply capacity), so 242 bytes of write fit.
    let before = running.i2c.transactions();
    assert_eq!(device.i2c_transfer(0x50, &[0x11; 242], 0, &mut []), Ok(0));
    assert_eq!(running.i2c.transactions(), before + 1);
    assert!(device.i2c_transfer(0x50, &[0x11; 243], 0, &mut []).is_err());
    assert_eq!(
        running.i2c.transactions(),
        before + 1,
        "the refused call sent nothing"
    );
}

#[test]
fn a_typed_read_fills_the_reply_buffer_the_caller_gives() {
    let running = start();
    let client = Client::connect(&running.socket, "test").unwrap();
    let mut device = client.open::<dyn RawBusApi>("raw").unwrap();
    let mut into = [0u8; 3];
    assert_eq!(device.i2c_transfer(0x50, &[], 3, &mut into), Ok(3));
    let mut small = [0u8; 2];
    assert!(device.i2c_transfer(0x50, &[], 3, &mut small).is_err());
}

#[test]
fn the_wire_limit_is_one_frame_less_the_call_fields() {
    // Measured: an I2C write's arguments are the frame's 26 bytes of fixed fields, the call's 14
    // bytes (address, length, read length, capacity) and the write. So MAX_FRAME - 40 bytes of
    // write cross, and MAX_FRAME - 30 are refused by the client before anything is sent. SPI's
    // call has 12 bytes of its own, so MAX_FRAME - 38 cross.
    let running = start();
    let client = Client::connect(&running.socket, "test").unwrap();
    let mut handle = client.open_tag("raw", TAG).unwrap();
    let mut reply = [0u8; 0];

    let before = running.i2c.transactions();
    let fits = i2c_args(0x50, &vec![0u8; MAX_FRAME - 40], 0, 0);
    assert_eq!(client.call(&mut handle, I2C, &fits, &mut reply).unwrap(), 0);
    assert_eq!(running.i2c.transactions(), before + 1);
    let too_big = i2c_args(0x50, &vec![0u8; MAX_FRAME - 30], 0, 0);
    assert!(client.call(&mut handle, I2C, &too_big, &mut reply).is_err());
    assert_eq!(running.i2c.transactions(), before + 1);

    let fits = spi_args(&vec![0u8; MAX_FRAME - 38], 0, 0);
    assert_eq!(client.call(&mut handle, SPI, &fits, &mut reply).unwrap(), 0);
    let too_big = spi_args(&vec![0u8; MAX_FRAME - 28], 0, 0);
    assert!(client.call(&mut handle, SPI, &too_big, &mut reply).is_err());

    // A read: the reply may be MAX_FRAME - 32 bytes, the most a reply frame holds.
    let args = i2c_args(0x50, &[], (MAX_FRAME - 32) as u32, (MAX_FRAME - 32) as u32);
    let mut big = vec![0u8; MAX_FRAME - 32];
    assert_eq!(
        client.call(&mut handle, I2C, &args, &mut big).unwrap(),
        MAX_FRAME - 32
    );
}
