use super::*;

#[test]
fn a_complete_slot_has_every_required_file() {
    assert_eq!(rpi_tryboot_missing(&RPI_TRYBOOT_REQUIRED), Ok(()));
}

#[test]
fn a_missing_file_is_named() {
    let present: Vec<&str> = RPI_TRYBOOT_REQUIRED
        .iter()
        .copied()
        .filter(|name| *name != DEVICE_TREE && *name != OVERLAY_README)
        .collect();
    assert_eq!(
        rpi_tryboot_missing(&present),
        Err(vec![DEVICE_TREE, OVERLAY_README])
    );
}

#[test]
fn the_overlay_names_agree_with_the_directory() {
    assert!(OVERLAY_README.starts_with(OVERLAY_DIR));
    assert!(BOARD_OVERLAY.starts_with(OVERLAY_DIR));
    assert_eq!(
        BOARD_OVERLAY,
        format!("{OVERLAY_DIR}{BOARD_OVERLAY_NAME}.dtbo")
    );
}
