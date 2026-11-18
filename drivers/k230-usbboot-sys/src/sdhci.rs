//! The SD host: the K230 SDHCI controller's registers under the stage's SD card protocol
//! ([`lemnos_k230_usbboot_stage::sd::SdHost`]), in programmed I/O.
//!
//! Register map: the SDHCI standard offsets, as the SDK's `drv_sdhci.h` names them (`SDHCI_ARGUMENT`
//! 0x08, `SDHCI_COMMAND` 0x0e, `SDHCI_RESPONSE` 0x10, `SDHCI_BUFFER` 0x20, `SDHCI_PRESENT_STATE` 0x24,
//! `SDHCI_POWER_CONTROL` 0x29, `SDHCI_CLOCK_CONTROL` 0x2c, `SDHCI_SOFTWARE_RESET` 0x2f,
//! `SDHCI_INT_STATUS` 0x30). The controller's base is a parameter (`SdhciHost::new`): the boot
//! controller is not known to the stage without the boot mode and the host's parameter.
//!
//! The clock (bench, 2026-11-18, `testing/board-runs/K230-USB.md` R3c and R3d). The first version enabled
//! the internal clock and the card clock with no divider and nothing else (`CLOCK_CONTROL` 0x0005), and on
//! the board no command ever completed: CMD0, which expects no reply, ran out its wait on both controllers
//! with the command inhibit still set. The boot ROM leaves the slot's controller at `CLOCK_CONTROL` 0x022f:
//! internal clock on and stable, card clock on, PLL enable (bit 3), the programmable-clock bit (bit 5), and
//! divider field 2. The SDK's RT-Smart driver (`drv_sdhci.c:714-743` at 7e302f7) sets the divider field
//! to half the division, with bit 5 and the card clock, from a 100 MHz base for this controller
//! (`drv_sdhci.c:894`): field 2 is 25 MHz, and 125 is 400 kHz, the identification clock. This host sets
//! what the ROM's state had, with the 400 kHz divider, in the standard's order: divider and internal
//! clock, wait stable; PLL enable, wait stable; card clock. Which of the missing bits stopped the command
//! is not separated. The timeout register takes the ROM's value (0x0e).
//!
//! The command-conflict check (bench, R3d). With the clock running, CMD0 ended at once with the command
//! timeout and command CRC errors together, which is how the standard reports a conflict on the command
//! line, on both controllers. The SDK's driver writes 0 to the block's vendor control byte
//! (`MSHC_CTRL_R`, 0x508) after every full reset of a controller without a PHY, which the slot's is
//! (`drv_sdhci.c:271-284`, `:889`), and this host does the same. That the byte's bit 0 is the conflict
//! check is the DesignWare block's own map, not in the SDK clone; the bench decides it. The other
//! controller has a PHY that the SDK sets up after a reset (`drv_sdhci.c:209-240`); this host does not, so
//! only the slot's controller is expected to work.
//!
//! UNVERIFIED on the board: the long-response (R2) layout and the data timing, taken from the standard.
//!
//! Every wait is bounded by [`SPIN`] reads, so a controller that never answers fails the call.

use lemnos_k230_usbboot_stage::sd::{BLOCK, Expect, Reply, SdError, SdHost};

use crate::{read32, write32};

/// The controller's register window.
pub const WINDOW: usize = 0x1000;
/// Reads a wait makes before it gives up.
pub const SPIN: u32 = 1_000_000;

const ARGUMENT: usize = 0x08;
const BLOCK_SIZE: usize = 0x04;
const TRANSFER_MODE: usize = 0x0c;
const RESPONSE: usize = 0x10;
const BUFFER: usize = 0x20;
const PRESENT_STATE: usize = 0x24;
const POWER_CONTROL: usize = 0x29;
const CLOCK_CONTROL: usize = 0x2c;
/// The block's vendor control byte (`drv_sdhci.c:40-41`: the vendor area at 0x500, this register at +8).
const MSHC_CTRL: usize = 0x508;
const TIMEOUT_CONTROL: usize = 0x2e;
const SOFTWARE_RESET: usize = 0x2f;
const INT_STATUS: usize = 0x30;
const INT_ENABLE: usize = 0x34;

/// Interrupt status bits: command complete, transfer complete, buffer write ready, buffer read
/// ready, and the error summary.
const INT_CMD_DONE: u32 = 1 << 0;
const INT_XFER_DONE: u32 = 1 << 1;
const INT_WRITE_READY: u32 = 1 << 4;
const INT_READ_READY: u32 = 1 << 5;
const INT_ERROR: u32 = 1 << 15;
/// Present state: command inhibit (bit 0) and data inhibit (bit 1).
const CMD_INHIBIT: u32 = 1 << 0;
const DAT_INHIBIT: u32 = 1 << 1;

/// Clock control bits: internal clock enable, internal clock stable, card clock enable, PLL enable, and
/// the programmable-clock bit (`drv_sdhci.h:111-115`).
const CLOCK_INT_EN: u16 = 0x0001;
const CLOCK_INT_STABLE: u16 = 0x0002;
const CLOCK_CARD_EN: u16 = 0x0004;
const CLOCK_PLL_EN: u16 = 0x0008;
const CLOCK_PROG: u16 = 0x0020;
/// The identification clock's ceiling, in kHz.
const IDENT_KHZ: u32 = 400;
/// The capabilities register: its bits 15:8 are the base clock in MHz (`drv_sdhci.h:193`). Read on the
/// bench: 100 on the slot's controller, 200 on the other, as `drv_sdhci.c:844` and `:894` set them.
const CAPABILITIES: usize = 0x40;
/// The status enables: every normal status, and the ten error statuses of the standard (command and
/// data timeout, CRC, end bit, index, current limit, auto command, ADMA), as the SDK's driver enables
/// its command and data masks (`drv_sdhci.c:568`). The first version enabled no error status, so a
/// command with no reply could only run out its wait.
const STATUS_ENABLES: u32 = 0x03ff_ffff;
/// Status reads between the card clock's start and the first command: the card needs 74 clocks (185
/// microseconds at 400 kHz); a read took about 0.3 microseconds on the bench, so this is a few
/// milliseconds.
const SETTLE: u32 = 10_000;

/// The controller's registers by offset, as 32-bit words.
pub trait SdhciRegs {
    /// Reads the word at `offset`.
    fn read(&self, offset: usize) -> u32;
    /// Writes the word at `offset`.
    fn write(&self, offset: usize, value: u32);
}

/// The registers of the controller at a base address.
pub struct Window {
    base: usize,
}

impl SdhciRegs for Window {
    fn read(&self, offset: usize) -> u32 {
        let addr = self.base + offset;
        if !in_window(self.base, addr) {
            return 0;
        }
        // SAFETY: `in_window` holds, so `addr` is an aligned register of this controller's window,
        // and reading an SDHCI register has no effect beyond its documented one (status reads).
        unsafe { read32(addr) }
    }

    fn write(&self, offset: usize, value: u32) {
        let addr = self.base + offset;
        if !in_window(self.base, addr) {
            return;
        }
        // SAFETY: `in_window` holds, so `addr` is an aligned register of this controller's window;
        // the callers write only the documented SDHCI values.
        unsafe { write32(addr, value) }
    }
}

/// The SDHCI host over a controller's registers.
pub struct SdhciHost<R: SdhciRegs = Window> {
    regs: R,
}

impl SdhciHost<Window> {
    /// The host over the controller at `base`, started as [`SdhciHost::start`] does.
    pub fn new(base: usize) -> Result<Self, SdError> {
        Self::start(Window { base })
    }
}

impl<R: SdhciRegs> SdhciHost<R> {
    /// Resets the controller, powers the bus at 3.3 V, and starts the card clock at 400 kHz (see the
    /// module's note on the clock). Fails when the controller does not answer.
    pub fn start(regs: R) -> Result<Self, SdError> {
        let host = Self { regs };
        host.set8(SOFTWARE_RESET, 1)?;
        host.wait_clear8(SOFTWARE_RESET, 1)?;
        host.set8(MSHC_CTRL, 0)?;
        host.set8(POWER_CONTROL, 0x0f)?;
        host.set8(TIMEOUT_CONTROL, 0x0e)?;
        let clock = ident_divider(host.read(CAPABILITIES))? | CLOCK_PROG | CLOCK_INT_EN;
        host.set16(CLOCK_CONTROL, clock)?;
        host.wait_set16(CLOCK_CONTROL, CLOCK_INT_STABLE)?;
        host.set16(CLOCK_CONTROL, clock | CLOCK_PLL_EN)?;
        host.wait_set16(CLOCK_CONTROL, CLOCK_INT_STABLE)?;
        host.set16(CLOCK_CONTROL, clock | CLOCK_PLL_EN | CLOCK_CARD_EN)?;
        host.write(INT_ENABLE, STATUS_ENABLES);
        for _ in 0..SETTLE {
            let _ = host.read(PRESENT_STATE);
        }
        Ok(host)
    }

    fn read(&self, offset: usize) -> u32 {
        self.regs.read(offset)
    }

    /// The 136-bit reply as its 128 bits, most significant first. The controller keeps bits 127 to 8 in
    /// its response registers' bits 119 to 0 (the CRC byte is not stored), so the registers' value is
    /// moved up one byte, as the SDK's driver does (`drv_sdhci.c:320-328` at 7e302f7). The first version
    /// did not move it, which reads every CSD and CID field one byte off (an SDHC card's CSD structure
    /// would read 0, "not SDHC"); no card had answered yet, so the bench never saw it.
    fn long_reply(&self) -> [u8; 16] {
        let mut raw = [0u8; 16];
        for i in 0..4 {
            let word = self.read(RESPONSE + 4 * (3 - i));
            raw[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
        }
        let mut out = [0u8; 16];
        out[..15].copy_from_slice(&raw[1..]);
        out
    }

    fn write(&self, offset: usize, value: u32) {
        self.regs.write(offset, value);
    }

    /// A byte register: a read-modify-write of its aligned word.
    fn set8(&self, offset: usize, bits: u8) -> Result<(), SdError> {
        let word = offset & !3;
        let shift = (offset & 3) * 8;
        let old = self.read(word);
        self.write(word, (old & !(0xff << shift)) | (u32::from(bits) << shift));
        Ok(())
    }

    fn set16(&self, offset: usize, bits: u16) -> Result<(), SdError> {
        let word = offset & !3;
        let shift = (offset & 2) * 8;
        let old = self.read(word);
        self.write(
            word,
            (old & !(0xffff << shift)) | (u32::from(bits) << shift),
        );
        Ok(())
    }

    fn wait_clear8(&self, offset: usize, bits: u8) -> Result<(), SdError> {
        let word = offset & !3;
        let shift = (offset & 3) * 8;
        for _ in 0..SPIN {
            if (self.read(word) >> shift) as u8 & bits == 0 {
                return Ok(());
            }
        }
        Err(SdError::Timeout(0))
    }

    fn wait_set16(&self, offset: usize, bits: u16) -> Result<(), SdError> {
        let word = offset & !3;
        let shift = (offset & 2) * 8;
        for _ in 0..SPIN {
            if (self.read(word) >> shift) as u16 & bits == bits {
                return Ok(());
            }
        }
        Err(SdError::Timeout(0))
    }

    /// Waits until the interrupt status has `bit`, or reports an error.
    fn wait_int(&self, bit: u32, cmd: u8) -> Result<(), SdError> {
        for _ in 0..SPIN {
            let status = self.read(INT_STATUS);
            if status & INT_ERROR != 0 {
                // The error bits are left in the status for the report to print; the next command
                // clears the whole status before it starts.
                return Err(SdError::Reply(cmd, "the controller reported an error"));
            }
            if status & bit != 0 {
                self.write(INT_STATUS, bit);
                return Ok(());
            }
        }
        Err(SdError::Timeout(cmd))
    }

    fn wait_inhibit_clear(&self, mask: u32, cmd: u8) -> Result<(), SdError> {
        for _ in 0..SPIN {
            if self.read(PRESENT_STATE) & mask == 0 {
                return Ok(());
            }
        }
        Err(SdError::Timeout(cmd))
    }
}

/// The clock control's divider field for the identification clock, from the capabilities' base clock:
/// the card clock is the base over twice the division (`drv_sdhci.c:726-737`), so the division is the
/// smallest that gives 400 kHz or less. Its low eight bits sit at bit 8 and its two upper bits at bit 6.
fn ident_divider(capabilities: u32) -> Result<u16, SdError> {
    let base_khz = ((capabilities >> 8) & 0xff) * 1000;
    if base_khz == 0 {
        return Err(SdError::Refused(
            "the controller's capabilities give no base clock",
        ));
    }
    let division = base_khz.div_ceil(2 * IDENT_KHZ);
    if division > 0x3ff {
        return Err(SdError::Refused(
            "the base clock is too fast for the divider",
        ));
    }
    Ok((((division & 0xff) << 8) | (((division >> 8) & 0x3) << 6)) as u16)
}

/// The registers the report prints around its card read, by SDHCI standard offset and name: the present
/// state (bit 16 card inserted, bit 17 card state stable, bit 18 the card-detect pin's level), the host
/// control and power word, the clock, timeout and reset word, the interrupt status, the capabilities, and
/// the vendor control word.
/// Every one is a status or setting register whose read has no effect; the data buffer (0x20), whose read
/// consumes data, is not among them.
pub const SNAPSHOT: [(usize, &str); 6] = [
    (PRESENT_STATE, "PRESENT_STATE"),
    (0x28, "HOST_CONTROL_POWER"),
    (CLOCK_CONTROL, "CLOCK_TIMEOUT_RESET"),
    (INT_STATUS, "INT_STATUS"),
    (CAPABILITIES, "CAPABILITIES"),
    (MSHC_CTRL, "MSHC_CTRL"),
];

/// Reads the [`SNAPSHOT`] registers of the controller at `base`. Nothing is written.
pub fn snapshot(base: usize) -> [u32; 6] {
    let mut out = [0u32; 6];
    for (slot, (offset, _)) in out.iter_mut().zip(SNAPSHOT) {
        let addr = base + offset;
        if in_window(base, addr) {
            // SAFETY: `in_window` holds, so `addr` is an aligned register of this controller's window, and
            // it is one of the `SNAPSHOT` registers, whose read has no effect.
            *slot = unsafe { read32(addr) };
        }
    }
    out
}

/// True when `addr` is an aligned register in the controller's window.
fn in_window(base: usize, addr: usize) -> bool {
    addr.is_multiple_of(4) && addr >= base && addr < base + WINDOW
}

impl<R: SdhciRegs> SdHost for SdhciHost<R> {
    fn command(&mut self, index: u8, arg: u32, expect: Expect) -> Result<Reply, SdError> {
        self.wait_inhibit_clear(CMD_INHIBIT, index)?;
        // The command register is the high halfword of the word at 0x0c, and the transfer mode is the
        // low halfword: both go in one write, so the data setup is in place when the command starts.
        // Response type (bits 1:0), CRC check (bit 3), index check (bit 4), data present (bit 5).
        // CMD41 (R3) has neither a CRC nor an index.
        let (data, transfer): (bool, u32) = match index {
            17 => (true, 0x12), // single block, block count, read
            24 => (true, 0x02), // single block, block count, write
            _ => (false, 0x00),
        };
        let response: u32 = match expect {
            Expect::None => 0x00,
            Expect::Long => 0x01 | 0x08,
            Expect::Short if index == 41 => 0x02,
            Expect::Short => 0x02 | 0x08 | 0x10,
        } | if data { 0x20 } else { 0x00 };
        self.write(INT_STATUS, 0xffff_ffff);
        self.write(ARGUMENT, arg);
        if data {
            // Block size (low halfword of 0x04) and block count (high halfword of 0x04): one block.
            self.write(BLOCK_SIZE, (1 << 16) | BLOCK as u32);
        }
        let word = (u32::from(index) << 24) | (response << 16) | transfer;
        self.write(TRANSFER_MODE, word);
        self.wait_int(INT_CMD_DONE, index)?;
        Ok(match expect {
            Expect::None => Reply::None,
            Expect::Short => Reply::Short(self.read(RESPONSE)),
            Expect::Long => Reply::Long(self.long_reply()),
        })
    }

    fn read_block(&mut self, out: &mut [u8; BLOCK]) -> Result<(), SdError> {
        self.wait_int(INT_READ_READY, 17)?;
        for word in 0..BLOCK / 4 {
            let value = self.read(BUFFER);
            out[4 * word..4 * word + 4].copy_from_slice(&value.to_le_bytes());
        }
        self.wait_int(INT_XFER_DONE, 17)?;
        self.wait_inhibit_clear(DAT_INHIBIT, 17)
    }

    fn write_block(&mut self, data: &[u8; BLOCK]) -> Result<(), SdError> {
        self.wait_int(INT_WRITE_READY, 24)?;
        for word in 0..BLOCK / 4 {
            let mut bytes = [0u8; 4];
            bytes.copy_from_slice(&data[4 * word..4 * word + 4]);
            self.write(BUFFER, u32::from_le_bytes(bytes));
        }
        self.wait_int(INT_XFER_DONE, 24)?;
        self.wait_inhibit_clear(DAT_INHIBIT, 24)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use core::cell::Cell;

    /// The stand-in for the controller as the bench found it on 2026-11-18. The internal clock reports
    /// stable when it is enabled. A command completes only when the clock word has what the ROM's working
    /// state had: internal clock, card clock, PLL enable and the programmable-clock bit, with a divider.
    /// Without them the command inhibit stays set and no interrupt comes, which is what the first host
    /// got on both controllers. The stand-in holds no card: a command that expects a reply ends in the
    /// controller's timeout error.
    struct Model {
        words: [Cell<u32>; 0x150],
    }

    impl Model {
        fn new() -> Self {
            let model = Self {
                words: [const { Cell::new(0) }; 0x150],
            };
            // The slot's controller, as read on the bench: a 100 MHz base.
            model.words[CAPABILITIES / 4].set(0x276a_6481);
            model
        }

        fn clock(&self) -> u16 {
            self.words[CLOCK_CONTROL / 4].get() as u16
        }

        fn card_clock_runs(&self) -> bool {
            let need = CLOCK_INT_EN | CLOCK_CARD_EN | CLOCK_PLL_EN | CLOCK_PROG;
            self.clock() & need == need && self.clock() >> 8 != 0
        }
    }

    impl SdhciRegs for Model {
        fn read(&self, offset: usize) -> u32 {
            let word = self.words[offset / 4].get();
            match offset {
                CLOCK_CONTROL if word & u32::from(CLOCK_INT_EN) != 0 => {
                    word | u32::from(CLOCK_INT_STABLE)
                }
                _ => word,
            }
        }

        fn write(&self, offset: usize, value: u32) {
            match offset {
                // The reset bits clear themselves; the rest of the word is kept. A full reset puts the
                // conflict check back on, the block's state after reset.
                CLOCK_CONTROL => {
                    if value & (1 << 24) != 0 {
                        self.words[MSHC_CTRL / 4].set(1);
                    }
                    self.words[offset / 4].set(value & 0x00ff_ffff);
                }
                // Write one to clear.
                INT_STATUS => {
                    let word = &self.words[offset / 4];
                    word.set(word.get() & !value);
                }
                TRANSFER_MODE => {
                    if !self.card_clock_runs() {
                        self.words[PRESENT_STATE / 4].set(CMD_INHIBIT);
                        return;
                    }
                    let expects_reply = (value >> 16) & 0x03 != 0;
                    // A status appears only when its enable is set, and the error summary only with
                    // an enabled error.
                    let enabled = self.words[INT_ENABLE / 4].get();
                    // With the conflict check on, every command ends as a conflict (R3d on the bench):
                    // the command timeout and CRC errors together.
                    let conflict = self.words[MSHC_CTRL / 4].get() & 1 != 0;
                    let raw = if conflict {
                        0x3 << 16
                    } else if expects_reply {
                        1 << 16
                    } else {
                        INT_CMD_DONE
                    };
                    let mut status = raw & enabled;
                    if status >> 16 != 0 {
                        status |= INT_ERROR;
                    }
                    if status == 0 {
                        self.words[PRESENT_STATE / 4].set(CMD_INHIBIT);
                    }
                    self.words[INT_STATUS / 4].set(status);
                }
                _ => self.words[offset / 4].set(value),
            }
        }
    }

    #[test]
    fn a_command_that_expects_no_reply_completes_once_the_card_clock_runs() {
        let mut host = SdhciHost::start(Model::new()).unwrap();
        assert_eq!(
            host.regs.clock() & 0xff2d,
            0x7d2d,
            "{:#06x}",
            host.regs.clock()
        );
        assert_eq!(
            (host.regs.words[TIMEOUT_CONTROL / 4].get() >> 16) & 0xff,
            0x0e
        );
        assert_eq!(host.command(0, 0, Expect::None), Ok(Reply::None));
    }

    #[test]
    fn an_empty_slot_is_the_controllers_timeout_error_at_cmd8_and_the_error_bits_stay_readable() {
        let mut host = SdhciHost::start(Model::new()).unwrap();
        host.command(0, 0, Expect::None).unwrap();
        assert_eq!(
            host.command(8, 0x1aa, Expect::Short),
            Err(SdError::Reply(8, "the controller reported an error"))
        );
        assert_eq!(host.read(INT_STATUS), INT_ERROR | (1 << 16));
    }

    #[test]
    fn with_the_conflict_check_left_on_every_command_ends_as_a_conflict() {
        // The R3d host: everything started, the vendor control byte untouched after the reset.
        let mut host = SdhciHost::start(Model::new()).unwrap();
        host.regs.words[MSHC_CTRL / 4].set(1);
        assert_eq!(
            host.command(0, 0, Expect::None),
            Err(SdError::Reply(0, "the controller reported an error"))
        );
        assert_eq!(host.read(INT_STATUS), INT_ERROR | (0x3 << 16));
    }

    #[test]
    fn a_clock_word_without_the_roms_bits_never_completes_a_command() {
        // The first host's word, 0x0005: the bench result of 2026-11-18, kept as the stand-in's failing case.
        let host = SdhciHost { regs: Model::new() };
        host.set16(CLOCK_CONTROL, 0x0005).unwrap();
        let mut host = host;
        assert_eq!(host.command(0, 0, Expect::None), Err(SdError::Timeout(0)));
        assert_eq!(host.read(PRESENT_STATE) & CMD_INHIBIT, CMD_INHIBIT);
    }

    #[test]
    fn the_identification_divider_follows_the_base_clock_the_controller_reports() {
        // 100 MHz over 2 x 125 and 200 MHz over 2 x 250 are both 400 kHz.
        assert_eq!(ident_divider(0x276a_6481), Ok(125 << 8));
        assert_eq!(ident_divider(0x256e_c881), Ok(250 << 8));
        // A division past eight bits puts its upper bits at bit 6: 255 MHz needs 319 (0x13f).
        assert_eq!(ident_divider(0x0000_ff00), Ok((0x3f << 8) | (1 << 6)));
        assert!(matches!(ident_divider(0), Err(SdError::Refused(_))));
    }

    #[test]
    fn a_long_reply_is_moved_up_the_byte_the_controller_leaves_out() {
        use lemnos_k230_usbboot_stage::sd::field;
        // An SDHC card's CSD: structure 1 in bits 127:126, C_SIZE 0x1d2bf in bits 69:48 (a 64 GB card's
        // order of size). The controller holds bits 127:8 in its registers' bits 119:0.
        let csd: u128 = (1u128 << 126) | (0x1_d2bfu128 << 48);
        let held = csd >> 8;
        let model = Model::new();
        for i in 0..4 {
            model.words[RESPONSE / 4 + i].set((held >> (32 * i)) as u32);
        }
        let host = SdhciHost { regs: model };
        let reply = host.long_reply();
        assert_eq!(field(&reply, 127, 126), 1);
        assert_eq!(field(&reply, 69, 48), 0x1_d2bf);
    }

    #[test]
    fn the_snapshot_reads_only_aligned_registers_of_the_window_and_never_the_data_buffer() {
        for (offset, name) in SNAPSHOT {
            assert!(in_window(0x9158_1000, 0x9158_1000 + offset), "{name}");
            assert_ne!(offset, BUFFER, "{name}: a read of the buffer consumes data");
        }
    }
}
