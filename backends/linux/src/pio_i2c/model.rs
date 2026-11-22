//! A cycle model of one RP1 PIO state machine running the I2C program, loaded the way the RP1
//! firmware loads it: the image goes in at its origin and every JMP target in it is moved by that
//! origin. (The Raspberry Pi kernel's own PIO drivers and piolib hand the firmware origin-0 images
//! with "any origin" and never move a jump themselves, so the firmware does.) Exec'd words, `out
//! pc` targets, the wrap and the start PC are not moved: they are absolute.
//!
//! SCL and SDA are open drain with pull-ups and no target on the bus, so SCL is high whenever the
//! machine releases it and every ACK reads as a NACK. Delays are not modelled: nothing else drives
//! the lines, so they change no outcome here.

use std::collections::VecDeque;

use super::program::{PROGRAM_LEN, idle_wrap, initial_pc, program};
use super::protocol::{Op, encode, rx_word_count};

const SDA: u16 = 8;
const SCL: u16 = 7;
/// The OUT threshold of `sm_config` (`jmp !osre`).
const PULL_THRESHOLD: u32 = 14;
const STEPS: usize = 100_000;

/// What the RP1 firmware does with `PIO_ADD_PROGRAM` at `origin`.
fn firmware_load(memory: &mut [u16; 32], origin: u16, image: &[u16]) {
    for (i, &word) in image.iter().enumerate() {
        let moved = if word >> 13 == 0 {
            (word & !0x1f) | ((word + origin) & 0x1f)
        } else {
            word
        };
        memory[usize::from(origin) + i] = moved;
    }
}

struct Machine {
    memory: [u16; 32],
    pc: u16,
    wrap: (u16, u16),
    osr: u32,
    shifted: u32,
    isr: u32,
    x: u32,
    exec: Option<u16>,
    sda_driven: bool,
    scl_driven: bool,
    tx: VecDeque<u32>,
    rx: Vec<u32>,
    /// (SCL, SDA) levels after every step.
    trace: Vec<(bool, bool)>,
}

impl Machine {
    fn new(base: u16) -> Self {
        let mut memory = [0u16; 32];
        firmware_load(&mut memory, base, &program(SCL));
        Machine {
            memory,
            pc: initial_pc(base),
            wrap: idle_wrap(base),
            osr: 0,
            shifted: 32,
            isr: 0,
            x: 0,
            exec: None,
            sda_driven: false,
            scl_driven: false,
            tx: VecDeque::new(),
            rx: Vec::new(),
            trace: Vec::new(),
        }
    }

    fn level(&self, gpio: u16) -> bool {
        match gpio {
            SCL => !self.scl_driven,
            SDA => !self.sda_driven,
            _ => false,
        }
    }

    fn advance(&mut self) {
        self.pc = if self.pc == self.wrap.1 {
            self.wrap.0
        } else {
            (self.pc + 1) & 0x1f
        };
    }

    /// One instruction. False when the machine is stalled on an empty TX FIFO.
    fn step(&mut self) -> bool {
        let (instr, exec) = match self.exec.take() {
            Some(instr) => (instr, true),
            None => (self.memory[usize::from(self.pc)], false),
        };
        // Side-set (one bit, pin direction) applies on the instruction's first cycle, stalled or not.
        self.scl_driven = (instr >> 12) & 1 == 1;
        let ran = self.run(instr, exec);
        self.trace.push((self.level(SCL), self.level(SDA)));
        ran
    }

    fn run(&mut self, instr: u16, exec: bool) -> bool {
        let next = |m: &mut Machine| {
            if !exec {
                m.advance();
            }
        };
        match instr >> 13 {
            0 => {
                let taken = match (instr >> 5) & 7 {
                    0b000 => true,
                    0b001 => self.x == 0,
                    0b111 => self.shifted < PULL_THRESHOLD,
                    cond => panic!("jmp condition {cond:#b} is not in the program"),
                };
                if taken {
                    self.pc = instr & 0x1f;
                } else {
                    next(self);
                }
            }
            1 => {
                assert_eq!((instr >> 5) & 3, 0, "only `wait gpio` is in the program");
                if self.level(instr & 0x1f) != ((instr >> 7) & 1 == 1) {
                    if exec {
                        self.exec = Some(instr);
                    }
                    return true;
                }
                next(self);
            }
            2 => {
                assert_eq!(instr & 0xff, 0x01, "only `in pins, 1` is in the program");
                self.isr = (self.isr << 1) | u32::from(self.level(SDA));
                next(self);
            }
            3 => {
                let count = u32::from(instr & 0x1f);
                let data = self.osr >> (32 - count);
                self.osr = self.osr.checked_shl(count).unwrap_or(0);
                self.shifted = (self.shifted + count).min(32);
                match (instr >> 5) & 7 {
                    0b001 => {
                        self.x = data;
                        next(self);
                    }
                    0b100 => {
                        self.sda_driven = data & 1 == 1;
                        next(self);
                    }
                    0b101 => self.pc = data as u16 & 0x1f,
                    0b111 => {
                        self.exec = Some(data as u16);
                        next(self);
                    }
                    dst => panic!("out destination {dst:#b} is not in the program"),
                }
            }
            4 if instr & 0x80 != 0 => {
                let Some(word) = self.tx.pop_front() else {
                    if exec {
                        self.exec = Some(instr);
                    }
                    return false;
                };
                self.osr = word;
                self.shifted = 0;
                next(self);
            }
            4 => {
                self.rx.push(self.isr);
                self.isr = 0;
                next(self);
            }
            5 => {
                assert_eq!(
                    instr & 0xff,
                    0x63,
                    "only `mov pindirs, null` is in the program"
                );
                self.sda_driven = false;
                next(self);
            }
            op => panic!("instruction class {op} is not in the program"),
        }
        true
    }

    /// Runs until the machine waits on an empty TX FIFO, or gives up after `STEPS`.
    fn run_words(&mut self, words: &[u32]) -> Result<(), String> {
        self.tx.extend(words.iter().copied());
        for _ in 0..STEPS {
            if !self.step() {
                return Ok(());
            }
        }
        Err(format!(
            "stuck at pc {} with {} of {} TX words left, SCL {}",
            self.pc,
            self.tx.len(),
            words.len(),
            if self.level(SCL) { "high" } else { "low" }
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wire {
    Start,
    Stop,
    Bit(bool),
}

/// The wire as a target sees it: a bit at each SCL rise, a START or STOP when SDA moves while SCL
/// is high (in place of the bit its SCL rise began).
fn wire(trace: &[(bool, bool)]) -> Vec<Wire> {
    let mut out = Vec::new();
    let mut tentative = false;
    let mut prev = (true, true);
    for &(scl, sda) in trace {
        if !prev.0 && scl {
            out.push(Wire::Bit(sda));
            tentative = true;
        } else if prev.0 && scl && prev.1 != sda {
            if tentative {
                out.pop();
            }
            out.push(if sda { Wire::Stop } else { Wire::Start });
            tentative = false;
        } else if !scl {
            tentative = false;
        }
        prev = (scl, sda);
    }
    out
}

fn byte(value: u8) -> impl Iterator<Item = Wire> {
    (0..8).rev().map(move |i| Wire::Bit(value >> i & 1 == 1))
}

/// Each origin the program can be loaded at.
fn origins() -> impl Iterator<Item = u16> {
    0..=(32 - PROGRAM_LEN as u16)
}

/// On the board the IMU's machine loaded at origin 4, beside the LED ring's program at 0..3. It
/// pulled SCL low about 50 ms after the pins went to the PIO, held it there, and never took its
/// TX words: every jump the firmware had moved landed 4 past its target, and the jump back to the
/// low loop became a jump to itself.
#[test]
fn a_write_runs_to_its_stop_at_every_origin_the_firmware_loads_it() {
    let ops = [Op::Start, Op::Write(0x32), Op::Stop];
    for base in origins() {
        let mut machine = Machine::new(base);
        machine
            .run_words(&encode(base, SCL, &ops))
            .unwrap_or_else(|why| panic!("origin {base}: {why}"));
        let expected: Vec<Wire> = [Wire::Start]
            .into_iter()
            .chain(byte(0x32))
            .chain([Wire::Bit(true), Wire::Stop])
            .collect();
        assert_eq!(wire(&machine.trace), expected, "origin {base}");
        assert_eq!(machine.rx.len(), rx_word_count(&ops), "origin {base}");
        assert!(
            machine.level(SCL) && machine.level(SDA),
            "origin {base}: the bus idles released"
        );
        let (bottom, top) = idle_wrap(base);
        assert!(
            (bottom..=top).contains(&machine.pc),
            "origin {base}: back in the released loop"
        );
    }
}

#[test]
fn a_register_read_with_a_repeated_start_runs_at_every_origin() {
    let ops = [
        Op::Start,
        Op::Write(0x32),
        Op::Write(0x00),
        Op::Start,
        Op::Write(0x33),
        Op::Read { ack: true },
        Op::Read { ack: false },
        Op::Stop,
    ];
    for base in origins() {
        let mut machine = Machine::new(base);
        machine
            .run_words(&encode(base, SCL, &ops))
            .unwrap_or_else(|why| panic!("origin {base}: {why}"));
        let nack = Wire::Bit(true);
        let expected: Vec<Wire> = [Wire::Start]
            .into_iter()
            .chain(byte(0x32))
            .chain([nack])
            .chain(byte(0x00))
            .chain([nack, Wire::Start])
            .chain(byte(0x33))
            .chain([nack])
            // Nothing drives SDA, so both bytes read 0xff; the master ACKs the first.
            .chain(byte(0xff))
            .chain([Wire::Bit(false)])
            .chain(byte(0xff))
            .chain([nack, Wire::Stop])
            .collect();
        assert_eq!(wire(&machine.trace), expected, "origin {base}");
        assert_eq!(machine.rx.len(), rx_word_count(&ops), "origin {base}");
        assert_eq!(
            machine.rx[2] & 0xff,
            0xff,
            "origin {base}: the first read byte"
        );
    }
}

/// The image handed to the firmware is origin-relative: its jumps stay inside `0..PROGRAM_LEN`.
#[test]
fn the_loaded_image_jumps_only_inside_itself_from_origin_zero() {
    for (i, &word) in program(SCL).iter().enumerate() {
        if word >> 13 == 0 {
            assert!(usize::from(word & 0x1f) < PROGRAM_LEN, "instruction {i}");
        }
    }
}
