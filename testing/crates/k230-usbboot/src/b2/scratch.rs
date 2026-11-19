//! The host's side of the B2 card writes (`testing/board-runs/K230-USB.md`, "B2 card writes"): step 5 of the run
//! for the scratch build, which serves the card only inside the scratch range
//! (`lemnos_k230_usbboot_stage::fl1::scratch`).
//!
//! The order. Every step must hold; the first that does not stops the step and the run sends END and nothing more
//! ([`super::claim_and_check`]):
//!
//! 1. **Identify**: the scratch build's record, byte for byte. Any other build (the check build answers on the
//!    same mode, by design) gets nothing but END.
//! 2. **Before**: the whole range read over USB. It must be zeros (as the survey found it, R0): anything else stops
//!    the run before the first write.
//! 3. **The edges**: the first sector, then the last, each written (status 0, after the stage's own read-back
//!    compare) and read back over USB and compared.
//! 4. **The range**: 16 writes of the most sectors (128) of a known pattern, each with status 0 and read back over
//!    USB and compared; every other one carries the compared mark, which must change nothing inside the range.
//! 5. **The deliberate writes outside** ([`REFUSALS`]), each sent once without and once with the compared mark:
//!    each must be refused with status 2 (storage), or 4 (not compared) without the mark where it touches the
//!    boot area, which the region gate refuses before the storage. Then the whole range is read back: it must be
//!    what step 4 wrote.
//! 6. **Zero**: 16 writes of 128 zero sectors, each read back over USB and compared; the range is as step 2 found
//!    it.
//! 7. **Identify** again (the session still serves), then END.
//!
//! The card outside the range is not readable over USB (the scratch build refuses reads outside it too): that it is
//! unchanged is shown by the piece path's hashes before and after the run, not here.

use lemnos_flash_wire::frame::{
    MAX_SECTORS, Request, SECTOR, STATUS_NOT_COMPARED, STATUS_OK, STATUS_STORAGE,
};
use lemnos_flash_wire::identity;
use lemnos_flash_wire::region::Class;
use lemnos_k230_usbboot_stage::fl1::REGIONS;
use lemnos_k230_usbboot_stage::fl1::scratch::{
    CARD_OF_RECORD_BLOCKS, SCRATCH_BUILD_ID, SCRATCH_FIRST, SCRATCH_IDENTITY, SCRATCH_LAST,
};

use super::{Bounds, Link, REPLY_MAX, end, pattern, read_frames, write_marked};

/// The card of record's backup GPT header (the primary header's alternate, read in the survey).
pub const BACKUP_GPT_HEADER: u32 = 415_743;

/// The range's length in sectors.
pub const RANGE_SECTORS: u32 = SCRATCH_LAST - SCRATCH_FIRST + 1;

/// The writes outside the range the run sends on purpose: (what, first sector, sectors). None may reach the card.
pub const REFUSALS: [(&str, u32, u16); 11] = [
    ("the sector before the range", SCRATCH_FIRST - 1, 1),
    ("the sector after the range", SCRATCH_LAST + 1, 1),
    ("two sectors across the first", SCRATCH_FIRST - 1, 2),
    ("two sectors across the last", SCRATCH_LAST, 2),
    (
        "the most sectors, ending past the last",
        SCRATCH_LAST - (MAX_SECTORS as u32 - 2),
        MAX_SECTORS,
    ),
    ("sector 0 (the protective MBR)", 0, 1),
    ("sector 1 (the primary GPT header)", 1, 1),
    ("sector 2048 (a boot header block)", 2048, 1),
    ("sector 3072 (a boot header block)", 3072, 1),
    ("the backup GPT header", BACKUP_GPT_HEADER, 1),
    (
        "the card's last sector",
        (CARD_OF_RECORD_BLOCKS - 1) as u32,
        1,
    ),
];

/// The status a deliberate write outside the range must get: 4 without the compared mark where it touches a region
/// that needs the mark (the region gate, before the storage), else 2 (the storage refuses it whole).
pub fn expected_refusal(lba: u32, count: u16, compared: bool) -> u8 {
    let end = lba + u32::from(count);
    let gated = REGIONS
        .iter()
        .any(|r| r.class != Class::Plain && lba < r.end && end > r.first);
    if gated && !compared {
        STATUS_NOT_COMPARED
    } else {
        STATUS_STORAGE
    }
}

/// The pattern step 4 writes at the `i`-th 128-sector write.
pub fn range_pattern(i: u32) -> Vec<u8> {
    pattern(usize::from(MAX_SECTORS), 0x40 + i as u8)
}

/// Reads the whole range in 128-sector reads.
fn read_range(link: &mut dyn Link, bounds: &Bounds) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(RANGE_SECTORS as usize * SECTOR);
    for i in 0..RANGE_SECTORS / u32::from(MAX_SECTORS) {
        let lba = SCRATCH_FIRST + i * u32::from(MAX_SECTORS);
        match read_frames(link, bounds, lba, MAX_SECTORS)? {
            Ok(data) => out.extend_from_slice(&data),
            Err(status) => {
                return Err(format!("the read of 128 sectors at {lba}: status {status}"));
            }
        }
    }
    Ok(out)
}

/// Writes `payload` at `lba` and reads it back over USB: status 0 and equal, or why not.
fn write_and_check(
    link: &mut dyn Link,
    bounds: &Bounds,
    lba: u32,
    payload: &[u8],
    compared: bool,
) -> Result<(), String> {
    let count = payload.len() / SECTOR;
    match write_marked(link, bounds, lba, payload, compared)? {
        STATUS_OK => {}
        other => return Err(format!("the write of {count} at {lba}: status {other}")),
    }
    match read_frames(link, bounds, lba, count as u16)? {
        Ok(back) if back == payload => Ok(()),
        Ok(_) => Err(format!(
            "the write of {count} at {lba}: read back over USB, it differs from what was written"
        )),
        Err(status) => Err(format!(
            "the read back of {count} at {lba}: status {status}"
        )),
    }
}

fn identify(link: &mut dyn Link, bounds: &Bounds) -> Result<(), String> {
    link.bulk_out(&Request::identify().encode(), bounds.bulk)
        .map_err(|e| format!("the identify request: {e}"))?;
    let reply = link
        .bulk_in(REPLY_MAX, bounds.bulk)
        .map_err(|e| format!("the identity: {e}"))?;
    if reply == SCRATCH_IDENTITY {
        return Ok(());
    }
    Err(match identity::check(&reply) {
        Ok(peer) => format!(
            "build {:#018x}, not the scratch build {SCRATCH_BUILD_ID:#018x}: nothing more is sent",
            peer.build_id
        ),
        Err(skew) => format!("{skew:?}: nothing more is sent"),
    })
}

/// Steps 1 to 7. Returns whether END was sent and answered; a failure is pushed to `failures` and stops the steps.
pub fn checks(
    link: &mut dyn Link,
    bounds: &Bounds,
    failures: &mut Vec<String>,
    log: &mut dyn FnMut(&str),
) -> bool {
    match steps(link, bounds, log) {
        Ok(()) => match end(link, bounds) {
            Ok(()) => {
                log("END: status 0; the stage reports and resets");
                true
            }
            Err(text) => {
                log(&format!("FAIL END: {text}"));
                failures.push(format!("END: {text}"));
                false
            }
        },
        Err(text) => {
            log(&format!("FAIL {text}; the run stops here"));
            failures.push(text);
            false
        }
    }
}

fn steps(link: &mut dyn Link, b: &Bounds, log: &mut dyn FnMut(&str)) -> Result<(), String> {
    identify(link, b).map_err(|e| format!("identify: {e}"))?;
    log(&format!(
        "identify: the scratch build {SCRATCH_BUILD_ID:#018x}, its record byte for byte"
    ));
    let before = read_range(link, b).map_err(|e| format!("before: {e}"))?;
    if before.iter().any(|&byte| byte != 0) {
        return Err(format!(
            "before: the range {SCRATCH_FIRST}..={SCRATCH_LAST} is not zeros as the survey found it: nothing written"
        ));
    }
    log(&format!(
        "before: the range {SCRATCH_FIRST}..={SCRATCH_LAST} read over USB, {RANGE_SECTORS} sectors, all zeros"
    ));
    for (what, lba, seed, compared) in [
        ("the first sector", SCRATCH_FIRST, 0xf1, false),
        ("the last sector", SCRATCH_LAST, 0xf2, true),
    ] {
        write_and_check(link, b, lba, &pattern(1, seed), compared)?;
        log(&format!(
            "{what} ({lba}, compared mark {compared}): status 0, read back over USB equal"
        ));
    }
    let mut written = Vec::with_capacity(before.len());
    for i in 0..RANGE_SECTORS / u32::from(MAX_SECTORS) {
        let lba = SCRATCH_FIRST + i * u32::from(MAX_SECTORS);
        let payload = range_pattern(i);
        write_and_check(link, b, lba, &payload, i % 2 == 1)?;
        written.extend_from_slice(&payload);
    }
    log(
        "the range: 16 writes of 128 sectors, every other one marked, each status 0 and read back over USB equal",
    );
    for (what, lba, count) in REFUSALS {
        for compared in [false, true] {
            let want = expected_refusal(lba, count, compared);
            match write_marked(link, b, lba, &pattern(usize::from(count), 0xee), compared)? {
                got if got == want => log(&format!(
                    "a write of {count} at {lba}, {what}, compared mark {compared}: status {got}, refused"
                )),
                got => {
                    return Err(format!(
                        "a write of {count} at {lba}, {what}, compared mark {compared}: status {got}, not {want}"
                    ));
                }
            }
        }
    }
    if read_range(link, b).map_err(|e| format!("after the refusals: {e}"))? != written {
        return Err("after the refusals: the range differs from what was written".to_string());
    }
    log("after the refusals: the range read back over USB is what was written");
    let zeros = vec![0u8; usize::from(MAX_SECTORS) * SECTOR];
    for i in 0..RANGE_SECTORS / u32::from(MAX_SECTORS) {
        write_and_check(
            link,
            b,
            SCRATCH_FIRST + i * u32::from(MAX_SECTORS),
            &zeros,
            false,
        )?;
    }
    if read_range(link, b).map_err(|e| format!("after the zeroing: {e}"))? != before {
        return Err("after the zeroing: the range is not as it was before".to_string());
    }
    log(
        "zeroed: 16 writes of 128 zero sectors, each read back equal; the range is as it was before",
    );
    identify(link, b).map_err(|e| format!("identify at the end: {e}"))?;
    log("identify at the end: the same record");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_refusals_are_all_outside_the_range_and_the_boot_area_ones_need_the_mark_first() {
        for (what, lba, count) in REFUSALS {
            let last = lba + u32::from(count) - 1;
            assert!(lba < SCRATCH_FIRST || last > SCRATCH_LAST, "{what}");
        }
        assert_eq!(expected_refusal(0, 1, false), STATUS_NOT_COMPARED);
        assert_eq!(expected_refusal(2048, 1, false), STATUS_NOT_COMPARED);
        assert_eq!(expected_refusal(2048, 1, true), STATUS_STORAGE);
        assert_eq!(
            expected_refusal(SCRATCH_FIRST - 1, 2, false),
            STATUS_STORAGE
        );
        assert_eq!(
            expected_refusal(BACKUP_GPT_HEADER, 1, false),
            STATUS_STORAGE
        );
        assert_eq!(RANGE_SECTORS % u32::from(MAX_SECTORS), 0);
    }
}
