//! `tree --bridge-env`: the bridge's link setting from the board file's gadget devices.

use super::bridge_env::{HEADER, VARIABLE, text};

fn board(devices: &str) -> lemnos_host::board::BoardFile {
    let text =
        format!("format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n{devices}");
    lemnos_host::board::parse(&text).expect("board")
}

fn line_of(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| line.starts_with(VARIABLE))
        .collect()
}

#[test]
fn a_gadget_with_the_default_composition_offers_orion_link() {
    let board = board(
        "[[devices]]\nid = \"gadget\"\ndriver = \"usb-gadget\"\nuses = { function = \"udc:u\" }\n",
    );
    let written = text(&board).unwrap();
    assert!(written.starts_with(HEADER));
    assert_eq!(line_of(&written), ["LEMNOS_ORION_LINKS=gadget=orion-link"]);
}

#[test]
fn a_gadget_with_its_own_interfaces_offers_the_name_on_its_first_interface_line() {
    let board = board(
        "[[devices]]\nid = \"usb0\"\ndriver = \"usb-gadget\"\nuses = { function = \"udc:u\" }\n\
         config = { interfaces = \"function ffs\\ninterface 0xff 0 0 pipe\\nendpoint in bulk\\nendpoint out bulk\\nfunction acm\\n\" }\n",
    );
    assert_eq!(
        line_of(&text(&board).unwrap()),
        ["LEMNOS_ORION_LINKS=usb0=pipe"]
    );
}

#[test]
fn the_name_is_read_from_an_interfaces_file() {
    let dir = std::env::temp_dir().join(format!("lemnos-bridge-env-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("interfaces.txt");
    std::fs::write(&file, "interface 0xff 0 0 from-file\nendpoint in bulk\n").unwrap();
    let board = board(&format!(
        "[[devices]]\nid = \"g\"\ndriver = \"usb-gadget\"\nuses = {{ function = \"udc:u\" }}\n\
         config = {{ interfaces_file = \"{}\" }}\n",
        file.display()
    ));
    assert_eq!(
        line_of(&text(&board).unwrap()),
        ["LEMNOS_ORION_LINKS=g=from-file"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_board_with_no_gadget_writes_a_comment_and_no_setting() {
    let board = board("[[devices]]\nid = \"x\"\ndriver = \"fake-counter\"\n");
    let written = text(&board).unwrap();
    assert!(written.starts_with(HEADER));
    assert!(line_of(&written).is_empty());
    assert!(written.lines().all(|line| line.starts_with('#')));
    assert!(written.contains("no usb-gadget device"));
}

#[test]
fn a_disabled_gadget_is_not_offered() {
    let board = board(
        "[[devices]]\nid = \"g\"\ndriver = \"usb-gadget\"\ndisabled = true\nuses = { function = \"udc:u\" }\n",
    );
    assert!(line_of(&text(&board).unwrap()).is_empty());
}

#[test]
fn two_gadgets_are_two_entries_of_one_line() {
    let board = board(
        "[[devices]]\nid = \"a\"\ndriver = \"usb-gadget\"\nuses = { function = \"udc:u\" }\n\
         [[devices]]\nid = \"b\"\ndriver = \"usb-gadget\"\nuses = { function = \"udc:v\" }\n\
         config = { interfaces = \"interface 0xff 0 0 second\\nendpoint in bulk\\n\" }\n",
    );
    assert_eq!(
        line_of(&text(&board).unwrap()),
        ["LEMNOS_ORION_LINKS=a=orion-link,b=second"]
    );
}

#[test]
fn a_first_interface_without_a_name_or_not_the_link_is_refused_by_device() {
    let unnamed = board(
        "[[devices]]\nid = \"g\"\ndriver = \"usb-gadget\"\nuses = { function = \"udc:u\" }\n\
         config = { interfaces = \"interface 0xff 0 0\\nendpoint in bulk\\n\" }\n",
    );
    let error = text(&unnamed).unwrap_err();
    assert!(
        error.contains("\"g\"") && error.contains("no name"),
        "{error}"
    );
    let moved = board(
        "[[devices]]\nid = \"g\"\ndriver = \"usb-gadget\"\nuses = { function = \"udc:u\" }\n\
         config = { interfaces = \"interface 0x02 2 1 acm\\ninterface 0xff 0 0 link\\n\" }\n",
    );
    let error = text(&moved).unwrap_err();
    assert!(error.contains("not the vendor-class link"), "{error}");
}
