//! The stage image's contract with the host: where it is loaded, how big it may be, the mode block
//! the host patches before sending, and the mode words.
//!
//! The load address is fixed, not chosen per run. The image is linked at [`LOAD_ADDRESS`], the
//! vendor tool's default (`kburn_k230.h:31`, `k230_flash.cpp:403`), which is inside the range the
//! vendor tool accepts (`k230_flash.cpp:109`: `0x80300000..=0x80400000`). The host loads it there and
//! nowhere else, so the image is not position-independent: it needs no relocation, and the proof checks
//! the link (`drivers/k230-usbboot-image/check.sh`). Its footprint (code, data, bss, stack) must end
//! inside the range, which bounds it to [`MAX_FOOTPRINT`].
//!
//! The mode block is five words in the image's read-only data: a magic, the mode, its inverse, the SD
//! parameter, and its inverse. The host finds the magic, writes the mode and the parameter, and the
//! image refuses a block whose inverses do not match (it then takes the SoC reset, so a corrupt patch
//! cannot start a stage). Modes are flag bits, so one image runs the stage and every probe run.

/// The load address the image is linked at (`kburn_k230.h:31`).
pub const LOAD_ADDRESS: u32 = 0x8036_0000;
/// The bounds of the vendor tool's load range (`k230_flash.cpp:109`).
pub const LOAD_LOW: u32 = 0x8030_0000;
pub const LOAD_HIGH: u32 = 0x8040_0000;
/// The largest footprint the image may have: from its load address to the top of the range.
pub const MAX_FOOTPRINT: u64 = (LOAD_HIGH - LOAD_ADDRESS) as u64;

/// The block's magic: the bytes `4c 45 4d 4e` ("LEMN") in the image.
pub const BLOCK_MAGIC: u32 = 0x4e4d_454c;
/// The block's length in bytes: five words.
pub const BLOCK_LEN: usize = 20;
/// The SD parameter meaning "none passed": the stage then decides from the boot mode alone.
pub const SD_NONE: u32 = 2;

/// Mode flag: arm `wdt0` (TOP 11).
pub const WDT0: u32 = 1 << 0;
/// Mode flag: arm `wdt1` (TOP 10 in a probe, TOP 11 in the stage).
pub const WDT1: u32 = 1 << 1;
/// Mode flag: take the SoC reset at once, with nothing armed and no spin.
pub const SOC_AT_ONCE: u32 = 1 << 2;
/// Mode flag: run the stage (the medium, the device path, the feed) instead of a probe.
pub const STAGE: u32 = 1 << 3;
/// Mode flag: the report (stage L): the banner, the boot mode, the pad dump, the card's boot area, and the
/// records answered over USB. It writes nothing and feeds nothing; the armed watchdog returns the board.
pub const REPORT: u32 = 1 << 4;

/// Mode flag: one piece of a card flash ([`crate::piece`]): the job loaded beside the image is done on
/// the card the SD parameter names, the outcome is printed, and the SoC reset returns the board. It
/// writes the card.
pub const PIECE: u32 = 1 << 5;

/// Mode flag: the B2 check build ([`crate::fl1::check`]): the FL1 stage's own USB device path, a RAM
/// window and a record in place of the card, never fed. Only an image built for the check (no SD host code
/// linked in) runs it; any other image refuses the flag by its SoC reset.
pub const CHECK: u32 = 1 << 6;

/// Mode flag: the B2 scratch build ([`crate::fl1::scratch`]): the FL1 stage's own USB device path over the card,
/// reachable only inside the scratch range, fed per verified round. It is always sent with [`CHECK`] (mode
/// `scratch`), so an image that is not the scratch build never touches a card on it: the card image refuses
/// [`CHECK`] by its SoC reset, and the check build runs its RAM window and identifies as the check build, which
/// the host refuses before it sends a write. Only the scratch build runs it, and only with both flags.
pub const SCRATCH: u32 = 1 << 7;

/// The mode word of a named run: the five probe runs and the stage.
pub fn mode_word(name: &str) -> Option<u32> {
    match name {
        "spin" => Some(0),
        "wdt0" => Some(WDT0),
        "wdt1" => Some(WDT1),
        "all" => Some(WDT0 | WDT1),
        "soc" => Some(SOC_AT_ONCE),
        "stage" => Some(STAGE | WDT0 | WDT1),
        "report" => Some(REPORT | WDT0),
        "piece" => Some(PIECE | WDT0),
        "check" => Some(CHECK | STAGE | WDT0 | WDT1),
        "scratch" => Some(SCRATCH | CHECK | STAGE | WDT0 | WDT1),
        _ => None,
    }
}

/// The five words of the mode block, as the image reads them.
pub fn block_words(mode: u32, sd: u32) -> [u32; 5] {
    [BLOCK_MAGIC, mode, !mode, sd, !sd]
}

/// Patches the mode block into `image`: finds the magic (four-byte aligned, exactly once) and writes
/// the mode and the SD parameter with their inverses. Any other count of magics is refused.
pub fn patch(image: &mut [u8], mode: u32, sd: u32) -> Result<(), &'static str> {
    let magic = BLOCK_MAGIC.to_le_bytes();
    let mut found = None;
    let mut count = 0;
    for at in (0..image.len().saturating_sub(BLOCK_LEN - 1)).step_by(4) {
        if image[at..at + 4] == magic {
            count += 1;
            found = Some(at);
        }
    }
    if count != 1 {
        return Err("the mode block's magic is not found exactly once");
    }
    let at = found.ok_or("the mode block's magic is not found")?;
    for (i, word) in block_words(mode, sd).iter().enumerate() {
        image[at + 4 * i..at + 4 * i + 4].copy_from_slice(&word.to_le_bytes());
    }
    Ok(())
}

/// Reads the mode block the image has: `(mode, sd)`, or `None` when the magic is wrong or an inverse
/// does not match. A refused block means the image must not start a stage or a probe.
pub fn read_block(words: &[u32; 5]) -> Option<(u32, Option<u32>)> {
    if words[0] != BLOCK_MAGIC || words[2] != !words[1] || words[4] != !words[3] {
        return None;
    }
    let sd = match words[3] {
        SD_NONE => None,
        other => Some(other),
    };
    Some((words[1], sd))
}

/// Checks a footprint: the image's last byte is `footprint` bytes past its load address. Returns the
/// footprint, or the reason it does not fit the range the image is loaded in.
pub fn check_footprint(footprint: u64) -> Result<u64, &'static str> {
    if footprint == 0 {
        Err("the image is empty")
    } else if footprint > MAX_FOOTPRINT {
        Err("the image's footprint runs past the top of the load range")
    } else {
        Ok(footprint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_round_trips_through_the_patch_and_the_read() {
        let mut image = vec![0u8; 64];
        image[16..20].copy_from_slice(&BLOCK_MAGIC.to_le_bytes());
        patch(&mut image, WDT0 | WDT1, 1).unwrap();
        let mut words = [0u32; 5];
        for (i, w) in words.iter_mut().enumerate() {
            *w = u32::from_le_bytes(image[16 + 4 * i..20 + 4 * i].try_into().unwrap());
        }
        assert_eq!(read_block(&words), Some((WDT0 | WDT1, Some(1))));
    }

    #[test]
    fn a_block_with_a_wrong_inverse_is_refused_and_so_is_a_missing_or_doubled_magic() {
        let mut words = block_words(WDT0, 0);
        words[2] = 0;
        assert_eq!(read_block(&words), None, "a corrupt mode is refused");
        let mut image = vec![0u8; 64];
        assert!(patch(&mut image, 0, 0).is_err(), "no magic");
        image[0..4].copy_from_slice(&BLOCK_MAGIC.to_le_bytes());
        image[8..12].copy_from_slice(&BLOCK_MAGIC.to_le_bytes());
        assert!(patch(&mut image, 0, 0).is_err(), "two magics");
    }

    #[test]
    fn the_sd_parameter_none_is_not_a_controller() {
        assert_eq!(read_block(&block_words(0, SD_NONE)), Some((0, None)));
        assert_eq!(read_block(&block_words(0, 1)), Some((0, Some(1))));
    }

    #[test]
    fn the_named_runs_have_their_modes() {
        assert_eq!(mode_word("spin"), Some(0));
        assert_eq!(mode_word("soc"), Some(SOC_AT_ONCE));
        assert_eq!(mode_word("stage"), Some(STAGE | WDT0 | WDT1));
        assert_eq!(mode_word("check"), Some(CHECK | STAGE | WDT0 | WDT1));
        // The scratch mode carries the check flag too: the card image resets on it, never reaching its card code.
        let scratch = mode_word("scratch").unwrap();
        assert_eq!(scratch, SCRATCH | CHECK | STAGE | WDT0 | WDT1);
        assert_ne!(scratch & CHECK, 0);
        assert_eq!(mode_word("check").unwrap() & SCRATCH, 0);
        assert_eq!(mode_word("five"), None);
    }

    #[test]
    fn the_load_range_is_the_vendor_tools_and_the_footprint_must_fit_it() {
        assert_eq!(LOAD_ADDRESS, 0x8036_0000);
        assert_eq!(MAX_FOOTPRINT, 0xA_0000);
        assert!(check_footprint(MAX_FOOTPRINT).is_ok());
        assert!(check_footprint(MAX_FOOTPRINT + 1).is_err());
        assert!(check_footprint(0).is_err());
    }
}
