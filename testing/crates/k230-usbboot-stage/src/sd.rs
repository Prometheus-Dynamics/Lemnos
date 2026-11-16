//! The SD card medium: the SD protocol's initialisation and single-block transfers over an
//! [`SdHost`], and the [`Medium`] the stage writes. The host is the controller (the `-sys` crate
//! drives the K230's SDHCI registers); the protocol and the card's answers are here, so the
//! simulator and the tests run them with a fake card.
//!
//! Provenance (read, nothing copied):
//! - The controller: the device tree gives `sdhci0` at `0x91580000` (`mmc_sd0`) and `sdhci1` at
//!   `0x91581000` (`mmc_sd1`, compatible `kendryte,k230-dw-mshc`), `k230.dtsi` lines 154-176 at
//!   kendryte/k230_sdk 7e302f7. The SDK's board header gives the SD block at `0x91580000`
//!   (`SD_HC_BASE_ADDR`, `board.h:243`). Which controller the board boots from is UNVERIFIED (the
//!   boot mode is read from `soc_boot_ctl`, `sysctl_boot.c:9-23`).
//! - The register map of the SDHCI controller: `drv_sdhci.h` in the SDK's `interdrv/sdio`
//!   (`SDHCI_ARGUMENT` 0x08, `SDHCI_COMMAND` 0x0e, `SDHCI_RESPONSE` 0x10, `SDHCI_BUFFER` 0x20,
//!   `SDHCI_PRESENT_STATE` 0x24, `SDHCI_CLOCK_CONTROL` 0x2c, `SDHCI_SOFTWARE_RESET` 0x2f,
//!   `SDHCI_INT_STATUS` 0x30).
//! - The card protocol is the SD physical layer: CMD0, CMD8, ACMD41 (with CMD55), CMD2, CMD3, CMD9,
//!   CMD7, CMD13, CMD16, CMD17, CMD24. Only SDHC/SDXC cards (block addressing) are accepted.
//!
//! UNVERIFIED on hardware: the controller's timing and its clock, the card's answers on the board,
//! and the write's busy time. The protocol's own checks (the ACMD41 busy bit, the CMD8 echo, the
//! CSD version) are tested with the fake card.

use crate::medium::{Geometry, Medium, MediumError};

/// One SD block.
pub const BLOCK: usize = 512;
/// The ACMD41 tries before the card is called not ready (each try is a CMD55 and an ACMD41).
pub const READY_TRIES: u32 = 1000;
/// The READY_FOR_DATA polls after a write before the card is called busy (CMD13).
pub const BUSY_POLLS: u32 = 1000;
/// The argument for CMD8: 2.7-3.6 V and the check pattern 0xAA.
const CMD8_ARG: u32 = 0x0000_01aa;
/// The host capacity bit (HCS) and the voltage window, for ACMD41.
const ACMD41_ARG: u32 = 0x4000_0000 | 0x00ff_8000;
/// Card status bit 8: READY_FOR_DATA.
const STATUS_READY: u32 = 1 << 8;

/// The two SDHCI controllers the device tree gives (`k230.dtsi:154-176` at 7e302f7: `sdhci0` at
/// `0x91580000`, `sdhci1` at `0x91581000`). Which one the board boots from is not in any source the
/// stage can read, so the stage never picks one: it takes it from the boot mode and a parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdController {
    Sdhci0,
    Sdhci1,
}

impl SdController {
    /// The controller's register base.
    pub fn base(self) -> usize {
        match self {
            Self::Sdhci0 => 0x9158_0000,
            Self::Sdhci1 => 0x9158_1000,
        }
    }

    /// The parameter value the host passes: 0 for `sdhci0`, 1 for `sdhci1`.
    pub fn parameter(self) -> u32 {
        match self {
            Self::Sdhci0 => 0,
            Self::Sdhci1 => 1,
        }
    }
}

/// The boot mode the boot system controller reports in `soc_boot_ctl` bits 1:0
/// (`sysctl_boot.c:9-23`, `sysctl_boot_get_boot_mode`: 0 NOR, 1 NAND, 2 eMMC, 3 SD card).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootMode {
    Nor,
    Nand,
    Emmc,
    SdCard,
}

/// The boot mode from `soc_boot_ctl` (`0x91102040`).
pub fn boot_mode(soc_boot_ctl: u32) -> BootMode {
    match soc_boot_ctl & 0x3 {
        1 => BootMode::Nand,
        2 => BootMode::Emmc,
        3 => BootMode::SdCard,
        _ => BootMode::Nor,
    }
}

/// Decodes the parameter the host passes (0 or 1). Any other value is refused by name.
pub fn controller_from_parameter(value: u32) -> Result<SdController, SdError> {
    match value {
        0 => Ok(SdController::Sdhci0),
        1 => Ok(SdController::Sdhci1),
        _ => Err(SdError::Refused(
            "unknown SD controller parameter: pass 0 (sdhci0) or 1 (sdhci1)",
        )),
    }
}

/// The SD controller to boot from. A parameter the host passed wins; with none, the boot mode must
/// name no controller the stage can know. An SD or eMMC boot with no parameter is refused by name,
/// and so is a NOR or NAND boot (no SD controller is the boot device then).
pub fn resolve_controller(
    mode: BootMode,
    parameter: Option<SdController>,
) -> Result<SdController, SdError> {
    if let Some(controller) = parameter {
        return Ok(controller);
    }
    match mode {
        BootMode::SdCard => Err(SdError::Refused(
            "the boot mode is SD card, but the SD controller it boots from is not known: pass it as the parameter",
        )),
        BootMode::Emmc => Err(SdError::Refused(
            "the boot mode is eMMC, which has no SD controller the stage knows: pass the controller as the parameter",
        )),
        BootMode::Nor | BootMode::Nand => Err(SdError::Refused(
            "the boot mode is NOR or NAND: no SD card is the boot device",
        )),
    }
}

/// A failure of the card or the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SdError {
    /// The command got no reply, or the host reported a timeout.
    Timeout(u8),
    /// The card answered, but not what the protocol says for this command.
    Reply(u8, &'static str),
    /// The card is not an SDHC/SDXC card (block addressing).
    NotSdhc,
    /// The host failed on the data path.
    Data(&'static str),
    /// The stage refuses to start the medium, with the reason named.
    Refused(&'static str),
}

impl core::fmt::Display for SdError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Timeout(cmd) => write!(f, "CMD{cmd} got no reply"),
            Self::Reply(cmd, what) => write!(f, "CMD{cmd}: {what}"),
            Self::NotSdhc => write!(f, "the card is not SDHC or SDXC"),
            Self::Data(what) => write!(f, "data: {what}"),
            Self::Refused(why) => write!(f, "refused: {why}"),
        }
    }
}

/// The reply shape a command expects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    /// No reply (CMD0).
    None,
    /// A 32-bit reply (R1, R3, R6, R7).
    Short,
    /// A 128-bit reply (R2: CID or CSD), as 16 bytes, most significant first, CRC removed.
    Long,
}

/// A reply, by its shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    None,
    Short(u32),
    Long([u8; 16]),
}

/// The controller under the card protocol.
pub trait SdHost {
    /// Sends `index` with `arg` and returns the reply of the shape `expect` asks for.
    fn command(&mut self, index: u8, arg: u32, expect: Expect) -> Result<Reply, SdError>;
    /// Reads one block after a CMD17, into `out`.
    fn read_block(&mut self, out: &mut [u8; BLOCK]) -> Result<(), SdError>;
    /// Writes one block after a CMD24.
    fn write_block(&mut self, data: &[u8; BLOCK]) -> Result<(), SdError>;
}

/// A bit range of a 128-bit R2 value: bit `n` is `bytes[15 - n / 8] >> (n % 8)`.
pub fn field(bytes: &[u8; 16], hi: u32, lo: u32) -> u64 {
    let mut value = 0u64;
    for bit in (lo..=hi).rev() {
        let set = (bytes[15 - (bit / 8) as usize] >> (bit % 8)) & 1;
        value = (value << 1) | u64::from(set);
    }
    value
}

/// An SDHC/SDXC card, initialised and selected, with its capacity from the CSD.
pub struct Card<H: SdHost> {
    host: H,
    rca: u32,
    blocks: u64,
    cid: [u8; 16],
}

/// The card check the stage reports before any write: the CID, the capacity, and a CRC of one block
/// read at the start and one at the middle. A read failure is a card fault candidate (a card that is
/// hot, or worn), reported as such, not only a flasher fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardCheck {
    pub cid: [u8; 16],
    pub blocks: u64,
    pub crc_first: u32,
    pub crc_middle: u32,
}

impl<H: SdHost> Card<H> {
    /// Runs the initialisation sequence on `host` and returns the card, selected and ready.
    pub fn init(mut host: H) -> Result<Self, SdError> {
        command(&mut host, 0, 0, Expect::None)?;
        let echo = short(command(&mut host, 8, CMD8_ARG, Expect::Short)?, 8)?;
        if echo & 0xfff != CMD8_ARG & 0xfff {
            return Err(SdError::Reply(8, "the check pattern is not echoed"));
        }
        let mut ready = false;
        for _ in 0..READY_TRIES {
            command(&mut host, 55, 0, Expect::Short)?;
            let ocr = short(command(&mut host, 41, ACMD41_ARG, Expect::Short)?, 41)?;
            if ocr & (1 << 31) != 0 {
                if ocr & (1 << 30) == 0 {
                    return Err(SdError::NotSdhc);
                }
                ready = true;
                break;
            }
        }
        if !ready {
            return Err(SdError::Reply(41, "the card never became ready"));
        }
        command(&mut host, 2, 0, Expect::Long)?;
        let rca = short(command(&mut host, 3, 0, Expect::Short)?, 3)? >> 16;
        let csd = match command(&mut host, 9, rca << 16, Expect::Long)? {
            Reply::Long(csd) => csd,
            _ => return Err(SdError::Reply(9, "no CSD")),
        };
        if field(&csd, 127, 126) != 1 {
            return Err(SdError::NotSdhc);
        }
        let c_size = field(&csd, 69, 48);
        let blocks = (c_size + 1) * 1024;
        let cid = match command(&mut host, 10, rca << 16, Expect::Long)? {
            Reply::Long(cid) => cid,
            _ => [0; 16],
        };
        short(command(&mut host, 7, rca << 16, Expect::Short)?, 7)?;
        short(command(&mut host, 16, BLOCK as u32, Expect::Short)?, 16)?;
        Ok(Self {
            host,
            rca,
            blocks,
            cid,
        })
    }

    /// The card check: the CID, the capacity, and a CRC of a block at the start and one in the middle.
    pub fn check(&mut self) -> Result<CardCheck, SdError> {
        let mut first = [0u8; BLOCK];
        let mut middle = [0u8; BLOCK];
        self.read(0, &mut first)?;
        self.read(self.blocks / 2, &mut middle)?;
        Ok(CardCheck {
            cid: self.cid,
            blocks: self.blocks,
            crc_first: crate::format::crc32(&first),
            crc_middle: crate::format::crc32(&middle),
        })
    }

    /// The card's relative address.
    pub fn rca(&self) -> u32 {
        self.rca
    }

    /// The card's capacity in 512-byte blocks.
    pub fn blocks(&self) -> u64 {
        self.blocks
    }

    /// Reads block `lba`.
    pub fn read(&mut self, lba: u64, out: &mut [u8; BLOCK]) -> Result<(), SdError> {
        check_lba(lba, self.blocks, 17)?;
        short(command(&mut self.host, 17, lba as u32, Expect::Short)?, 17)?;
        self.host.read_block(out)
    }

    /// Writes block `lba`, then waits until the card is ready again.
    pub fn write(&mut self, lba: u64, data: &[u8; BLOCK]) -> Result<(), SdError> {
        check_lba(lba, self.blocks, 24)?;
        short(command(&mut self.host, 24, lba as u32, Expect::Short)?, 24)?;
        self.host.write_block(data)?;
        for _ in 0..BUSY_POLLS {
            let status = short(
                command(&mut self.host, 13, self.rca << 16, Expect::Short)?,
                13,
            )?;
            if status & STATUS_READY != 0 {
                return Ok(());
            }
        }
        Err(SdError::Reply(13, "the card stayed busy after the write"))
    }
}

fn check_lba(lba: u64, blocks: u64, cmd: u8) -> Result<(), SdError> {
    if lba >= blocks || lba > u32::MAX as u64 {
        Err(SdError::Reply(cmd, "the block is past the card"))
    } else {
        Ok(())
    }
}

fn command<H: SdHost>(host: &mut H, index: u8, arg: u32, expect: Expect) -> Result<Reply, SdError> {
    host.command(index, arg, expect)
}

fn short(reply: Reply, cmd: u8) -> Result<u32, SdError> {
    match reply {
        Reply::Short(word) => Ok(word),
        _ => Err(SdError::Reply(cmd, "no short reply")),
    }
}

/// The card as the stage's medium: 512-byte blocks, erased by writing 0xff. Writes must cover whole
/// blocks; reads may start and end anywhere.
pub struct SdMedium<H: SdHost> {
    card: Card<H>,
}

/// The erase unit the stage reports: the boot header block, so the header is one erase.
pub const ERASE: u64 = 4096;

impl<H: SdHost> SdMedium<H> {
    /// A medium over an initialised card.
    pub fn new(card: Card<H>) -> Self {
        Self { card }
    }

    fn fail(error: SdError) -> MediumError {
        match error {
            SdError::Timeout(_) => MediumError("the card did not answer"),
            SdError::Reply(_, _) | SdError::NotSdhc => MediumError("the card refused the command"),
            SdError::Data(_) | SdError::Refused(_) => MediumError("the data transfer failed"),
        }
    }
}

impl<H: SdHost> Medium for SdMedium<H> {
    fn geometry(&self) -> Geometry {
        Geometry {
            capacity: self.card.blocks() * BLOCK as u64,
            block: BLOCK as u64,
            erase: ERASE,
        }
    }

    fn read(&mut self, offset: u64, out: &mut [u8]) -> Result<(), MediumError> {
        let mut block = [0u8; BLOCK];
        let mut done = 0usize;
        while done < out.len() {
            let at = offset + done as u64;
            let lba = at / BLOCK as u64;
            let within = (at % BLOCK as u64) as usize;
            self.card.read(lba, &mut block).map_err(Self::fail)?;
            let take = (BLOCK - within).min(out.len() - done);
            out[done..done + take].copy_from_slice(&block[within..within + take]);
            done += take;
        }
        Ok(())
    }

    fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), MediumError> {
        if !offset.is_multiple_of(BLOCK as u64) || !data.len().is_multiple_of(BLOCK) {
            return Err(MediumError("a write must cover whole blocks"));
        }
        let mut block = [0u8; BLOCK];
        for (i, chunk) in data.chunks(BLOCK).enumerate() {
            block.copy_from_slice(chunk);
            self.card
                .write(offset / BLOCK as u64 + i as u64, &block)
                .map_err(Self::fail)?;
        }
        Ok(())
    }

    fn erase(&mut self, offset: u64, len: u64) -> Result<(), MediumError> {
        if !offset.is_multiple_of(BLOCK as u64) || !len.is_multiple_of(BLOCK as u64) {
            return Err(MediumError("an erase must cover whole blocks"));
        }
        let ones = [0xffu8; BLOCK];
        for i in 0..len / BLOCK as u64 {
            self.card
                .write(offset / BLOCK as u64 + i, &ones)
                .map_err(Self::fail)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake SDHC card: its answers follow the protocol, and a knob makes each one fail.
    struct FakeCard {
        blocks: u64,
        /// ACMD41 answers busy this many times before it is ready.
        busy_for: u32,
        /// The CMD8 echo is wrong (a v1 card answers this way).
        v1: bool,
        /// The card stays busy after writes (CMD13 never reports ready).
        stuck_busy: bool,
        /// The data the card has.
        bytes: Vec<u8>,
        /// The block the last CMD17/CMD24 addressed.
        at: Option<(u8, u64)>,
        acmd: bool,
        acmd41_calls: u32,
    }

    impl FakeCard {
        fn new(blocks: u64) -> Self {
            Self {
                blocks,
                busy_for: 0,
                v1: false,
                stuck_busy: false,
                bytes: vec![0; (blocks as usize) * BLOCK],
                at: None,
                acmd: false,
                acmd41_calls: 0,
            }
        }

        fn csd(&self) -> [u8; 16] {
            let mut csd = [0u8; 16];
            // CSD_STRUCTURE = 1 (version 2): bits 127:126.
            csd[0] = 0x40;
            let c_size = self.blocks / 1024 - 1;
            for i in 0..22 {
                if (c_size >> i) & 1 == 1 {
                    let bit = 48 + i;
                    csd[15 - (bit / 8) as usize] |= 1 << (bit % 8);
                }
            }
            csd
        }
    }

    impl SdHost for FakeCard {
        fn command(&mut self, index: u8, arg: u32, _expect: Expect) -> Result<Reply, SdError> {
            let was_acmd = std::mem::replace(&mut self.acmd, false);
            match (index, was_acmd) {
                (0, _) => Ok(Reply::None),
                (8, _) => Ok(Reply::Short(if self.v1 { 0 } else { arg })),
                (55, _) => {
                    self.acmd = true;
                    Ok(Reply::Short(0x0120))
                }
                (41, true) => {
                    self.acmd41_calls += 1;
                    let ready = self.acmd41_calls > self.busy_for;
                    let ocr = if ready { 0xc0ff_8000 } else { 0x00ff_8000 };
                    Ok(Reply::Short(ocr))
                }
                (2, _) => Ok(Reply::Long([0x11; 16])),
                (10, _) => Ok(Reply::Long([0x5c; 16])),
                (3, _) => Ok(Reply::Short(0x1234_0000)),
                (9, _) => Ok(Reply::Long(self.csd())),
                (7, _) | (16, _) => Ok(Reply::Short(0x0000_0900)),
                (13, _) => Ok(Reply::Short(if self.stuck_busy { 0 } else { STATUS_READY })),
                (17, _) | (24, _) => {
                    if arg as u64 >= self.blocks {
                        return Err(SdError::Reply(index, "past the card"));
                    }
                    self.at = Some((index, u64::from(arg)));
                    Ok(Reply::Short(0x0000_0900))
                }
                _ => Err(SdError::Timeout(index)),
            }
        }

        fn read_block(&mut self, out: &mut [u8; BLOCK]) -> Result<(), SdError> {
            let (cmd, lba) = self.at.take().ok_or(SdError::Data("no read"))?;
            if cmd != 17 {
                return Err(SdError::Data("the read was not addressed"));
            }
            let at = lba as usize * BLOCK;
            out.copy_from_slice(&self.bytes[at..at + BLOCK]);
            Ok(())
        }

        fn write_block(&mut self, data: &[u8; BLOCK]) -> Result<(), SdError> {
            let (cmd, lba) = self.at.take().ok_or(SdError::Data("no write"))?;
            if cmd != 24 {
                return Err(SdError::Data("the write was not addressed"));
            }
            let at = lba as usize * BLOCK;
            self.bytes[at..at + BLOCK].copy_from_slice(data);
            Ok(())
        }
    }

    /// A medium over a card of 1024 blocks (512 KiB, the SDHC capacity unit).
    fn medium() -> SdMedium<FakeCard> {
        SdMedium::new(Card::init(FakeCard::new(1024)).expect("a healthy card initialises"))
    }

    #[test]
    fn the_card_initialises_and_reports_its_capacity_from_the_csd() {
        let card = Card::init(FakeCard::new(1024)).unwrap();
        assert_eq!(card.blocks(), 1024);
        assert_eq!(card.rca(), 0x1234);
        assert_eq!(SdMedium::new(card).geometry().capacity, 1024 * 512);
    }

    #[test]
    fn a_block_written_reads_back_and_a_partial_read_spans_blocks() {
        let mut m = medium();
        let data: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
        m.write(512, &data).unwrap();
        let mut back = vec![0u8; 1024];
        m.read(512, &mut back).unwrap();
        assert_eq!(back, data);
        // A read that starts mid-block and ends mid-block.
        let mut part = vec![0u8; 700];
        m.read(700, &mut part).unwrap();
        assert_eq!(part, data[188..888]);
    }

    #[test]
    fn an_erase_leaves_ones_and_the_medium_is_not_written_outside_it() {
        let mut m = medium();
        m.write(0, &[7u8; 1024]).unwrap();
        m.erase(0, 512).unwrap();
        let mut back = vec![0u8; 1024];
        m.read(0, &mut back).unwrap();
        assert!(back[..512].iter().all(|&b| b == 0xff));
        assert!(back[512..].iter().all(|&b| b == 7));
    }

    #[test]
    fn a_card_that_stays_busy_is_called_not_ready_after_bounded_tries() {
        let mut fake = FakeCard::new(1024);
        fake.busy_for = u32::MAX;
        let error = Card::init(fake).err().unwrap();
        assert!(matches!(error, SdError::Reply(41, _)), "{error}");
    }

    #[test]
    fn a_v1_card_that_does_not_echo_the_check_pattern_is_refused() {
        let mut fake = FakeCard::new(1024);
        fake.v1 = true;
        assert!(matches!(
            Card::init(fake).err().unwrap(),
            SdError::Reply(8, _)
        ));
    }

    #[test]
    fn a_write_the_card_never_finishes_is_an_error_not_a_hang() {
        // Real-failing: the card never reports ready after the write.
        let mut fake = FakeCard::new(1024);
        fake.stuck_busy = true;
        let mut card = Card::init(fake).unwrap();
        let error = card.write(0, &[1u8; BLOCK]).unwrap_err();
        assert!(matches!(error, SdError::Reply(13, _)), "{error}");
    }

    #[test]
    fn a_write_past_the_card_or_not_on_blocks_is_refused_before_it_goes() {
        let mut m = medium();
        assert!(m.write(0, &[1u8; 100]).is_err());
        assert!(m.write(1024 * 512, &[1u8; 512]).is_err());
        assert!(m.erase(100, 512).is_err());
    }

    #[test]
    fn the_boot_mode_is_read_from_soc_boot_ctl_bits_one_and_zero() {
        assert_eq!(boot_mode(0x3), BootMode::SdCard);
        assert_eq!(boot_mode(0x2), BootMode::Emmc);
        assert_eq!(boot_mode(0x1), BootMode::Nand);
        assert_eq!(boot_mode(0x0), BootMode::Nor);
        assert_eq!(boot_mode(0xffff_fffc), BootMode::Nor);
    }

    #[test]
    fn an_sd_boot_with_no_parameter_is_refused_by_name_and_a_parameter_wins() {
        let error = resolve_controller(BootMode::SdCard, None).unwrap_err();
        assert!(error.to_string().contains("not known"), "{error}");
        assert_eq!(
            resolve_controller(BootMode::SdCard, Some(SdController::Sdhci1)),
            Ok(SdController::Sdhci1)
        );
        assert_eq!(
            resolve_controller(BootMode::Nor, Some(SdController::Sdhci0)),
            Ok(SdController::Sdhci0),
            "the parameter is the host's choice, and it stands"
        );
    }

    #[test]
    fn an_unknown_parameter_and_a_nor_boot_are_refused_by_name() {
        let error = controller_from_parameter(7).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unknown SD controller parameter"),
            "{error}"
        );
        assert_eq!(controller_from_parameter(1), Ok(SdController::Sdhci1));
        let nor = resolve_controller(BootMode::Nor, None).unwrap_err();
        assert!(nor.to_string().contains("NOR"), "{nor}");
    }

    #[test]
    fn the_controller_bases_are_the_device_tree_ones() {
        assert_eq!(SdController::Sdhci0.base(), 0x9158_0000);
        assert_eq!(SdController::Sdhci1.base(), 0x9158_1000);
    }

    #[test]
    fn the_card_check_reports_the_cid_capacity_and_two_block_crcs() {
        let mut card = Card::init(FakeCard::new(1024)).unwrap();
        let check = card.check().unwrap();
        assert_eq!(check.cid, [0x5c; 16]);
        assert_eq!(check.blocks, 1024);
        // Both reads of a zeroed card give the same CRC: the check sees the data, not a fault.
        assert_eq!(check.crc_first, check.crc_middle);
    }

    #[test]
    fn a_card_that_stops_answering_mid_check_is_an_error_to_report_as_a_card_fault_candidate() {
        // A card whose reads are refused (here, past its capacity) makes the check an error, not a pass.
        let mut card = Card::init(FakeCard::new(1024)).unwrap();
        card.blocks = 0;
        assert!(card.check().is_err());
    }

    #[test]
    fn the_csd_fields_are_read_from_the_bit_positions_the_spec_gives() {
        let mut bytes = [0u8; 16];
        bytes[15] = 0x05; // bits 2 and 0 of bits 7:0
        assert_eq!(field(&bytes, 2, 0), 5);
        bytes[0] = 0x40; // bit 126
        assert_eq!(field(&bytes, 127, 126), 1);
    }
}
