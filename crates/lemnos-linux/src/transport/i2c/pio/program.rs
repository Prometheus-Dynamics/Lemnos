//! The PIO I2C master program and the words that drive it.
//!
//! Pins: SCL is `scl`, driven by side-set as a pin direction (driven low, or
//! released high with the board's pull-up). SDA is `sda`, the OUT and IN group,
//! also direction only. Both are open-drain with the output latch forced low.
//! The side-set is non-optional, so every instruction sets SCL.
//!
//! The bus idles with SCL and SDA released, as I2C requires. START, repeated
//! START and STOP are not in the program: the host sends them as exec words that
//! the machine runs in place (`out exec, 16`, as pico-examples' i2c.pio does).
//!
//! Side-set applies to exec words too, and `out exec` runs with its own side
//! value before the instruction it executes. So the two exec loops keep that
//! value equal to the SCL level: the released loop (`REL_LOOP`) runs with SCL
//! released, the low loop (`LOW_LOOP`) with SCL low. A word that changes SCL is
//! an exec'd `jmp` whose side value changes SCL and moves the machine to the
//! loop for the new level, so no `out exec` ever runs at the wrong level.
//!
//! Word layout (TX FIFO, 32 bits, MSB first):
//! - bit 31 = 1: an exec word. Bits 30..15 are a 16-bit instruction, run in
//!   place. For `out pindirs, 1` (SDA), bit 14 is the value (1 = drive low).
//! - bit 31 = 0: a data word (low loop only). Bits 30..26 are the entry
//!   (`base + op`, the `out pc, 5` target), then the payload:
//!   - WRITE (op 0): the byte inverted in bits 25..18 (bit 1 = SDA released).
//!   - READ (op 1): zero bits 25..18, then bit 17 = ACK (1) or NACK (0).
//!
//! Every START and STOP pushes one RX word (the ACK bits of the WRITEs since
//! the previous push, newest in bit 0). Each READ pushes one RX word (the byte
//! in bits 7:0; bit 8 holds the ACK of the address write before it).
//!
//! The program is 27 instructions. It fits at offset 4, beside `ws2812-pio-rp1`
//! (four slots at 0..3).

/// Instructions in the program.
pub const PROGRAM_LEN: usize = 27;
/// Entry offset of the WRITE op, from the program base.
pub const OP_WRITE: u16 = 0;
/// Entry offset of the READ op.
pub const OP_READ: u16 = 1;
/// Maximum WRITEs between pushes (the 32-bit ISR holds the ACK bits).
pub const MAX_WRITES_PER_PUSH: usize = 32;

/// Side-set: SCL driven low (direction output, latch forced low).
const LOW: bool = true;
/// Side-set: SCL released (direction input; the pull-up takes it high).
const REL: bool = false;

const COND_ALWAYS: u16 = 0b000;
const COND_NOT_X: u16 = 0b001;
const COND_NOT_OSRE: u16 = 0b111;

const DST_PINDIRS: u16 = 0b100;
const DST_PC: u16 = 0b101;
const DST_EXEC: u16 = 0b111;
/// SET's destination code for PINDIRS (used by nothing here; kept for the SCL pin).
const MOV_PINDIRS: u16 = 0b011;

// Offsets from the base.
const LOW_PULL: u16 = 2;
const DATA: u16 = 7;
const REL_PULL: u16 = 8;
const WRITE_BODY: u16 = 11;
const READ_BODY: u16 = 18;
/// The released loop wraps from its `out exec` back to its pull.
const REL_WRAP_TOP: u16 = 10;

/// The delay and side-set field: side value in bit 4, delay (0..=15) in 3:0.
fn ds(side: bool, delay: u16) -> u16 {
    debug_assert!(delay <= 15);
    (u16::from(side) << 4) | delay
}

fn jmp(cond: u16, addr: u16, side: bool, delay: u16) -> u16 {
    (ds(side, delay) << 8) | (cond << 5) | addr
}

fn wait_gpio_high(pin: u16, side: bool, delay: u16) -> u16 {
    0x2000 | (ds(side, delay) << 8) | (1 << 7) | pin
}

fn in_pin(side: bool, delay: u16) -> u16 {
    0x4000 | (ds(side, delay) << 8) | 1
}

fn out_1(dst: u16, side: bool, delay: u16) -> u16 {
    0x6000 | (ds(side, delay) << 8) | (dst << 5) | 1
}

fn out_5(dst: u16, side: bool, delay: u16) -> u16 {
    0x6000 | (ds(side, delay) << 8) | (dst << 5) | 5
}

fn out_16(side: bool, delay: u16) -> u16 {
    0x6000 | (ds(side, delay) << 8) | (DST_EXEC << 5) | 16
}

/// `out x, 1` (the flag bit).
fn out_x1(side: bool) -> u16 {
    0x6000 | (ds(side, 0) << 8) | (1 << 5) | 1
}

fn push_block(side: bool, delay: u16) -> u16 {
    0x8020 | (ds(side, delay) << 8)
}

fn pull_block(side: bool) -> u16 {
    0x80a0 | (ds(side, 0) << 8)
}

fn mov_null(side: bool, delay: u16) -> u16 {
    0xa000 | (ds(side, delay) << 8) | (MOV_PINDIRS << 5) | 0b011
}

/// The program, loaded at instruction offset `base`.
pub fn program(base: u16, scl: u16) -> [u16; PROGRAM_LEN] {
    let b = base;
    let low_pull = b + LOW_PULL;
    let data = b + DATA;
    let write = b + WRITE_BODY;
    let read = b + READ_BODY;
    let mut p = [0u16; PROGRAM_LEN];
    // 0..=1: the op table. Entries base+0 and base+1 are the WRITE and READ bodies.
    p[0] = jmp(COND_ALWAYS, write, LOW, 0);
    p[1] = jmp(COND_ALWAYS, read, LOW, 0);
    // 2..=7: low loop (SCL low). Flag 0 jumps to the entry; flag 1 runs an exec word.
    p[2] = pull_block(LOW);
    p[3] = out_x1(LOW);
    p[4] = jmp(COND_NOT_X, data, LOW, 0);
    p[5] = out_16(LOW, 0);
    p[6] = jmp(COND_ALWAYS, low_pull, LOW, 0);
    p[7] = out_5(DST_PC, LOW, 0);
    // 8..=10: released loop (SCL released). Exec words only; wraps to its pull.
    p[8] = pull_block(REL);
    p[9] = out_x1(REL);
    p[10] = out_16(REL, 0);
    // 11..=17: WRITE bit loop, ACK, back to the low loop.
    p[11] = out_1(DST_PINDIRS, LOW, 7);
    p[12] = wait_gpio_high(scl, REL, 7);
    p[13] = jmp(COND_NOT_OSRE, write, LOW, 7);
    p[14] = out_1(DST_PINDIRS, LOW, 7);
    p[15] = wait_gpio_high(scl, REL, 7);
    p[16] = in_pin(REL, 7);
    p[17] = jmp(COND_ALWAYS, low_pull, LOW, 15);
    // 18..=26: READ bit loop, push, ACK, release SDA, back to the low loop.
    p[18] = out_1(DST_PINDIRS, LOW, 7);
    p[19] = wait_gpio_high(scl, REL, 0);
    p[20] = in_pin(REL, 6);
    p[21] = jmp(COND_NOT_OSRE, read, LOW, 7);
    p[22] = push_block(LOW, 0);
    p[23] = out_1(DST_PINDIRS, LOW, 7);
    p[24] = wait_gpio_high(scl, REL, 7);
    p[25] = mov_null(LOW, 7);
    p[26] = jmp(COND_ALWAYS, low_pull, LOW, 0);
    p
}

/// The wrap window of the released loop: (bottom, top) instruction offsets.
pub fn idle_wrap(base: u16) -> (u16, u16) {
    (base + REL_PULL, base + REL_WRAP_TOP)
}

/// The entry of the released loop, where a fresh state machine starts (SCL released).
pub fn initial_pc(base: u16) -> u16 {
    base + REL_PULL
}

/// The `out pc, 5` target value of WRITE and READ.
pub fn entry(base: u16, op: u16) -> u32 {
    u32::from(base + op)
}

/// The data word for one WRITE of `byte`.
pub fn write_word(base: u16, byte: u8) -> u32 {
    (entry(base, OP_WRITE) << 26) | (u32::from(!byte) << 18)
}

/// The data word for one READ, with the ACK (`true`) or NACK bit.
pub fn read_word(base: u16, ack: bool) -> u32 {
    (entry(base, OP_READ) << 26) | (u32::from(ack) << 17)
}

/// An exec word carrying `instruction`.
pub fn exec_word(instruction: u16) -> u32 {
    (1 << 31) | (u32::from(instruction) << 15)
}

/// An exec word that drives SDA through `out pindirs, 1` (low = drive low,
/// high = release). Its bit is bit 14 (the OSR's next bit after the instruction).
/// `side` is the SCL level the instruction holds (side-set applies to it).
pub fn exec_sda(low: bool, side: bool, delay: u16) -> u32 {
    exec_word(out_1(DST_PINDIRS, side, delay)) | (u32::from(low) << 14)
}

/// `wait 1 gpio <scl>` as an exec word, with SCL released.
pub fn wait_scl_high(scl: u16, delay: u16) -> u32 {
    exec_word(wait_gpio_high(scl, REL, delay))
}

/// `push block` as an exec word at the given SCL level.
pub fn push(side: bool) -> u32 {
    exec_word(push_block(side, 0))
}

/// A jump to `addr` as an exec word that also sets SCL to `side`.
pub fn jump_to(addr: u16, side: bool, delay: u16) -> u32 {
    exec_word(jmp(COND_ALWAYS, addr, side, delay))
}

/// The exec words of a START. From idle (released loop) SDA falls while SCL is
/// released, then SCL goes low and the machine moves to the low loop. A
/// repeated START (low loop) releases SDA, then moves to the released loop,
/// waits for SCL to rise, drops SDA, and returns to the low loop with SCL low.
pub fn start_words(base: u16, scl: u16, from_idle: bool) -> Vec<u32> {
    let low_pull = base + LOW_PULL;
    let rel_pull = base + REL_PULL;
    if from_idle {
        vec![
            push(REL),
            exec_sda(true, REL, 15),
            jump_to(low_pull, LOW, 15),
        ]
    } else {
        vec![
            push(LOW),
            exec_sda(false, LOW, 7),
            jump_to(rel_pull, REL, 7),
            wait_scl_high(scl, 15),
            exec_sda(true, REL, 15),
            jump_to(low_pull, LOW, 15),
        ]
    }
}

/// The exec words of a STOP from the low loop (SCL low): SDA low, SCL released
/// (moving to the released loop), wait for SCL, then SDA released while SCL is
/// high. The machine stays in the released loop: the bus idles released.
pub fn stop_words(base: u16, scl: u16) -> Vec<u32> {
    vec![
        push(LOW),
        exec_sda(true, LOW, 7),
        jump_to(base + REL_PULL, REL, 0),
        wait_scl_high(scl, 15),
        exec_sda(false, REL, 15),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_encodings_match_the_rp2040_datasheet() {
        assert_eq!(pull_block(REL), 0x80a0);
        assert_eq!(push_block(REL, 0), 0x8020);
        assert_eq!(wait_gpio_high(7, REL, 0), 0x2087);
        assert_eq!(out_1(DST_PINDIRS, REL, 0), 0x6081);
        assert_eq!(in_pin(REL, 0), 0x4001);
        assert_eq!(out_5(DST_PC, REL, 0), 0x60a5);
        assert_eq!(jmp(COND_ALWAYS, 0, REL, 0), 0x0000);
        assert_eq!(jmp(COND_NOT_OSRE, 0, REL, 0), 0x00e0);
        assert_eq!(out_x1(REL), 0x6021);
        assert_eq!(mov_null(REL, 0), 0xa063);
    }

    #[test]
    fn side_set_and_delay_fill_the_five_bit_field() {
        assert_eq!(ds(LOW, 15), 0x1f);
        assert_eq!(ds(REL, 0), 0x00);
        assert_eq!(ds(LOW, 7), 0x17);
    }

    #[test]
    fn program_jump_targets_stay_inside_the_program() {
        for base in 0..=5u16 {
            let p = program(base, 7);
            assert_eq!(p.len(), PROGRAM_LEN);
            assert!(usize::from(base) + PROGRAM_LEN <= 32);
            for (i, &word) in p.iter().enumerate() {
                if word >> 13 == 0 {
                    let target = word & 0x1f;
                    assert!(
                        (base..base + PROGRAM_LEN as u16).contains(&target),
                        "instruction {i} jumps to {target}, outside base {base}"
                    );
                }
            }
            assert_eq!(p[0] & 0x1f, base + WRITE_BODY);
            assert_eq!(p[1] & 0x1f, base + READ_BODY);
            assert_eq!(initial_pc(base), base + REL_PULL);
            assert_eq!(idle_wrap(base), (base + REL_PULL, base + REL_WRAP_TOP));
        }
    }

    #[test]
    fn data_and_exec_words_have_their_layout() {
        let base = 4;
        assert_eq!(write_word(base, 0xff) >> 26, u32::from(base));
        assert_eq!((write_word(base, 0xff) >> 18) & 0xff, 0x00);
        assert_eq!((write_word(base, 0x00) >> 18) & 0xff, 0xff);
        assert_eq!(read_word(base, true) >> 26, u32::from(base + 1));
        assert_eq!(read_word(base, true) & (1 << 17), 1 << 17);
        assert_eq!(exec_word(0x80a0) >> 31, 1);
        assert_eq!((exec_word(0x80a0) >> 15) & 0xffff, 0x80a0);
        assert_eq!(exec_sda(true, REL, 0) & (1 << 14), 1 << 14);
    }

    #[test]
    fn start_and_stop_sequences_end_in_their_loops() {
        let base = 4;
        let from_idle = start_words(base, 7, true);
        assert_eq!(from_idle.len(), 3);
        assert_eq!(
            ((from_idle[2] >> 15) & 0xffff) & 0x1f,
            u32::from(base + LOW_PULL)
        );
        let repeated = start_words(base, 7, false);
        assert_eq!(repeated.len(), 6);
        assert_eq!(
            ((repeated[5] >> 15) & 0xffff) & 0x1f,
            u32::from(base + LOW_PULL)
        );
        let stop = stop_words(base, 7);
        assert_eq!(stop.len(), 5);
    }
}
