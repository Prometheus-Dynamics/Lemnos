//! The late grants (HOTPLUG-GRANT): the attributes the daemon writes on a board's own fans and PWM channels, granted
//! on the hotplug event, generated from the board file. The fixture board's fragments are golden; a device the board
//! does not name, and a host-owned fan, get no grant.

use lemnos_devtree::{mdev, udev};
use lemnos_host::board::{self, BoardFile};

fn golden(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn fixture() -> BoardFile {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/late-grants.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    board::parse(&text).unwrap_or_else(|e| panic!("{e}"))
}

/// A board with `devices` (a TOML `[[devices]]` block list) and the reference board's SoC and controller.
fn board_with(devices: &str) -> BoardFile {
    board::parse(&format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\nsoc = \"bcm2712-rp1\"\n\
         [[hardware]]\ncontroller = \"pwm1\"\npins = \"gpio45\"\n{devices}"
    ))
    .unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn the_fixture_board_gives_its_fragments_as_golden() {
    let board = fixture();
    assert_eq!(udev::fragment(&board), golden("late-grants-udev.rules"));
    assert_eq!(mdev::fragment(&board), golden("late-grants-mdev.conf"));
}

#[test]
fn a_fan_the_board_names_gets_one_rule_per_attribute_on_its_add() {
    let text = udev::fragment(&fixture());
    assert!(text.contains(
        "SUBSYSTEM==\"hwmon\", ACTION==\"add\", ATTR{name}==\"pwmfan\", TEST==\"pwm1\", RUN+=\"/bin/chgrp lemnos $sys$devpath/pwm1\", RUN+=\"/bin/chmod 0660 $sys$devpath/pwm1\""
    ));
    assert!(text.contains("ATTR{name}==\"pwmfan\", TEST==\"pwm1_enable\""));
    assert!(text.contains("KERNEL==\"hwmon7\", TEST==\"pwm1\""));
}

#[test]
fn a_host_owned_fan_and_an_unnamed_fan_get_no_grant() {
    let text = format!(
        "{}{}",
        udev::fragment(&fixture()),
        mdev::fragment(&fixture())
    );
    assert!(!text.contains("hostfan"), "{text}");
    // Nothing the board does not name: no other fan name, no other hwmon directory, no other controller. (The mdev
    // channel line matches any `pwmchipN` and selects the chip by its controller's node, so `pwmchip` is not absent.)
    for absent in ["hwmon3", "hwmon0", "pwmfan2", "/axi/other"] {
        assert!(!text.contains(absent), "{absent} granted: {text}");
    }
}

#[test]
fn a_channel_is_granted_on_its_chip_change_for_its_export_and_its_controller_only() {
    let text = udev::fragment(&fixture());
    let rule = text
        .lines()
        .find(|l| l.contains("ENV{EXPORT}==\"pwm1\""))
        .expect("channel 1 rule");
    assert!(rule.contains("ACTION==\"change\""));
    assert!(rule.contains("/sys/firmware/devicetree/base/axi/pcie@1000120000/rp1/pwm@9c000"));
    assert!(rule.contains("$sys$devpath/$env{EXPORT}/period"));
    // export and unexport are never granted, and nothing exports.
    assert!(!text.contains("export\""), "{text}");
    assert!(!text.contains("/export"), "{text}");
    let mdev = mdev::fragment(&fixture());
    assert!(
        mdev.contains("-SUBSYSTEM=^pwm$;EXPORT=^pwm1$;^pwmchip[0-9]+$ root:root 0660 */bin/sh -c")
    );
    assert!(!mdev.contains("/export"), "{mdev}");
}

#[test]
fn a_board_with_no_fan_or_channel_gets_no_late_lines() {
    let board = board_with("");
    assert!(!udev::fragment(&board).contains("Late grants"));
    assert!(!mdev::fragment(&board).contains("Late grants"));
}

#[test]
fn a_channel_named_by_its_chip_name_gets_no_grant_and_the_generator_refuses_it() {
    // `pwmchipN:CH` is not a name the board file's generator accepts (probe order): the board refuses it.
    let board = board_with(
        "[[devices]]\nid = \"f\"\ndriver = \"pwm-channel-fan\"\nuses = { pwm = \"pwmchip0:1\" }\nconfig = { frequency_hz = 25000, min_duty = 200, kick_ms = 500 }\n",
    );
    assert!(!udev::fragment(&board).contains("ENV{EXPORT}"));
}
