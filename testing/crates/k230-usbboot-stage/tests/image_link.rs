//! The load address is one constant. The host loads the image there (`image::LOAD_ADDRESS`), the linker
//! script links it there (`drivers/k230-usbboot-image/image.ld`), and the stage's entry is its first
//! instruction. These tests read the linker script and fail if the two disagree.

use lemnos_k230_usbboot_stage::image::{LOAD_ADDRESS, LOAD_HIGH, LOAD_LOW, MAX_FOOTPRINT};

const LINKER_SCRIPT: &str = include_str!("../../../../drivers/k230-usbboot-image/image.ld");

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

/// The address a `.name 0x... (NOLOAD)` section line places a section at.
fn section_at(name: &str) -> u32 {
    let prefix = format!(".{name} 0x");
    let line = LINKER_SCRIPT
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with(&prefix))
        .unwrap_or_else(|| panic!("the linker script places .{name} at an explicit address"));
    let hex: String = line[prefix.len()..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    u32::from_str_radix(&hex, 16).expect("the address is hex")
}

#[test]
fn the_stack_and_its_guard_band_sit_below_the_code_in_that_order() {
    // The guard band, then the 16 KiB reserve, then the code at the origin: nothing of the image is below the stack.
    let guard = section_at("guard");
    let stack = section_at("stack");
    assert!(
        guard < stack,
        "guard {guard:#x} is below the stack {stack:#x}"
    );
    assert_eq!(stack - guard, 0x100, "the guard band is 256 bytes");
    assert_eq!(
        stack + 0x4000,
        LOAD_ADDRESS,
        "the reserve is 16 KiB and its top is the code's first byte"
    );
    assert!(
        guard >= LOAD_LOW,
        "the guard band is inside the vendor's range"
    );
}
