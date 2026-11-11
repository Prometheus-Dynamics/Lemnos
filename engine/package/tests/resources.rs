//! A package's driver uses every kind of resource a built-in driver can be given, against the
//! simulated board, and what the resource did and answered comes back through the host.
//!
//! The driver is the real compiled fixture. Its `exercise` method makes one call on one
//! resource, as any driver makes it; the test plays the hardware on the other side of the mock.

mod common;

use common::{
    buffer_file, exercise, fixtures, host_on, index, kind, method, scratch, usb_descriptors,
};
use lemnos_core::package::status_of_bus;
use lemnos_core::{ErrorKind, Status};
use lemnos_hal::blocks::{ControlSetup, StreamFrame, UsbFunctionEvent};
use lemnos_hal::raw::{Bias, EdgeDetect, LineConfig, Polarity, PwmConfig, SpiConfig, SpiMode};
use lemnos_host::{DeviceState, Host, Options};
use lemnos_package::{Loader, Mode};
use lemnos_sim::SimBoard;
use lemnos_sim::mock::{ControlAnswer, MockStream, SpiRecord};

/// The fixture on `sim`, as one device `thing` that uses `uses`.
fn thing(sim: SimBoard, uses: &str) -> (Host, u32) {
    let mut loader = Loader::new(Mode::Open);
    loader.load(&fixtures().good).unwrap();
    let devices =
        format!("[[devices]]\nid = \"thing\"\ndriver = \"package-fixture\"\nuses = {{ {uses} }}\n");
    let (host, _) = host_on(sim, &devices, &loader.entries(), Options::default());
    assert_eq!(host.device("thing").unwrap().state, DeviceState::Running);
    let device = index(&host, "thing");
    (host, device)
}

fn le32(value: u32) -> [u8; 4] {
    value.to_le_bytes()
}

fn le64(value: u64) -> [u8; 8] {
    value.to_le_bytes()
}

fn joined(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

#[test]
fn spi_transactions_of_every_kind_of_step_cross() {
    let sim = SimBoard::new();
    let spi = sim.add_spi("spi0");
    let (host, thing) = thing(sim, r#"spi = "spi0""#);
    let run = |step, data: &[u8]| exercise(&host, thing, kind::SPI, step, data);

    // A write, then a read.
    spi.respond(&[0xaa, 0xbb]);
    assert_eq!(run(0, &[1, 2, 3]), Ok(vec![0xaa, 0xbb]));
    // Both at once, into another buffer.
    spi.respond(&[0x11, 0x22]);
    assert_eq!(run(1, &[4, 5]), Ok(vec![0x11, 0x22]));
    // Both at once, in place, then a delay.
    spi.respond(&[0x33, 0x44]);
    assert_eq!(run(2, &[6, 7]), Ok(vec![0x33, 0x44]));
    assert_eq!(spi.written(), [vec![1, 2, 3], vec![4, 5], vec![6, 7]]);
}

#[test]
fn a_raw_spi_transfer_carries_each_segments_settings() {
    let sim = SimBoard::new();
    let spi = sim.add_raw_spi("spi1");
    let (host, thing) = thing(sim, r#"raw_spi = "spi1""#);
    spi.respond(&[9, 8, 7, 6]);
    assert_eq!(
        exercise(&host, thing, kind::RAW_SPI, 0, &[3, 0x55, 0x66]),
        Ok(vec![9, 8, 7, 6])
    );
    assert_eq!(
        spi.transactions(),
        [vec![
            SpiRecord {
                config: SpiConfig {
                    mode: SpiMode::Mode3,
                    speed_hz: 1_000_000,
                    bits_per_word: 8,
                },
                tx: vec![0x55, 0x66],
                rx_len: 2,
            },
            SpiRecord {
                config: SpiConfig::default(),
                tx: vec![],
                rx_len: 2,
            },
        ]]
    );
}

#[test]
fn a_line_is_configured_driven_read_and_reports_edges() {
    let sim = SimBoard::new();
    let line = sim.add_line("gpio4");
    let (host, thing) = thing(sim, r#"line = "gpio4""#);
    let run = |step, data: &[u8]| exercise(&host, thing, kind::LINE, step, data);

    assert_eq!(run(0, &[1]), Ok(vec![]));
    assert_eq!(line.config(), LineConfig::output(true));
    assert_eq!(line.level(), Some(true));
    assert_eq!(run(3, &[0]), Ok(vec![]));
    assert_eq!(line.level(), Some(false));
    assert_eq!(run(2, &[]), Ok(vec![0]));

    // Every field of a configuration arrives.
    assert_eq!(run(1, &[]), Ok(vec![]));
    assert_eq!(
        line.config(),
        LineConfig::input()
            .active_low()
            .with_bias(Bias::PullUp)
            .with_edge(EdgeDetect::Both)
            .with_debounce_us(250)
    );
    line.drive(false);
    assert_eq!(run(2, &[]), Ok(vec![1]), "low on an active-low input is 1");
    // No edge yet, then one, then none again.
    assert_eq!(run(4, &[]), Ok(vec![0]));
    line.edge(true, 1234);
    assert_eq!(run(4, &[]), Ok(joined(&[&[1, 1], &le32(1), &le64(1234)])));
    assert_eq!(run(4, &[]), Ok(vec![0]));

    // The line's own refusals come back as they are.
    assert_eq!(
        run(3, &[1]),
        Err(Status::INVALID_ARGUMENT),
        "set on an input"
    );
    line.fail(Some(ErrorKind::Busy));
    assert_eq!(run(2, &[]), Err(Status::BUSY));
}

#[test]
fn a_pwm_channel_takes_its_settings_and_gives_them_back() {
    let sim = SimBoard::new();
    let pwm = sim.add_pwm("pwm0");
    let (host, thing) = thing(sim, r#"pwm = "pwm0""#);
    let settings = joined(&[&le64(20_000_000), &le64(1_500_000), &[1, 1]]);
    assert_eq!(exercise(&host, thing, kind::PWM, 0, &settings), Ok(vec![]));
    assert_eq!(
        pwm.current(),
        PwmConfig {
            period_ns: 20_000_000,
            duty_ns: 1_500_000,
            polarity: Polarity::Inversed,
            enabled: true,
        }
    );
    assert_eq!(exercise(&host, thing, kind::PWM, 1, &[]), Ok(settings));
    // More duty than period is the channel's to refuse.
    let wrong = joined(&[&le64(10), &le64(11), &[0, 0]]);
    assert_eq!(
        exercise(&host, thing, kind::PWM, 0, &wrong),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(pwm.history().len(), 1);
}

#[test]
fn a_uart_sends_receives_and_reports_every_kind_of_error() {
    let sim = SimBoard::new();
    let uart = sim.add_uart("uart1");
    let faults = sim.faults("uart1").unwrap();
    let (host, thing) = thing(sim, r#"uart = "uart1""#);
    let run = |step, data: &[u8]| exercise(&host, thing, kind::UART, step, data);

    assert_eq!(run(0, &le32(115_200)), Ok(vec![]));
    assert_eq!(uart.baud(), 115_200);
    assert_eq!(run(1, b"ping"), Ok(le64(4).to_vec()));
    assert_eq!(uart.sent(), b"ping");
    uart.receive(b"pong");
    assert_eq!(run(2, &[]), Ok(b"pong".to_vec()));
    assert_eq!(run(2, &[]), Ok(vec![]), "nothing arrived in time");

    // Each kind of error the port can report reaches the driver as that kind.
    for error in [
        ErrorKind::NotFound,
        ErrorKind::Unavailable,
        ErrorKind::Busy,
        ErrorKind::PermissionDenied,
        ErrorKind::Timeout,
        ErrorKind::InvalidInput,
        ErrorKind::Unsupported,
        ErrorKind::Configuration,
        ErrorKind::Nack,
        ErrorKind::Overrun,
        ErrorKind::Failed,
    ] {
        faults.fail_next(error);
        assert_eq!(run(1, b"x"), Err(status_of_bus(error)), "{error:?}");
    }
    assert_eq!(run(1, b"x"), Ok(le64(1).to_vec()));
}

#[test]
fn registers_are_read_written_modified_and_written_in_blocks() {
    let sim = SimBoard::new();
    let registers = sim.add_registers("block", 0x40);
    let (host, thing) = thing(sim, r#"registers = "block""#);
    let run = |step, data: &[u8]| exercise(&host, thing, kind::REGISTERS, step, data);

    assert_eq!(run(0, &[]), Ok(le64(0x40).to_vec()));
    registers.set(8, 0xabcd);
    assert_eq!(run(1, &le32(8)), Ok(le32(0xabcd).to_vec()));
    assert_eq!(run(2, &joined(&[&le32(4), &le32(0xdead_beef)])), Ok(vec![]));
    assert_eq!(
        run(3, &joined(&[&le32(4), &le32(0xff), &le32(0x12)])),
        Ok(vec![])
    );
    assert_eq!(
        run(4, &joined(&[&le32(16), &le32(1), &le32(2), &le32(3)])),
        Ok(vec![])
    );
    let written = [
        (4, 0xdead_beef),
        (4, 0xdead_be12),
        (16, 1),
        (20, 2),
        (24, 3),
    ];
    assert_eq!(registers.writes(), written);

    // Outside the window: refused, and a block that would run past the end writes nothing.
    assert_eq!(run(1, &le32(0x40)), Err(Status::INVALID_ARGUMENT));
    assert_eq!(
        run(4, &joined(&[&le32(0x3c), &le32(7), &le32(8)])),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(registers.writes(), written);
}

#[test]
fn an_interrupt_is_enabled_waited_for_and_disabled() {
    let sim = SimBoard::new();
    let interrupt = sim.add_interrupt("irq");
    let (host, thing) = thing(sim, r#"interrupt = "irq""#);
    let run = |step| exercise(&host, thing, kind::INTERRUPT, step, &[]);

    assert_eq!(run(2), Ok(vec![0]), "the timeout passed first");
    assert_eq!(run(0), Ok(vec![]));
    assert!(interrupt.enabled());
    interrupt.fire(77);
    // A wait without limit crosses as one, and the event comes back whole.
    assert_eq!(run(3), Ok(joined(&[&[1], &le32(1), &le64(77)])));
    assert_eq!(run(1), Ok(vec![]));
    assert!(!interrupt.enabled());
}

#[test]
fn dma_memory_is_allocated_filled_synced_read_and_released() {
    let sim = SimBoard::new();
    let dma = sim.add_dma("dma");
    let (host, thing) = thing(sim, r#"dma = "dma""#);
    let run = |step, data: &[u8]| exercise(&host, thing, kind::DMA, step, data);
    let read = |offset: u64, len: u32| run(2, &joined(&[&le64(offset), &le32(len)]));

    assert_eq!(
        run(0, &le64(64)),
        Ok(joined(&[&le32(1), &le64(64), &le64(1 << 32)]))
    );
    assert_eq!(run(1, &joined(&[&le64(4), &[1, 2, 3]])), Ok(vec![]));
    assert_eq!(read(4, 3), Ok(vec![1, 2, 3]));
    // Handed to the block, the CPU may not touch it; taken back, it sees what the block wrote.
    assert_eq!(run(3, &[]), Ok(vec![]));
    assert_eq!(read(4, 3), Err(Status::BUSY));
    dma.device_write(1, 0, &[9]);
    assert_eq!(run(4, &[]), Ok(vec![]));
    assert_eq!(read(0, 1), Ok(vec![9]));
    // Past the end of the buffer is refused.
    assert_eq!(read(60, 8), Err(Status::INVALID_ARGUMENT));
    assert_eq!(run(5, &[]), Ok(vec![]));
    assert_eq!(dma.live_buffers(), 0);
    assert_eq!(read(0, 1), Err(Status::NOT_FOUND));
}

#[test]
fn clocks_and_resets_are_switched_and_rates_come_back() {
    let sim = SimBoard::new();
    let clocks = sim.add_clocks("clk");
    let reset = sim.add_reset("rst");
    let (host, thing) = thing(sim, r#"clocks = "clk", reset = "rst""#);
    let clock = |step, data: &[u8]| exercise(&host, thing, kind::CLOCKS, step, data);

    assert_eq!(clock(0, &le32(2)), Ok(vec![]));
    assert!(clocks.enabled(2));
    let rate = 24_000_000u64;
    assert_eq!(
        clock(3, &joined(&[&le32(2), &le64(rate)])),
        Ok(le64(rate).to_vec())
    );
    assert_eq!(clock(2, &le32(2)), Ok(le64(rate).to_vec()));
    assert_eq!(clock(1, &le32(2)), Ok(vec![]));
    assert!(!clocks.enabled(2));

    assert_eq!(exercise(&host, thing, kind::RESET, 0, &le32(1)), Ok(vec![]));
    assert_eq!(exercise(&host, thing, kind::RESET, 1, &le32(1)), Ok(vec![]));
    assert_eq!(reset.history(), [(1, true), (1, false)]);
}

#[test]
fn a_stream_carries_buffers_jobs_bytes_and_buffer_handles() {
    let dir = scratch("stream");
    let stream =
        MockStream::with_handles(vec![buffer_file(&dir, "0", 16), buffer_file(&dir, "1", 16)]);
    let sim = SimBoard::new();
    let consumer = sim.add_stream_of("frames", stream);
    let (host, thing) = thing(sim, r#"stream = "frames""#);
    let run = |step, data: &[u8]| exercise(&host, thing, kind::STREAM, step, data);

    assert_eq!(run(0, &[]), Ok(le32(2).to_vec()));
    assert_eq!(run(1, &[]), Ok(joined(&[&[1], &le32(0)])));
    // The driver writes into the buffer, and the consumer's bytes are read back out of it.
    assert_eq!(run(2, &[2, 5, 6, 7]), Ok(vec![1]));
    assert_eq!(consumer.contents(0)[..5], [0, 0, 5, 6, 7]);
    consumer.fill(0, &[1, 2]);
    assert_eq!(run(3, &[0, 5]), Ok(vec![1, 1, 2, 5, 6, 7]));
    // The buffer's handle crosses as a descriptor: the driver reads the same memory through it.
    assert_eq!(run(6, &[5]), Ok(vec![1, 1, 2, 5, 6, 7]));
    // A range outside the buffer is refused.
    assert_eq!(run(2, &[15, 1, 2]), Err(Status::INVALID_ARGUMENT));

    assert_eq!(run(4, &[5, 3]), Ok(vec![]));
    assert_eq!(
        consumer.take_frame(),
        Some(StreamFrame {
            buffer: 0,
            bytes: 5,
            sequence: 3,
            identity: 0xfeed,
            timestamp_ns: 99,
        })
    );
    // Published, the buffer is no longer the driver's: no handle, no bytes.
    assert_eq!(run(6, &[1]), Err(Status::INVALID_ARGUMENT));
    assert_eq!(run(3, &[0, 1]), Err(Status::INVALID_ARGUMENT));

    assert_eq!(run(5, &[]), Ok(vec![0]));
    consumer.post_job(b"job!");
    assert_eq!(run(5, &[]), Ok(joined(&[&[1], b"job!"])));

    // The stream says what its buffers are, and a buffer taken under a consumer that has since
    // gone is no longer current and has nothing behind it.
    assert_eq!(run(8, &[]), Ok(vec![1]));
    assert_eq!(run(1, &[]), Ok(joined(&[&[1], &le32(1)])));
    assert_eq!(run(7, &[]), Ok(vec![1]));
    consumer.change_consumer();
    assert_eq!(run(7, &[]), Ok(vec![0]));
    assert_eq!(run(6, &[1]), Ok(vec![0]));
    // The driver faults its consumer, which the stream records.
    assert_eq!(run(9, &[]), Ok(vec![]));
    assert_eq!(consumer.faults(), 1);
    // The last free buffer, then none.
    assert_eq!(run(1, &[]), Ok(joined(&[&[1], &le32(0)])));
    assert_eq!(run(1, &[]), Ok(vec![0]));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_stream_without_handles_says_so() {
    let sim = SimBoard::new();
    sim.add_stream_of("plain", MockStream::with_bytes(1, 8));
    let (host, thing) = thing(sim, r#"stream = "plain""#);
    let run = |step, data: &[u8]| exercise(&host, thing, kind::STREAM, step, data);
    assert_eq!(run(1, &[]), Ok(joined(&[&[1], &le32(0)])));
    assert_eq!(run(6, &[1]), Err(Status::NOT_SUPPORTED));
    assert_eq!(run(8, &[]), Ok(vec![0]), "plain memory");
    assert_eq!(run(7, &[]), Ok(vec![1]));
    // `write` and `read` are the way into it.
    assert_eq!(run(2, &[0, 4, 4]), Ok(vec![1]));
    assert_eq!(run(3, &[0, 2]), Ok(vec![1, 4, 4]));
}

#[test]
fn a_usb_device_is_claimed_set_up_and_carries_every_kind_of_transfer() {
    let descriptor = [9, 2, 25, 0, 1, 1, 0, 0x80, 50];
    let sim = SimBoard::new();
    let usb = sim.add_usb_device("usb0", &descriptor);
    let (host, thing) = thing(sim, r#"usb = "usb0""#);
    let run = |step, data: &[u8]| exercise(&host, thing, kind::USB_DEVICE, step, data);

    assert_eq!(run(0, &[]), Ok(descriptor.to_vec()));
    assert_eq!(run(1, &[0]), Ok(vec![]));
    assert_eq!(usb.claimed(), [0]);
    assert_eq!(run(1, &[0]), Err(Status::BUSY));
    assert_eq!(run(3, &[0, 1]), Ok(vec![]));
    assert_eq!(usb.alternate(0), Some(1));
    // A kernel driver holds an interface until it is detached.
    usb.bind_kernel_driver(1);
    assert_eq!(run(1, &[1]), Err(Status::BUSY));
    assert_eq!(run(4, &[1]), Ok(vec![]));
    assert_eq!(usb.detaches(), [1]);
    assert_eq!(run(1, &[1]), Ok(vec![]));
    assert_eq!(run(2, &[1]), Ok(vec![]));
    assert_eq!(usb.claimed(), [0]);

    // Control in and out, with the whole setup.
    let setup = [0x80, 6, 0x00, 0x01, 0x34, 0x12];
    usb.queue_in(&[1, 2, 3]);
    assert_eq!(run(6, &setup), Ok(vec![1, 2, 3]));
    assert_eq!(
        usb.controls(),
        [ControlSetup {
            request_type: 0x80,
            request: 6,
            value: 0x0100,
            index: 0x1234,
        }]
    );
    assert_eq!(run(7, &joined(&[&setup, &[7, 7]])), Ok(le64(2).to_vec()));
    // Bulk and interrupt.
    assert_eq!(run(9, &[0x02, 1, 2, 3]), Ok(le64(3).to_vec()));
    assert_eq!(usb.written(), [(0, vec![7, 7]), (2, vec![1, 2, 3])]);
    usb.queue_in(&[4, 4]);
    assert_eq!(run(8, &[0x81]), Ok(vec![4, 4]));
    assert_eq!(run(8, &[0x81]), Err(Status::TIMED_OUT), "nothing queued");
    usb.queue_in(&[5]);
    assert_eq!(run(10, &[0x83]), Ok(vec![5]));

    // Isochronous: three packets of 8, of which one came whole, one short and one not at all.
    usb.queue_iso(&[1; 8]);
    usb.queue_iso(&[2, 2]);
    let mut expected = vec![1; 8];
    expected.extend([2, 2, 0, 0, 0, 0, 0, 0]);
    expected.extend([0; 8]);
    expected.extend([8, 1, 2, 1, 0, 0]);
    assert_eq!(run(11, &[0x81]), Ok(expected));

    // A reset that keeps the device, then one after which it is a different device: the handle
    // is dead, and says so on every call.
    assert_eq!(run(5, &[]), Ok(vec![]));
    assert_eq!((usb.resets(), usb.alternate(0)), (1, None));
    usb.reset_reenumerates(true);
    assert_eq!(run(5, &[]), Err(Status::NO_DEVICE));
    assert_eq!(run(0, &[]), Err(Status::NO_DEVICE));
}

#[test]
fn a_usb_function_is_enabled_answers_requests_and_moves_data() {
    let sim = SimBoard::new();
    let function = sim.add_usb_function("udc");
    let (host, thing) = thing(sim, r#"function = "udc""#);
    let run = |step, data: &[u8]| exercise(&host, thing, kind::USB_FUNCTION, step, data);
    let descriptors = usb_descriptors();

    assert_eq!(run(2, &[]), Err(Status::NO_DEVICE), "disabled");
    // A descriptor set that fails the check presents nothing.
    assert_eq!(run(0, &descriptors[..20]), Err(Status::INVALID_ARGUMENT));
    assert_eq!(run(0, &descriptors), Ok(vec![]));
    assert_eq!(function.descriptors(), Some(descriptors.clone()));
    assert_eq!(run(0, &descriptors), Err(Status::BUSY));
    assert_eq!(function.enables(), 1);

    // Events, in order, then none.
    assert_eq!(run(2, &[]), Ok(vec![0]));
    function.event(UsbFunctionEvent::Suspended);
    function.event(UsbFunctionEvent::Resumed);
    function.event(UsbFunctionEvent::Configured);
    assert_eq!(run(2, &[]), Ok(vec![3]));
    assert_eq!(run(2, &[]), Ok(vec![4]));
    assert_eq!(run(2, &[]), Ok(vec![1]));
    assert!(function.is_configured());

    // A request whose data goes to the host: at most its length is sent.
    let to_host = ControlSetup {
        request_type: 0xc0,
        request: 1,
        value: 2,
        index: 3,
    };
    function.request(to_host, 4, &[]);
    assert_eq!(run(2, &[]), Ok(vec![5, 0xc0, 1, 2, 0, 3, 0, 4, 0]));
    assert_eq!(run(3, &[9; 6]), Ok(le64(4).to_vec()));
    // One whose data comes from the host, and one that is refused.
    let from_host = ControlSetup {
        request_type: 0x40,
        request: 2,
        value: 0,
        index: 0,
    };
    function.request(from_host, 3, &[7, 8, 9]);
    assert_eq!(run(2, &[]), Ok(vec![5, 0x40, 2, 0, 0, 0, 0, 3, 0]));
    assert_eq!(run(4, &[3]), Ok(vec![7, 8, 9]));
    function.request(from_host, 0, &[]);
    assert_eq!(run(2, &[]).map(|event| event[0]), Ok(5));
    assert_eq!(run(5, &[]), Ok(vec![]));
    assert_eq!(
        run(5, &[]),
        Err(Status::INVALID_ARGUMENT),
        "nothing pending"
    );
    assert_eq!(
        function.answers(),
        [
            (to_host, ControlAnswer::In(vec![9; 4])),
            (from_host, ControlAnswer::Out),
            (from_host, ControlAnswer::Stalled),
        ]
    );

    // Halts and data on the endpoints.
    assert_eq!(run(6, &[0x81, 1]), Ok(vec![]));
    assert!(function.is_halted(0x81));
    assert_eq!(run(6, &[0x81, 0]), Ok(vec![]));
    assert!(!function.is_halted(0x81));
    assert_eq!(run(6, &[0x05, 1]), Err(Status::INVALID_ARGUMENT));
    function.host_sends(0x02, b"abc");
    assert_eq!(run(7, &[0x02]), Ok(b"abc".to_vec()));
    assert_eq!(run(7, &[0x02]), Err(Status::TIMED_OUT));
    assert_eq!(run(8, &[0x81, 1, 2]), Ok(le64(2).to_vec()));
    assert_eq!(function.host_received(), [(0x81, vec![1, 2])]);

    function.event(UsbFunctionEvent::Deconfigured);
    assert_eq!(run(2, &[]), Ok(vec![2]));
    assert_eq!(run(8, &[0x81, 1]), Err(Status::NO_DEVICE));
    assert_eq!(run(1, &[]), Ok(vec![]));
    assert_eq!(function.descriptors(), None);
}

#[test]
fn a_driver_given_everything_holds_it_and_gives_it_back() {
    let sim = SimBoard::new();
    sim.add_spi("a");
    sim.add_raw_spi("b");
    sim.add_line("c");
    sim.add_pwm("d");
    sim.add_uart("e");
    sim.add_usb_device("f", &[9, 2]);
    sim.add_usb_function("g");
    sim.add_registers("h", 16);
    sim.add_interrupt("i");
    sim.add_dma("j");
    sim.add_clocks("k");
    sim.add_reset("l");
    sim.add_stream("m", 2);
    sim.add_i2c("n");
    let names = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m",
    ];
    let board = sim.clone();
    let uses = r#"spi = "a", raw_spi = "b", line = "c", pwm = "d", uart = "e", usb = "f",
        function = "g", registers = "h", interrupt = "i", dma = "j", clocks = "k", reset = "l",
        stream = "m", bus = "n""#
        .replace('\n', " ");
    let (host, thing) = thing(sim, &uses);
    // Each is held by the driver, exclusively, through the host.
    assert!(names.iter().all(|name| board.is_claimed(name)));
    assert_eq!(
        exercise(&host, thing, kind::STREAM, 0, &[]),
        Ok(vec![2, 0, 0, 0])
    );
    assert_eq!(exercise(&host, thing, kind::LINE, 2, &[]), Ok(vec![0]));
    assert_eq!(host.call(thing, method(1), &[], &mut [0; 4]), Ok(4));

    // Letting go of the driver releases every one of them.
    host.shutdown();
    assert!(names.iter().all(|name| !board.is_claimed(name)));
}

#[test]
fn a_device_not_wired_to_a_resource_says_so() {
    let (host, thing) = thing(SimBoard::new(), "");
    for kind in [
        kind::SPI,
        kind::RAW_SPI,
        kind::LINE,
        kind::PWM,
        kind::UART,
        kind::USB_DEVICE,
        kind::USB_FUNCTION,
        kind::REGISTERS,
        kind::INTERRUPT,
        kind::DMA,
        kind::CLOCKS,
        kind::RESET,
        kind::STREAM,
    ] {
        assert_eq!(
            exercise(&host, thing, kind, 0, &[0; 24]),
            Err(Status::NOT_FOUND),
            "kind {kind}"
        );
    }
}
