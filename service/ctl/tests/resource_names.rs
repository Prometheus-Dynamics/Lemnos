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

/// A board with no `soc`: its names are checked as forms only, never against a SoC map.
fn plain(bus: &str) -> String {
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [security]\nrecord = \"/nonexistent/lemnos-resources-test/record\"\n\
         [[devices]]\nid = \"imu\"\ndriver = \"bmi088\"\nbus = \"{bus}\"\n"
    )
}

#[test]
fn validate_resource_names_refuses_an_unknown_form_on_a_board_with_no_soc_by_name() {
    let reason = validate(&plain("bogus:thing")).unwrap_err();
    assert!(reason.contains("\"bogus:thing\""), "{reason}");
    assert!(
        reason.contains("i2c-N"),
        "the known forms are listed: {reason}"
    );
}

#[test]
fn validate_resource_names_refuses_a_misspelt_prefix_on_a_soc_board_by_name() {
    let reason = validate(&board("rk3588", "i2cc-0")).unwrap_err();
    assert!(reason.contains("\"i2cc-0\""), "{reason}");
}

#[test]
fn validate_resource_names_checks_i2c_of_against_the_soc_nodes() {
    let node = "/axi/pcie@1000120000/rp1/i2c@74000";
    validate(&board("bcm2712-rp1", &format!("i2c:of={node}"))).expect("the RP1 has that node");
    let reason = validate(&board("rk3588", &format!("i2c:of={node}"))).unwrap_err();
    assert!(
        reason.contains(&format!("no I2C bus at {node}")),
        "{reason}"
    );
}

#[test]
fn validate_resource_names_checks_i2c_of_as_a_form_when_there_is_no_soc() {
    validate(&plain("i2c:of=/axi/pcie@1000120000/rp1/i2c@74000")).expect("a form, well formed");
    let reason = validate(&plain("i2c:of=axi/i2c")).unwrap_err();
    assert!(reason.contains("i2c:of="), "{reason}");
}

#[test]
fn validate_resource_names_checks_pio_names_against_the_soc_having_a_pio_block() {
    validate(&board("bcm2712-rp1", "pio-i2c:sda=8,scl=7")).expect("the RP1 has a PIO block");
    let reason = validate(&board("rk3588", "pio-i2c:sda=8,scl=7")).unwrap_err();
    assert!(reason.contains("PIO"), "{reason}");
}

#[test]
fn validate_resource_names_checks_the_grammar_of_bind_only_forms_without_a_soc_map() {
    validate(&board("rk3588", "gpiochip0:17")).expect("a numbered line is a form");
    validate(&plain("pwmchip0:1")).expect("a numbered channel is a form");
    let reason = validate(&plain("gpiochip0:x")).unwrap_err();
    assert!(reason.contains("\"gpiochip0:x\""), "{reason}");
}
