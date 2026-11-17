//! The Raspberry Pi firmware family's fallback, as fixed text and the one pure rule the firmware
//! applies to it. Everything here is fixed at image build except the three pointer files, which
//! change only by a one-sector write ([`pointer_sector`]).
//!
//! The firmware reads `config.txt` (or `tryboot.txt` on a tryboot restart). On a Raspberry Pi 5 family
//! board, `[boot_count>3]` selects `previous.txt` when the boot count is above 3 (`FALLBACK_AFTER`). The
//! official filter is `[ARG>VALUE]`, selected when ARG > VALUE (`conditional.adoc` at raspberrypi/documentation
//! commit b01a5c1a). The count is an 8-bit value that wraps at 256, and it is cleared by power loss and by a
//! good boot (Lemnos clears it at a confirmed trial and at a good default boot).
//!
//! Sources: `config_txt_boot.adoc` (`kernel_watchdog_timeout`), `conditional.adoc` (`boot_count` and
//! the expression filter), `rn2712.md` (the SoC watchdog's 16.8 s maximum). The firmware's own
//! behaviour with this text is to be run on a board (`testing/board-runs/I5i3.md`).

/// The one-sector size of a pointer's line, and of each of a pointer file's two copies.
pub const POINTER_BYTES: usize = 512;

/// The size of every pointer file on the partition (`default.txt`, `previous.txt`, `trial.txt`): two
/// sectors holding the same line. The one form for every writer and reader (I5i8): the second sector
/// is written first, so a cut after it leaves the new line in the file, and a torn sector leaves the
/// other whole.
pub const POINTER_FILE_BYTES: usize = 2 * POINTER_BYTES;

/// The watchdog timeout the kernel driver reports on the board, in seconds (the SoC maximum is 16.8).
pub const WATCHDOG_SECONDS: u8 = 15;

/// The slot lines name the slot's files with fixed names (`layout.rs`): the only per-slot difference
/// is the directory `os_prefix` selects, which the include before them sets. They are the same in
/// both files. Their behaviour is from the documentation only until the board run in
/// `testing/board-runs/I5i6.md`. Sources: `boot.adoc` in `raspberrypi/documentation` (`os_prefix`
/// covers kernels, initramfs, cmdline.txt, .dtbs and overlays); `computers/configuration/reference.adoc`
/// part 5.3 "Force a specific Device Tree" (`device_tree=<file>` loads that DTB). That part does not
/// say whether `os_prefix` applies to `device_tree=`: that is an inference from the `.dtbs` line, unverified.
/// The count fallback's threshold: the restarts without a good boot after which the firmware starts the
/// previous slot. It is the design's trial attempt count (`trial_attempts`, 3 by default): boots 0 to 3
/// run the default, boot 4 and later (up to the wrap at 256) the previous. The firmware's filter is
/// `[boot_count>3]`: the official `conditional.adoc` says `[ARG>VALUE]` is selected when ARG > VALUE.
pub const FALLBACK_AFTER: u8 = 3;

/// The line that selects the default pointer. Every Lemnos config.txt carries it, on every Pi board.
pub const SELECT_LINE: &str = "include default.txt";

/// `config.txt` for a board whose firmware has the count (Raspberry Pi 5 family, `boot_count`, see
/// [`FALLBACK_AFTER`]): the default, unless the count is above the threshold; then the previous. The
/// slot lines come after `[all]`, so they apply to both pointers.
pub const CONFIG_TXT: &str = "kernel_watchdog_timeout=15\n\
include default.txt\n\
[boot_count>3]\n\
include previous.txt\n\
[all]\n\
kernel=kernel8.img\n\
cmdline=cmdline.txt\n\
initramfs initramfs.cpio followkernel\n\
device_tree=board.dtb\n\
dtoverlay=lemnos-board\n";

/// `config.txt` for a Raspberry Pi board whose firmware has no `boot_count` (every Pi before the Pi 5
/// family): the default only, and no filter the firmware may not know. There is no count fallback on
/// these boards, and the board file may not ask for one (refused at build).
pub const CONFIG_TXT_NO_COUNT: &str = "kernel_watchdog_timeout=15\n\
include default.txt\n\
[all]\n\
kernel=kernel8.img\n\
cmdline=cmdline.txt\n\
initramfs initramfs.cpio followkernel\n\
device_tree=board.dtb\n\
dtoverlay=lemnos-board\n";

/// The `config.txt` text for a board: with the count fallback (Pi 5 family) or without it.
pub const fn config_for(count_fallback: bool) -> &'static str {
    if count_fallback {
        CONFIG_TXT
    } else {
        CONFIG_TXT_NO_COUNT
    }
}

/// `tryboot.txt`: the trial, once, with no count filter.
pub const TRYBOOT_TXT: &str = "kernel_watchdog_timeout=15\n\
include trial.txt\n\
kernel=kernel8.img\n\
cmdline=cmdline.txt\n\
initramfs initramfs.cpio followkernel\n\
device_tree=board.dtb\n\
dtoverlay=lemnos-board\n";

/// The fixed files a board gets at image build for its firmware, `(file, text)`. Only `rpi-tryboot`
/// has them: the Raspberry Pi firmware family's fallback. `None` for any other firmware (its
/// fallback is the fixed boot stage, not this text).
pub fn generated_for(firmware: &str) -> Option<[(&'static str, &'static str); 2]> {
    match firmware {
        "rpi-tryboot" => Some([("config.txt", CONFIG_TXT), ("tryboot.txt", TRYBOOT_TXT)]),
        _ => None,
    }
}

/// Which pointer file the firmware reads for a boot with this count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// `default.txt`.
    Default,
    /// `previous.txt`.
    Previous,
}

/// The pointer the firmware picks for a boot whose count is `count`: `[boot_count>3]` in `config.txt`
/// selects the previous when the count is above [`FALLBACK_AFTER`].
pub const fn pick(count: u8) -> Pick {
    if count > FALLBACK_AFTER {
        Pick::Previous
    } else {
        Pick::Default
    }
}

/// The restarts since the last good boot, from the firmware's count (`chosen/bootloader/count`, the
/// restarts since power loss, which no Lemnos boot clears) and the baseline record (M36b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Since {
    /// No baseline is kept (a first flash, or a first boot): this count is the baseline, and no
    /// restart has been counted yet.
    First(u32),
    /// The count is below the baseline: a power loss reset it. The baseline is zero and the count
    /// is the restarts since the loss.
    PowerLoss(u32),
    /// The restarts since the kept baseline.
    Counted {
        /// The restarts: the count less the baseline.
        restarts: u32,
        /// The baseline the restarts are counted from.
        baseline: u32,
    },
}

impl Since {
    /// The restarts counted since the last good boot (zero for a first boot).
    pub const fn restarts(self) -> u32 {
        match self {
            Since::First(_) => 0,
            Since::PowerLoss(count) => count,
            Since::Counted { restarts, .. } => restarts,
        }
    }

    /// The baseline the record must hold after this boot.
    pub const fn baseline(self) -> u32 {
        match self {
            Since::First(count) => count,
            Since::PowerLoss(_) => 0,
            Since::Counted { baseline, .. } => baseline,
        }
    }
}

/// The restarts since the last good boot. Acts only on the baseline, never on the raw count alone:
/// a count of 50 on a board flashed today with no baseline is a first boot, not 50 restarts.
pub const fn since_good(count: u32, baseline: Option<u32>) -> Since {
    match baseline {
        None => Since::First(count),
        Some(base) if count < base => Since::PowerLoss(count),
        Some(base) => Since::Counted {
            restarts: count - base,
            baseline: base,
        },
    }
}

/// The line that carries the checksum of the pointer line above it: `#lemnos-crc32=` and eight hex
/// digits, then a newline. It is a comment to the firmware.
const CHECK_PREFIX: &str = "#lemnos-crc32=";
/// The checksum line's length in bytes, newline included.
const CHECK_BYTES: usize = CHECK_PREFIX.len() + 8 + 1;

/// A pointer file's content: the `os_prefix` line naming the slot, a checksum line over that
/// line's bytes, then `#` padding and a final newline, exactly one sector. The checksum is what
/// lets a torn sector be refused: slot ids have different lengths, so a cut inside the new line
/// could otherwise leave a whole, valid, different line. `None` for an id that is not a valid slot id.
pub fn pointer_sector(id: &str) -> Option<[u8; POINTER_BYTES]> {
    if !crate::store::valid_id(id) {
        return None;
    }
    let line = format!("os_prefix=slots/{id}/\n");
    let check = format!(
        "{CHECK_PREFIX}{:08x}\n",
        crate::twocopy::crc32(line.as_bytes())
    );
    if line.len() + check.len() >= POINTER_BYTES {
        return None;
    }
    let mut out = [b'#'; POINTER_BYTES];
    out[..line.len()].copy_from_slice(line.as_bytes());
    out[line.len()..line.len() + check.len()].copy_from_slice(check.as_bytes());
    out[POINTER_BYTES - 1] = b'\n';
    Some(out)
}

/// The slot a pointer sector names: exactly one whole `os_prefix` line, a checksum line that
/// matches it, and `#` padding ending in a newline. Anything else (a torn or foreign sector, a
/// line with a bad checksum) is `None`.
pub fn pointer_slot(sector: &[u8; POINTER_BYTES]) -> Option<String> {
    let end = sector.iter().position(|&b| b == b'\n')? + 1;
    let line = &sector[..end];
    let text = std::str::from_utf8(line).ok()?;
    let id = text.strip_prefix("os_prefix=slots/")?.strip_suffix("/\n")?;
    if !crate::store::valid_id(id) {
        return None;
    }
    let check = sector.get(end..end + CHECK_BYTES)?;
    let check = std::str::from_utf8(check).ok()?;
    let hex = check.strip_prefix(CHECK_PREFIX)?.strip_suffix('\n')?;
    if hex.len() != 8 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let want = u32::from_str_radix(hex, 16).ok()?;
    if crate::twocopy::crc32(line) != want {
        return None;
    }
    let rest = &sector[end + CHECK_BYTES..];
    let (last, body) = rest.split_last()?;
    (*last == b'\n' && body.iter().all(|&b| b == b'#')).then(|| id.to_string())
}

/// A pointer file's bytes for `id`: [`pointer_sector`] twice, the first sector and then the second.
/// `None` for an id that is not a valid slot id (or a line that does not fit a sector).
pub fn pointer_file(id: &str) -> Option<[u8; POINTER_FILE_BYTES]> {
    let sector = pointer_sector(id)?;
    let mut out = [0u8; POINTER_FILE_BYTES];
    out[..POINTER_BYTES].copy_from_slice(&sector);
    out[POINTER_BYTES..].copy_from_slice(&sector);
    Some(out)
}

/// The slot a pointer file names: the last whole line of its two sectors, the second sector first
/// (the copy written first is the one the firmware reads when both are whole). `None` when neither
/// sector holds a whole, checksummed line.
pub fn pointer_file_slot(file: &[u8; POINTER_FILE_BYTES]) -> Option<String> {
    let (first, second) = file.split_at(POINTER_BYTES);
    let second: &[u8; POINTER_BYTES] = second.try_into().ok()?;
    let first: &[u8; POINTER_BYTES] = first.try_into().ok()?;
    pointer_slot(second).or_else(|| pointer_slot(first))
}

/// The refusal for a `config.txt` that lacks [`SELECT_LINE`], and for one that is not text. Static, so
/// the driver's health can carry it without allocating.
pub const SELECT_REFUSAL: &str = "config.txt has no `include default.txt` line: Lemnos never edits config.txt, so no slot is set up on this boot partition until its config.txt carries the lines of the image's own text (firmware::CONFIG_TXT)";
/// The refusal for a `config.txt` carrying the old mask filter (`[boot_count&4]`, the earlier Pi 5 text). Its
/// `ARG&MASK` form is selected at counts 4 to 7 and 12 to 15 and not at 8 to 11, so the firmware bounces
/// between the pointers, and `pick` models the threshold only. Lemnos never rewrites config.txt, so such a
/// board is refused until its boot partition is rebuilt offline.
pub const OLD_FILTER_REFUSAL: &str = "config.txt carries the old count filter `[boot_count&4]`, whose mask form bounces between the pointers: Lemnos never rewrites config.txt, so no slot is set up on this boot partition; rebuild the boot partition offline (lemnos-ctl boot-partition) to get the threshold form `[boot_count>3]`";
/// The refusal for a `config.txt` that is not text.
pub const NOT_TEXT_REFUSAL: &str = "config.txt is not text: Lemnos never edits config.txt, so no slot is set up on this boot partition until its config.txt is the image's own text";

/// The reason a `config.txt` read from the boot partition cannot select the default, `None` when it can.
/// Read only; nothing is edited.
pub fn config_refusal(text: &[u8]) -> Option<&'static str> {
    let Ok(text) = std::str::from_utf8(text) else {
        return Some(NOT_TEXT_REFUSAL);
    };
    if text.lines().any(|l| l.trim().starts_with("[boot_count&")) {
        return Some(OLD_FILTER_REFUSAL);
    }
    (!text.lines().any(|l| l.trim() == SELECT_LINE)).then_some(SELECT_REFUSAL)
}

/// [`config_refusal`] as a result.
pub fn check_config_text(text: &[u8]) -> Result<(), String> {
    config_refusal(text).map_or(Ok(()), |why| Err(why.to_string()))
}

#[cfg(test)]
#[path = "firmware_tests.rs"]
mod tests;
