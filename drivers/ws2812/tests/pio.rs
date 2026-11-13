//! The strip on a PIO output: the bit stream a cycle-level model of the program shifts out for a
//! known frame, the reset gap, the frame kept across a failed transfer, and the program's own
//! words. The model runs the four instructions as the machine does (one cycle per step, a side-set
//! on each instruction's first cycle, delay cycles after it, autopull from the FIFO), so the bits
//! are checked against the wire rather than against the encoder.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use lemnos_contract_pixels::Pixels;
use lemnos_core::{
    Config, ConfigValue, Driver, Held, LocalCaller, Pio, Provider, Resources, Spi, Status, Tag,
};
use lemnos_driver_ws2812::{PROGRAM, Ws2812};
use lemnos_hal::ErrorKind;
use lemnos_hal::pio::{PioJob, PioMachine, PioOperation};

/// One operation the fake machine was handed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    Write(Vec<u32>),
    Delay(u32),
}

#[derive(Default)]
struct State {
    loads: Vec<(Vec<u16>, u8, u8, u8)>,
    transactions: Vec<Vec<Op>>,
    /// Transactions that fail, the next ones.
    fail_next: usize,
}

/// A machine that records what it is given and fails the next few transactions on request.
#[derive(Clone)]
struct Fake(Arc<Mutex<State>>);

impl PioMachine for Fake {
    fn load(&mut self, job: &PioJob<'_>) -> Result<(), ErrorKind> {
        self.0.lock().unwrap().loads.push((
            job.program.to_vec(),
            job.wrap_bottom,
            job.wrap_top,
            job.out_bits,
        ));
        Ok(())
    }

    fn transaction(&mut self, operations: &mut [PioOperation<'_>]) -> Result<(), ErrorKind> {
        let mut state = self.0.lock().unwrap();
        let log = operations
            .iter()
            .map(|op| match op {
                PioOperation::Write(words) => Op::Write(words.to_vec()),
                PioOperation::DelayNs(ns) => Op::Delay(*ns),
            })
            .collect();
        state.transactions.push(log);
        if state.fail_next > 0 {
            state.fail_next -= 1;
            return Err(ErrorKind::Busy);
        }
        Ok(())
    }
}

/// A board with one PIO output and no SPI device.
struct Board(Option<Pio>);

impl Provider for Board {
    fn provided(&self, _need: &str) -> bool {
        true
    }

    fn device(&mut self, _need: &str, _tag: Tag) -> lemnos_core::Result<LocalCaller> {
        Err(Status::NOT_FOUND)
    }

    fn spi(&mut self, _need: &str) -> lemnos_core::Result<Spi> {
        Err(Status::NOT_FOUND)
    }

    fn pio(&mut self, _need: &str) -> lemnos_core::Result<Pio> {
        self.0.take().ok_or(Status::NOT_FOUND)
    }
}

fn bind(count: i64, wire: &str, state: &Arc<Mutex<State>>) -> Ws2812 {
    let machine: Pio = Held::boxed(Box::new(Fake(Arc::clone(state))));
    let mut board = Board(Some(machine));
    let mut res = Resources::new(&mut board, None);
    let entries = [
        ("count", ConfigValue::Int(count)),
        ("wire", ConfigValue::Str(wire)),
    ];
    Ws2812::bind(&mut res, &Config::new(&entries)).unwrap()
}

/// What the model's pin shows, one entry per machine cycle.
fn wave(words: &[u32], out_bits: u8) -> Vec<bool> {
    let mut fifo: VecDeque<u32> = words.iter().copied().collect();
    let out_bits = u32::from(out_bits);
    let (mut pc, mut delay, mut osr, mut count, mut x, mut pin) =
        (0usize, 0u32, 0u32, out_bits, 0u32, false);
    let mut pins = Vec::new();
    for _ in 0..(words.len() as u32 * out_bits * 10 + 100) {
        if delay > 0 {
            delay -= 1;
            pins.push(pin);
            continue;
        }
        let ins = PROGRAM[pc];
        let side = (ins >> 12) & 1 == 1;
        let cycles = u32::from((ins >> 8) & 0xf);
        let mut next = pc + 1;
        match ins >> 13 {
            0b011 => {
                // out x, 1: pull a word when the shifter has emptied, stalling if none is queued.
                if count >= out_bits {
                    match fifo.pop_front() {
                        Some(word) => {
                            osr = word;
                            count = 0;
                        }
                        None => {
                            pins.push(pin);
                            continue;
                        }
                    }
                }
                x = (osr >> 31) & 1;
                osr <<= 1;
                count += 1;
            }
            0b000 => {
                let cond = (ins >> 5) & 7;
                let taken = cond == 0 || (cond == 1 && x == 0);
                if taken {
                    next = usize::from(ins & 0x1f);
                }
            }
            _ => {}
        }
        pc = next;
        if pc > 3 {
            pc = 0;
        }
        pin = side;
        pins.push(pin);
        delay = cycles;
    }
    pins
}

/// The bits of each high run, which must be 2 or 7 cycles long, one run per bit, each starting
/// ten cycles after the one before (the first at cycle 3, after the bit's shift).
fn decode(pins: &[bool]) -> Vec<bool> {
    let mut bits = Vec::new();
    let mut i = 0;
    while i < pins.len() {
        if !pins[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < pins.len() && pins[i] {
            i += 1;
        }
        let high = i - start;
        assert!(
            high == 2 || high == 7,
            "a high run of {high} cycles is neither a 0 nor a 1"
        );
        assert_eq!(
            start,
            3 + 10 * bits.len(),
            "bit {} starts off its ten-cycle slot",
            bits.len()
        );
        bits.push(high == 7);
    }
    bits
}

/// The bits of `bytes`, most significant bit of the first byte first.
fn bits_of(bytes: &[u8]) -> Vec<bool> {
    bytes
        .iter()
        .flat_map(|&byte| (0..8).rev().map(move |bit| (byte >> bit) & 1 == 1))
        .collect()
}

fn last_write(state: &Arc<Mutex<State>>) -> Vec<u32> {
    let state = state.lock().unwrap();
    match state.transactions.last().unwrap().first().unwrap() {
        Op::Write(words) => words.clone(),
        other => panic!("first operation was {other:?}"),
    }
}

#[test]
fn an_rgb_led_shifts_green_red_blue_then_a_zero_padding() {
    let state = Arc::new(Mutex::new(State::default()));
    let mut driver = bind(1, "rgb", &state);
    driver.set(0, &[0x12, 0x34, 0x57, 0]).unwrap();
    driver.show().unwrap();
    let words = last_write(&state);
    assert_eq!(words, vec![0x3412_5700, 0]);
    // Green 0x34, red 0x12, blue 0x57 (its last bit set, so the padding must hold the line low).
    let expected = [bits_of(&[0x34, 0x12, 0x57]), bits_of(&[0, 0, 0])].concat();
    let pins = wave(&words, 24);
    assert_eq!(decode(&pins), expected);
}

#[test]
fn an_rgbw_led_shifts_four_bytes_and_the_padding_is_32_zero_bits() {
    let state = Arc::new(Mutex::new(State::default()));
    let mut driver = bind(1, "rgbw", &state);
    driver.set(0, &[0x12, 0x34, 0x56, 0x78]).unwrap();
    driver.show().unwrap();
    let words = last_write(&state);
    assert_eq!(words, vec![0x3412_5678, 0]);
    let expected = [bits_of(&[0x34, 0x12, 0x56, 0x78]), bits_of(&[0; 4])].concat();
    assert_eq!(decode(&wave(&words, 32)), expected);
}

#[test]
fn the_line_is_low_after_the_frame_so_the_reset_holds() {
    let state = Arc::new(Mutex::new(State::default()));
    let mut driver = bind(2, "rgb", &state);
    driver.set(0, &[0xff, 0xff, 0xff, 0]).unwrap();
    driver.set(1, &[0x00, 0x00, 0x01, 0]).unwrap();
    driver.show().unwrap();
    let words = last_write(&state);
    let pins = wave(&words, 24);
    // The model stalls at the next bit's shift with the line low: the reset starts there.
    assert!(!pins[pins.len() - 1], "the line is high at the stall");
    // The driver asks for the reset plus the words still queued after the hand-off.
    let state = state.lock().unwrap();
    let ops = state.transactions.last().unwrap();
    assert_eq!(ops[1], Op::Delay(80_000 + 5 * 24 * 1_250));
}

#[test]
fn a_failed_transfer_leaves_the_frame_for_the_next_show() {
    let state = Arc::new(Mutex::new(State::default()));
    let mut driver = bind(1, "rgb", &state);
    driver.set(0, &[0x12, 0x34, 0x57, 0]).unwrap();
    state.lock().unwrap().fail_next = 1;
    assert_eq!(Pixels::show(&mut driver), Err(Status::BUSY));
    let first = last_write(&state);
    assert_eq!(Pixels::show(&mut driver), Ok(()));
    let second = last_write(&state);
    assert_eq!(first, second, "the second show sends the same frame");
    let state = state.lock().unwrap();
    assert_eq!(
        state.transactions.len(),
        3,
        "load-off, the failed show, then the retry"
    );
}

#[test]
fn the_program_is_loaded_with_its_wrap_and_wire_width() {
    let state = Arc::new(Mutex::new(State::default()));
    let _ = bind(1, "rgbw", &state);
    let state = state.lock().unwrap();
    let (program, bottom, top, bits) = state.loads[0].clone();
    assert_eq!(program, PROGRAM.to_vec());
    assert_eq!((bottom, top, bits), (0, 3, 32));
}

#[test]
fn the_program_words_are_the_documented_instructions() {
    // out x,1 side 0 [2]; jmp !x,3 side 1 [1]; jmp 0 side 1 [4]; mov y,y side 0 [4].
    assert_eq!(PROGRAM, [0x6221, 0x1123, 0x1400, 0xa442]);
}
