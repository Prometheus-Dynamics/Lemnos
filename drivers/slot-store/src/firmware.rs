//! The Raspberry Pi firmware family's fallback, as fixed text and the one pure rule the firmware
//! applies to it. Everything here is fixed at image build except the three pointer files, which
//! change only by a one-sector write ([`pointer_sector`]).
//!
//! The firmware reads `config.txt` (or `tryboot.txt` on a tryboot restart). The expression filter
//! `[boot_count&4]` selects `previous.txt` when bit 2 of the boot count is set. With the count
//! wrapping at 256 (a multiple of 8), boots 0-3 and 8-11 run the default, 4-7 and 12-15 the
//! previous, and so on for ever: three attempts on each round before the previous is tried.
//!
//! Sources: `config_txt_boot.adoc` (`kernel_watchdog_timeout`), `conditional.adoc` (`boot_count` and
//! the expression filter), `rn2712.md` (the SoC watchdog's 16.8 s maximum). The firmware's own
//! behaviour with this text is to be run on a board (`testing/board-runs/I5i3.md`).

/// The one-sector size of a pointer file.
pub const POINTER_BYTES: usize = 512;

/// The watchdog timeout the kernel driver reports on the board, in seconds (the SoC maximum is 16.8).
pub const WATCHDOG_SECONDS: u8 = 15;

/// `config.txt`: the default, unless the count's bit 2 is set; then the previous.
pub const CONFIG_TXT: &str = "kernel_watchdog_timeout=15\n\
include default.txt\n\
[boot_count&4]\n\
include previous.txt\n\
[all]\n";

/// `tryboot.txt`: the trial, once, with no count filter.
pub const TRYBOOT_TXT: &str = "kernel_watchdog_timeout=15\n\
include trial.txt\n";

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

/// The pointer the firmware picks for a boot whose count is `count`: the expression filter's
/// `&4`, applied exactly as `config.txt` writes it.
pub const fn pick(count: u8) -> Pick {
    if count & 4 == 0 {
        Pick::Default
    } else {
        Pick::Previous
    }
}

/// A pointer file's content: one `os_prefix` line naming the slot, padded with a comment line to
/// exactly one sector. `None` for an id that is not a valid slot id.
pub fn pointer_sector(id: &str) -> Option<[u8; POINTER_BYTES]> {
    if !crate::store::valid_id(id) {
        return None;
    }
    let line = format!("os_prefix=slots/{id}/\n");
    let mut out = [b'#'; POINTER_BYTES];
    out[..line.len()].copy_from_slice(line.as_bytes());
    out[POINTER_BYTES - 1] = b'\n';
    Some(out)
}

/// The slot a pointer sector names, if it is one whole pointer line and nothing else but padding.
pub fn pointer_slot(sector: &[u8; POINTER_BYTES]) -> Option<String> {
    let end = sector.iter().position(|&b| b == b'\n')?;
    let text = std::str::from_utf8(&sector[..end]).ok()?;
    let id = text.strip_prefix("os_prefix=slots/")?.strip_suffix('/')?;
    if !crate::store::valid_id(id) || end + 1 > POINTER_BYTES {
        return None;
    }
    let rest = &sector[end + 1..];
    rest.iter()
        .all(|&b| b == b'#' || b == b'\n')
        .then(|| id.to_string())
}

#[cfg(test)]
#[path = "firmware_tests.rs"]
mod tests;
