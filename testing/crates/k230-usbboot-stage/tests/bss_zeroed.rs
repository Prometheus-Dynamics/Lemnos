//! The image's `.bss` is NOLOAD: the boot ROM copies only the file, so `.bss` holds whatever SRAM held before. The
//! entry must zero it before any static is read, and the linker script must name its bounds. The R1e run read a
//! give-up flag (`GAVE_UP`, in `.bss`) as set, with no `T` printed: the stale value from the run before. These tests
//! read the sources and fail if the bounds or the zeroing are missing.

const LINKER_SCRIPT: &str = include_str!("../../../../drivers/k230-usbboot-image/image.ld");
const ENTRY: &str = include_str!("../../../../drivers/k230-usbboot-sys/src/image.rs");

#[test]
fn the_linker_script_names_the_bss_bounds() {
    assert!(
        LINKER_SCRIPT.contains("lemnos_bss_start"),
        "image.ld must set lemnos_bss_start"
    );
    assert!(
        LINKER_SCRIPT.contains("lemnos_bss_end"),
        "image.ld must set lemnos_bss_end"
    );
}

#[test]
fn the_entry_zeroes_bss_before_any_static_is_read() {
    let zeroing = ENTRY
        .find("la t0, lemnos_bss_start")
        .expect("the entry zeroes .bss from its start");
    assert!(
        ENTRY.contains("la t1, lemnos_bss_end"),
        "the entry zeroes .bss to its end"
    );
    let first_static = ENTRY
        .find("la t6, lemnos_diag")
        .expect("the entry stores the diagnostic copy");
    assert!(
        zeroing < first_static,
        "the zeroing comes before the first store to a static"
    );
}
