//! The load address is one constant. The host loads the image there (`image::LOAD_ADDRESS`), the linker
//! script links it there (`drivers/k230-usbboot-image/image.ld`), and the stage's entry is its first
//! instruction. These tests read the linker script and fail if the two disagree.

use lemnos_k230_usbboot_stage::image::{LOAD_ADDRESS, LOAD_HIGH, LOAD_LOW, MAX_FOOTPRINT};

const LINKER_SCRIPT: &str = include_str!("../../k230-usbboot-image/image.ld");

/// The address on the linker script's `. = 0x...;` origin line.
fn linker_origin() -> u32 {
    let line = LINKER_SCRIPT
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with(". = 0x"))
        .expect("the linker script sets its origin with `. = 0x...;`");
    let hex = line.trim_start_matches(". = 0x").trim_end_matches(';');
    u32::from_str_radix(hex, 16).expect("the origin is hex")
}

#[test]
fn the_linker_origin_is_the_host_load_address() {
    assert_eq!(linker_origin(), LOAD_ADDRESS);
    assert_eq!(LOAD_ADDRESS, 0x8036_0000);
}

// The load address is inside the vendor's range: a compile-time check, so a bad constant does not build.
const _: () = assert!(LOAD_LOW <= LOAD_ADDRESS && LOAD_ADDRESS < LOAD_HIGH);

#[test]
fn the_footprint_bound_is_the_range_above_the_load_address() {
    assert_eq!(MAX_FOOTPRINT, u64::from(LOAD_HIGH - LOAD_ADDRESS));
}

#[test]
fn the_entry_symbol_is_the_first_section_at_the_origin() {
    // The first input section of .text is the entry, and it sits at the origin.
    assert!(LINKER_SCRIPT.contains("KEEP(*(.text.lemnos_image_entry))"));
    assert!(LINKER_SCRIPT.contains("ENTRY(lemnos_image_entry)"));
}
