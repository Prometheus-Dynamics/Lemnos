//! `lemnos-ctl validate` refuses a backend resource name the board's SoC map does not have, by
//! name, before any device is made. A board with no SoC has no map and is not checked here.

use std::sync::atomic::{AtomicU32, Ordering};

const RK3588_SOCS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../socs");

fn validate(text: &str) -> Result<(), String> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnos-ctl-resources-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("board.toml");
    std::fs::write(&path, text).unwrap();
    let mut out = Vec::new();
    let args = [
        "validate".to_string(),
        "--no-live".to_string(),
        "--socs".to_string(),
        RK3588_SOCS.to_string(),
        path.display().to_string(),
    ];
    let result = lemnos_ctl::run(&args, &mut out).map_err(|error| {
        let prefix = format!("{}: ", path.display());
        error.strip_prefix(&prefix).unwrap_or(&error).to_string()
    });
    std::fs::remove_dir_all(&dir).unwrap();
    result
}

fn board(soc: &str, bus: &str) -> String {
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\nsoc = \"{soc}\"\n\
         [security]\nrecord = \"/nonexistent/lemnos-resources-test/record\"\n\
         [[devices]]\nid = \"imu\"\ndriver = \"bmi088\"\nbus = \"{bus}\"\n"
    )
}

#[test]
fn validate_resource_names_refuses_an_i2c_bus_the_soc_lacks_by_name() {
    let reason = validate(&board("rk3588", "i2c-10")).unwrap_err();
    assert_eq!(
        reason,
        "device \"imu\": i2c-10: the rk3588 SoC has no I2C bus i2c-10 (it has: i2c0, i2c1, i2c2, i2c3)"
    );
}

#[test]
fn validate_resource_names_accepts_a_bus_the_soc_has() {
    validate(&board("rk3588", "i2c-0")).expect("i2c-0 is on the RK3588");
}

#[test]
fn validate_resource_names_needs_the_soc_map_for_a_board_that_names_a_soc() {
    let text = board("no-such-soc", "i2c-0");
    let reason = validate(&text).unwrap_err();
    assert!(reason.contains("no-such-soc.toml"), "{reason}");
}
