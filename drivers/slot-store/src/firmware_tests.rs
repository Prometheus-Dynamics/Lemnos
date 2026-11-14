use super::*;

#[test]
fn pi_firmware_text_is_the_fixed_fallback_text() {
    assert_eq!(
        CONFIG_TXT,
        "kernel_watchdog_timeout=15\ninclude default.txt\n[boot_count&4]\ninclude previous.txt\n[all]\n"
    );
    assert_eq!(
        TRYBOOT_TXT,
        "kernel_watchdog_timeout=15\ninclude trial.txt\n"
    );
    assert!(
        !TRYBOOT_TXT.contains("boot_count"),
        "a trial is never subject to the count"
    );
}

#[test]
fn the_count_pass_picks_default_for_0_to_3_and_previous_for_4_to_7_for_ever() {
    for count in 0..=255u8 {
        let want = if (count / 4) % 2 == 0 {
            Pick::Default
        } else {
            Pick::Previous
        };
        assert_eq!(pick(count), want, "count {count}");
    }
    // The 8-bit count wraps at 256, a multiple of 8: the pattern has no break.
    assert_eq!(pick(0), pick(8));
    assert_eq!(pick(4), pick(12));
    assert_eq!(pick(255), Pick::Previous);
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
