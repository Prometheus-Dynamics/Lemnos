use std::thread;
use std::time::Duration;

use lemnos_core::{ErrorKind, Provider, Status};
use lemnos_hal::blocks::StreamFrame;
use lemnos_hal::i2c::I2c as _;
use lemnos_hal::raw::{Direction, LineConfig, PwmConfig};
use lemnos_hal::spi::SpiDevice as _;

use crate::{FifoLevel, Layout, SimBoard, SimClock, SimI2c};

#[test]
fn byte_targets_step_the_pointer_and_word_targets_dont() {
    let mut bus = SimI2c::new(SimClock::new());
    bus.attach(0x10, Layout::Bytes);
    bus.attach(0x40, Layout::Words);
    bus.set_bytes(0x10, 2, &[7, 8]);
    bus.set_word(0x40, 1, &[0xab, 0xcd]);

    let mut out = [0; 2];
    bus.write_read(0x10, &[2], &mut out).unwrap();
    assert_eq!(out, [7, 8]);
    bus.write(0x10, &[9, 1, 2]).unwrap();
    assert_eq!(bus.byte(0x10, 10), Some(2));

    bus.write_read(0x40, &[1], &mut out).unwrap();
    assert_eq!(out, [0xab, 0xcd]);
    bus.read(0x40, &mut out).unwrap();
    assert_eq!(out, [0xab, 0xcd]);
    bus.write(0x40, &[5, 1, 2]).unwrap();
    assert_eq!(bus.word(0x40, 5), Some([1, 2].to_vec()));
}

#[test]
fn a_missing_or_dead_target_does_not_acknowledge() {
    let mut bus = SimI2c::new(SimClock::new());
    assert_eq!(bus.write(0x11, &[0]), Err(ErrorKind::Nack));
    bus.attach(0x11, Layout::Bytes);
    bus.write(0x11, &[0]).unwrap();
    bus.set_dead(0x11, true);
    assert_eq!(bus.write(0x11, &[0]), Err(ErrorKind::Nack));
    bus.set_dead(0x11, false);
    bus.write(0x11, &[0]).unwrap();
}

#[test]
fn a_fifo_drains_and_reports_its_level() {
    let mut bus = SimI2c::new(SimClock::new());
    bus.attach(0x18, Layout::Bytes);
    bus.add_fifo(0x18, 0x24, FifoLevel::Bytes(0x0e), 64);
    bus.fifo_push(0x18, &[1, 2, 3]);

    let mut level = [0; 2];
    bus.write_read(0x18, &[0x0e], &mut level).unwrap();
    assert_eq!(u16::from_le_bytes(level), 3);
    let mut out = [0; 2];
    bus.write_read(0x18, &[0x24], &mut out).unwrap();
    assert_eq!(out, [1, 2]);
    assert_eq!(bus.fifo_len(0x18), 1);
    let mut more = [9; 2];
    bus.write_read(0x18, &[0x24], &mut more).unwrap();
    assert_eq!(more, [3, 0]);
}

#[test]
fn a_fifo_counts_frames_when_asked() {
    let mut bus = SimI2c::new(SimClock::new());
    bus.attach(0x18, Layout::Bytes);
    bus.add_fifo(
        0x18,
        0x24,
        FifoLevel::Frames {
            register: 0x0e,
            frame: 3,
        },
        64,
    );
    bus.fifo_push(0x18, &[0; 7]);
    let mut frames = [0];
    bus.write_read(0x18, &[0x0e], &mut frames).unwrap();
    assert_eq!(frames, [2]);
}

#[test]
fn a_fifo_fills_on_simulated_time_and_overflows() {
    let clock = SimClock::new();
    let bus = SimI2c::new(clock.clone());
    bus.attach(0x18, Layout::Bytes);
    bus.add_fifo(0x18, 0x24, FifoLevel::Bytes(0x0e), 8);
    bus.fifo_stream(0x18, Duration::from_millis(5), &[1, 2, 3, 4]);
    assert_eq!(bus.fifo_len(0x18), 0);
    clock.advance(Duration::from_millis(4));
    assert_eq!(bus.fifo_len(0x18), 0);
    clock.advance(Duration::from_millis(1));
    assert_eq!(bus.fifo_len(0x18), 4);
    clock.advance(Duration::from_millis(15));
    assert_eq!(bus.fifo_len(0x18), 8);
    assert_eq!(bus.fifo_overflowed(0x18), 2);
}

#[test]
fn a_queued_fault_is_returned_once() {
    let mut board = SimBoard::new();
    board.add_i2c("bus").attach(0x10, Layout::Bytes);
    let mut bus = board.i2c("bus").unwrap();
    board.faults("bus").unwrap().fail_next(ErrorKind::Timeout);
    assert_eq!(bus.write(0x10, &[0]), Err(ErrorKind::Timeout));
    assert_eq!(bus.write(0x10, &[0]), Ok(()));
}

#[test]
fn a_fault_script_replays_in_order_across_operations() {
    let mut board = SimBoard::new();
    board.add_line("led");
    let mut line = board.line("led").unwrap();
    let faults = board.faults("led").unwrap();
    faults.fail_next_op("set", ErrorKind::Busy);
    faults.fail_next_op("get", ErrorKind::Failed);

    // `get` isn't reached until the `set` fault ahead of it has fired.
    assert_eq!(line.get(), Ok(false));
    assert_eq!(line.set(true), Err(ErrorKind::Busy));
    assert_eq!(line.get(), Err(ErrorKind::Failed));
    assert_eq!(line.get(), Ok(false));
}

#[test]
fn an_outage_runs_on_simulated_time_then_clears() {
    let mut board = SimBoard::new();
    let clock = board.clock();
    board.add_i2c("bus").attach(0x10, Layout::Bytes);
    let mut bus = board.i2c("bus").unwrap();
    board.faults("bus").unwrap().outage(
        Duration::from_secs(6),
        Some(Duration::from_secs(3)),
        ErrorKind::Nack,
    );
    assert_eq!(bus.write(0x10, &[0]), Ok(()));
    clock.advance(Duration::from_secs(6));
    assert_eq!(bus.write(0x10, &[0]), Err(ErrorKind::Nack));
    clock.advance(Duration::from_secs(3));
    assert_eq!(bus.write(0x10, &[0]), Ok(()));
}

#[test]
fn an_exclusive_resource_has_one_owner_and_is_released_on_drop() {
    let mut board = SimBoard::new();
    board.add_pwm("fan");
    let first = board.pwm("fan").unwrap();
    assert!(board.is_claimed("fan"));
    assert_eq!(board.pwm("fan").err(), Some(Status::BUSY));
    drop(first);
    assert!(!board.is_claimed("fan"));
    assert!(board.pwm("fan").is_ok());
}

#[test]
fn a_bus_is_shared() {
    let mut board = SimBoard::new();
    board.add_i2c("bus");
    let _a = board.i2c("bus").unwrap();
    let _b = board.i2c("bus").unwrap();
    assert!(!board.is_claimed("bus"));
}

#[test]
fn a_missing_or_mismatched_resource_is_refused() {
    let mut board = SimBoard::new();
    board.add_uart("port");
    assert!(board.provided("port"));
    assert!(!board.provided("nothing"));
    assert_eq!(board.i2c("nothing").err(), Some(Status::NOT_FOUND));
    assert_eq!(board.i2c("port").err(), Some(Status::INVALID_ARGUMENT));
}

#[test]
fn unplugging_removes_the_resource() {
    let mut board = SimBoard::new();
    board.add_usb_device("cam", &[9]);
    assert!(board.provided("cam"));
    board.remove("cam");
    assert!(!board.provided("cam"));
    assert_eq!(board.usb_device("cam").err(), Some(Status::NOT_FOUND));
}

#[test]
fn every_resource_round_trips_through_the_provider() {
    let mut board = SimBoard::new();

    let device = board.add_spi("spi");
    device.respond(&[5]);
    let mut spi = board.spi("spi").unwrap();
    let mut read = [0];
    spi.transfer(&mut read, &[1]).unwrap();
    assert_eq!((read, device.written()), ([5], [[1].to_vec()].to_vec()));

    board.add_raw_spi("raw");
    board.raw_spi("raw").unwrap();

    let line = board.add_line("line");
    let mut handle = board.line("line").unwrap();
    let output = LineConfig {
        direction: Direction::Output,
        ..LineConfig::default()
    };
    handle.configure(&output).unwrap();
    handle.set(true).unwrap();
    assert_eq!(line.level(), Some(true));

    let pwm = board.add_pwm("pwm");
    let config = PwmConfig {
        period_ns: 1000,
        duty_ns: 500,
        enabled: true,
        ..PwmConfig::default()
    };
    board.pwm("pwm").unwrap().configure(&config).unwrap();
    assert_eq!(pwm.current(), config);

    let uart = board.add_uart("uart");
    let mut port = board.uart("uart").unwrap();
    port.write(&[1, 2]).unwrap();
    assert_eq!(uart.sent(), [1, 2]);

    board.add_usb_device("usb", &[9, 2]);
    let mut usb = board.usb_device("usb").unwrap();
    let mut descriptor = [0; 4];
    assert_eq!(usb.config_descriptor(0, &mut descriptor), Ok(2));

    let function = board.add_usb_function("gadget");
    board.usb_function("gadget").unwrap().enable(&[1]).unwrap();
    assert_eq!(function.descriptors(), Some([1].to_vec()));

    let regs = board.add_registers("regs", 16);
    board.registers("regs").unwrap().write32(4, 7).unwrap();
    assert_eq!(regs.writes(), [(4, 7)]);

    board.add_interrupt("irq");
    board.interrupt("irq").unwrap().enable().unwrap();

    board.add_dma("dma");
    let mut dma = board.dma("dma").unwrap();
    let buffer = dma.allocate(4).unwrap();
    dma.write(buffer.id, 0, &[1]).unwrap();

    let clocks = board.add_clocks("clk");
    board.clocks("clk").unwrap().enable(1).unwrap();
    assert!(clocks.enabled(1));

    let reset = board.add_reset("rst");
    board.reset("rst").unwrap().assert(0).unwrap();
    assert_eq!(reset.history(), [(0, true)]);

    let stream = board.add_stream("frames", 2);
    let mut endpoint = board.stream("frames").unwrap();
    let frame = StreamFrame {
        buffer: 0,
        bytes: 1,
        sequence: 1,
        identity: 2,
        timestamp_ns: 3,
    };
    endpoint.publish(frame).unwrap();
    assert_eq!(stream.take_frame(), Some(frame));
}

#[test]
fn unrelated_resources_work_from_different_threads() {
    let mut board = SimBoard::new();
    board.add_i2c("a").attach(0x10, Layout::Bytes);
    board.add_i2c("b").attach(0x10, Layout::Bytes);
    let threads: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|name| {
            let mut bus = board.i2c(name).unwrap();
            thread::spawn(move || {
                for _ in 0..1000 {
                    bus.write(0x10, &[0]).unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}
