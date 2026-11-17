use super::*;

#[test]
fn pi_firmware_text_is_the_fixed_fallback_text() {
    assert_eq!(
        CONFIG_TXT,
        "kernel_watchdog_timeout=15\ninclude default.txt\n[boot_count>3]\ninclude previous.txt\n[all]\n\
            kernel=kernel8.img\ncmdline=cmdline.txt\ninitramfs initramfs.cpio followkernel\n\
            device_tree=board.dtb\ndtoverlay=lemnos-board\n"
    );
    assert_eq!(
        TRYBOOT_TXT,
        "kernel_watchdog_timeout=15\ninclude trial.txt\nkernel=kernel8.img\ncmdline=cmdline.txt\n\
            initramfs initramfs.cpio followkernel\ndevice_tree=board.dtb\ndtoverlay=lemnos-board\n"
    );
    assert!(
        !TRYBOOT_TXT.contains("boot_count"),
        "a trial is never subject to the count"
    );
}

/// The official expression filter's semantics: `[ARG&MASK]` is selected when `ARG & MASK != 0`, and
/// `[ARG>VALUE]` when `ARG > VALUE` (conditional.adoc, raspberrypi/documentation commit b01a5c1a).
fn selected_by_mask(arg: u8, mask: u8) -> bool {
    arg & mask != 0
}

#[test]
fn the_firmware_picks_default_up_to_the_threshold_and_previous_above_it_for_every_count() {
    // Over all 256 counts, under the documented `[boot_count>3]`: the default at counts 0 to 3, the
    // previous from 4 to 255. The count wraps to 0 after 255, so the default comes back there.
    for count in 0..=255u8 {
        let want = if count <= FALLBACK_AFTER {
            Pick::Default
        } else {
            Pick::Previous
        };
        assert_eq!(pick(count), want, "count {count}");
    }
    // Two rounds of the same pattern must not depend on the count above the threshold.
    assert_eq!(pick(4), Pick::Previous);
    assert_eq!(pick(8), Pick::Previous);
    assert_eq!(pick(255), Pick::Previous);
}

#[test]
fn the_mask_form_would_bounce_between_the_pointers_and_the_threshold_form_does_not() {
    // `[boot_count&4]` is selected at counts 4 to 7 and 12 to 15, and not at 8 to 11: a board that keeps
    // failing would bounce between previous and default. The threshold form has no such break, and
    // the test fails if the mask form is used (it disagrees with `pick` at count 8).
    let mut bounces = 0;
    for count in 0..=255u8 {
        let mask_picks_previous = selected_by_mask(count, 4);
        if mask_picks_previous != (pick(count) == Pick::Previous) {
            bounces += 1;
        }
    }
    assert!(
        bounces > 0,
        "the mask form was expected to disagree with the threshold"
    );
    assert!(!selected_by_mask(8, 4) && pick(8) == Pick::Previous);
    assert!(CONFIG_TXT.contains("[boot_count>3]"));
    assert!(!CONFIG_TXT.contains("&4"));
}

#[test]
fn the_count_fallback_is_on_pi5_family_boards_only_and_the_other_pis_get_no_filter() {
    assert_eq!(config_for(true), CONFIG_TXT);
    assert_eq!(config_for(false), CONFIG_TXT_NO_COUNT);
    assert!(!CONFIG_TXT_NO_COUNT.contains("boot_count"));
    assert!(!CONFIG_TXT_NO_COUNT.contains("previous.txt"));
    assert!(CONFIG_TXT_NO_COUNT.contains(SELECT_LINE));
}

#[test]
fn pointer_sector_is_one_sector_with_the_prefix_and_refuses_a_bad_slot_id() {
    let s = pointer_sector("os-2").unwrap();
    assert_eq!(s.len(), POINTER_BYTES);
    assert!(s.starts_with(b"os_prefix=slots/os-2/\n"));
    assert_eq!(pointer_slot(&s).as_deref(), Some("os-2"));
    assert!(pointer_sector("../etc").is_none());
    assert!(pointer_sector("bad id").is_none());
}

#[test]
fn a_pointer_with_anything_but_padding_after_its_line_is_not_a_pointer() {
    let mut s = pointer_sector("os-2").unwrap();
    s[100] = b'x';
    assert_eq!(pointer_slot(&s), None);
    let mut blank = [0u8; POINTER_BYTES];
    assert_eq!(pointer_slot(&blank), None, "no line at all");
    blank[..6].copy_from_slice(b"junk\n\n");
    assert_eq!(pointer_slot(&blank), None);
}

#[test]
fn a_pointer_torn_at_any_byte_is_one_of_the_old_line_the_new_line_or_refused() {
    let old = pointer_sector("os-1").unwrap();
    let new = pointer_sector("os-2").unwrap();
    for cut in 0..=POINTER_BYTES {
        let mut torn = old;
        torn[..cut].copy_from_slice(&new[..cut]);
        match pointer_slot(&torn) {
            None => {}
            Some(id) => assert!(
                id == "os-1" || id == "os-2",
                "cut {cut} read an unknown slot {id}"
            ),
        }
    }
}

#[test]
fn a_pointer_torn_between_ids_of_different_length_is_never_another_slot() {
    // "a" to "bb": a cut inside the id can leave "os_prefix=slots/b/" whole, which a reader
    // without the checksum would take for slot "b". Every cut must read as "a", "bb" or nothing.
    for (old_id, new_id) in [("a", "bb"), ("bb", "a"), ("x.y-1", "z_2"), ("q", "q.r")] {
        let old = pointer_sector(old_id).unwrap();
        let new = pointer_sector(new_id).unwrap();
        let mut refused = 0;
        for cut in 0..=POINTER_BYTES {
            let mut torn = old;
            torn[..cut].copy_from_slice(&new[..cut]);
            match pointer_slot(&torn) {
                None => refused += 1,
                Some(id) => assert!(
                    id == old_id || id == new_id,
                    "{old_id}->{new_id} cut {cut} read {id}"
                ),
            }
        }
        assert!(refused > 0, "{old_id}->{new_id}: no cut was refused");
    }
}

#[test]
fn a_pointer_whose_checksum_does_not_match_its_line_is_refused() {
    let mut s = pointer_sector("os-2").unwrap();
    // Change one digit of the checksum: the line itself is still a valid prefix line.
    let at = s.iter().position(|&b| b == b'=').unwrap() + 1;
    s[at] = if s[at] == b'0' { b'1' } else { b'0' };
    assert_eq!(pointer_slot(&s), None);
    // A line with a different id under the old checksum is refused too.
    let mut swapped = pointer_sector("os-1").unwrap();
    let other = pointer_sector("os-2").unwrap();
    swapped[..21].copy_from_slice(&other[..21]);
    assert_eq!(pointer_slot(&swapped), None);
}

#[test]
fn a_board_with_rpi_tryboot_gets_the_fixed_text_and_no_other_firmware_does() {
    let files = generated_for("rpi-tryboot").unwrap();
    assert_eq!(files[0], ("config.txt", CONFIG_TXT));
    assert_eq!(files[1], ("tryboot.txt", TRYBOOT_TXT));
    assert!(generated_for("u-boot").is_none());
    assert!(generated_for("").is_none());
}

/// Each slot line of both files names the layout's file, spelled as the layout spells it.
#[test]
fn the_slot_lines_name_the_layout_files() {
    use crate::layout::{BOARD_OVERLAY_NAME, CMDLINE, DEVICE_TREE, INITRAMFS, KERNEL};
    for text in [CONFIG_TXT, TRYBOOT_TXT] {
        assert!(text.contains(&format!("kernel={KERNEL}\n")), "{text}");
        assert!(text.contains(&format!("cmdline={CMDLINE}\n")), "{text}");
        assert!(
            text.contains(&format!("initramfs {INITRAMFS} followkernel\n")),
            "{text}"
        );
        assert!(
            text.contains(&format!("device_tree={DEVICE_TREE}\n")),
            "{text}"
        );
        assert!(
            text.contains(&format!("dtoverlay={BOARD_OVERLAY_NAME}\n")),
            "{text}"
        );
    }
}

/// The slot lines come after the include that sets `os_prefix`, in both files, so the firmware
/// applies them to the slot the pointer names.
#[test]
fn the_slot_lines_follow_the_include() {
    let at = |text: &str, needle: &str| text.find(needle).unwrap();
    assert!(at(CONFIG_TXT, "include previous.txt") < at(CONFIG_TXT, "kernel="));
    assert!(at(CONFIG_TXT, "[all]") < at(CONFIG_TXT, "kernel="));
    assert!(at(TRYBOOT_TXT, "include trial.txt") < at(TRYBOOT_TXT, "kernel="));
}

#[test]
fn a_first_boot_takes_the_count_as_the_baseline_and_counts_nothing() {
    let since = since_good(50, None);
    assert_eq!(since, Since::First(50));
    assert_eq!(since.restarts(), 0);
    assert_eq!(since.baseline(), 50);
}

#[test]
fn restarts_are_the_count_above_a_kept_baseline() {
    let since = since_good(58, Some(50));
    assert_eq!(since.restarts(), 8);
    assert_eq!(since.baseline(), 50);
}

#[test]
fn a_count_below_the_baseline_is_a_power_loss_and_the_baseline_goes_to_zero() {
    let since = since_good(3, Some(50));
    assert_eq!(since, Since::PowerLoss(3));
    assert_eq!(since.restarts(), 3);
    assert_eq!(since.baseline(), 0);
}

#[test]
fn a_baseline_of_zero_is_a_kept_baseline_not_an_unknown_one() {
    assert_eq!(since_good(8, Some(0)).restarts(), 8);
}

#[test]
fn a_config_txt_without_the_default_line_is_refused_by_name_and_the_image_texts_pass() {
    assert!(check_config_text(CONFIG_TXT.as_bytes()).is_ok());
    assert!(check_config_text(CONFIG_TXT_NO_COUNT.as_bytes()).is_ok());
    let without: String = CONFIG_TXT
        .lines()
        .filter(|l| *l != SELECT_LINE)
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(config_refusal(without.as_bytes()), Some(SELECT_REFUSAL));
    let why = check_config_text(without.as_bytes()).unwrap_err();
    assert!(why.contains("include default.txt"), "{why}");
    assert!(why.contains("Lemnos never edits config.txt"), "{why}");
    assert!(check_config_text(b"kernel_watchdog_timeout=15\n").is_err());
    assert_eq!(config_refusal(&[0xff, 0xfe]), Some(NOT_TEXT_REFUSAL));
}

/// The text every Pi 5 board carried before the threshold: its `ARG&MASK` filter is a refusal now.
const OLD_TEXT: &str = "kernel_watchdog_timeout=15\ninclude default.txt\n[boot_count&4]\ninclude previous.txt\n[all]\n\
kernel=kernel8.img\ncmdline=cmdline.txt\ninitramfs initramfs.cpio followkernel\ndevice_tree=board.dtb\ndtoverlay=lemnos-board\n";

#[test]
fn a_config_txt_with_the_old_mask_filter_is_refused_by_name_and_the_threshold_text_is_not() {
    // The old text has the default line, so only the filter can refuse it: the refusal names the filter.
    assert_eq!(
        config_refusal(OLD_TEXT.as_bytes()),
        Some(OLD_FILTER_REFUSAL)
    );
    let why = check_config_text(OLD_TEXT.as_bytes()).unwrap_err();
    assert!(why.contains("`[boot_count&4]`"), "{why}");
    assert!(why.contains("rebuild the boot partition offline"), "{why}");
    // The threshold text, and the Pi text with no filter, are accepted.
    assert_eq!(config_refusal(CONFIG_TXT.as_bytes()), None);
    assert_eq!(config_refusal(CONFIG_TXT_NO_COUNT.as_bytes()), None);
}
