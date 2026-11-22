//! `reboot_to`: the board's way back, asked of the firmware, then the restart. Each refusal is checked to ask
//! the firmware nothing and the restart helper for no restart; the firmware's refusal restarts nothing.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use lemnos_contract_system_reboot::Reboot;
use lemnos_core::{CallerId, Status};
use lemnos_restart::stand_in::{Manager, StandIn};
use lemnos_slot_store::security;

use crate::SystemReboot;
use crate::boot::test_restart;

/// The firmware's side: how many times it was asked, and whether it takes the request.
#[derive(Clone)]
struct Firmware {
    asked: Arc<AtomicU32>,
    takes: bool,
}

impl Firmware {
    fn new(takes: bool) -> Self {
        Self {
            asked: Arc::new(AtomicU32::new(0)),
            takes,
        }
    }

    fn arm(&self) -> crate::Arm {
        let me = self.clone();
        Box::new(move || {
            me.asked.fetch_add(1, Ordering::SeqCst);
            if me.takes {
                Ok(())
            } else {
                Err(std::io::Error::other(
                    "the firmware did not take the reboot order: answer word 0x80000001",
                ))
            }
        })
    }

    fn asked(&self) -> u32 {
        self.asked.load(Ordering::SeqCst)
    }
}

/// A reboot device for `way_back` and `firmware`, on a helper whose manager answers `manager`, with its data dir.
fn device(
    way_back: Option<&str>,
    firmware: Option<&Firmware>,
    manager: Manager,
) -> (SystemReboot, StandIn, Scratch) {
    let (restarter, stand) = test_restart::helper(manager);
    let data = Scratch::new();
    let device = SystemReboot {
        restarter,
        data: data.0.clone(),
        calling: CallerId(7),
        way_back: way_back.map(str::to_string),
        arm: firmware.map(Firmware::arm),
    };
    (device, stand, data)
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir = lemnos_socket_dir::scratch_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn requested(data: &Scratch) -> String {
    crate::boot::read_request(&data.0.join("board/slots"))
}

/// Serialises the tests that set the thread's switch view, so none sees another's.
static SWITCH: Mutex<()> = Mutex::new(());

fn off<T>(f: impl FnOnce() -> T) -> T {
    let _one = SWITCH.lock().unwrap_or_else(|p| p.into_inner());
    security::with_enforced(false, f)
}

#[test]
fn the_way_back_asks_the_firmware_once_then_restarts_through_the_helper() {
    let firmware = Firmware::new(true);
    let (mut device, stand, data) = device(Some("rpiboot"), Some(&firmware), Manager::Stops);
    assert_eq!(off(|| device.reboot_to("rpiboot")), Ok(()));
    assert_eq!(firmware.asked(), 1, "the firmware is asked once");
    assert_eq!(
        stand.asked(),
        ["check", "check", "start"],
        "the helper's readiness, then the reboot (its own check, then its start)"
    );
    assert_eq!(requested(&data), "by:7", "the boot history names who asked");
}

#[test]
fn a_board_without_a_way_back_refuses_and_asks_nothing() {
    let firmware = Firmware::new(true);
    let (mut device, stand, data) = device(None, Some(&firmware), Manager::Stops);
    assert_eq!(
        off(|| device.reboot_to("rpiboot")),
        Err(Status::NOT_SUPPORTED)
    );
    assert_eq!(firmware.asked(), 0);
    assert!(stand.asked().is_empty(), "{:?}", stand.asked());
    assert_eq!(requested(&data), "-");
}

#[test]
fn a_target_that_is_not_the_declared_one_is_refused_and_asks_nothing() {
    let firmware = Firmware::new(true);
    let (mut device, stand, _data) = device(Some("rpiboot"), Some(&firmware), Manager::Stops);
    for target in ["", "usb", "RPIBOOT", "rpiboot "] {
        assert_eq!(
            off(|| device.reboot_to(target)),
            Err(Status::INVALID_ARGUMENT),
            "{target:?}"
        );
    }
    assert_eq!(firmware.asked(), 0);
    assert!(stand.asked().is_empty(), "{:?}", stand.asked());
}

#[test]
fn a_board_whose_record_has_no_firmware_refuses_and_restarts_nothing() {
    let (mut device, stand, _data) = device(Some("rpiboot"), None, Manager::Stops);
    assert_eq!(
        off(|| device.reboot_to("rpiboot")),
        Err(Status::NOT_SUPPORTED)
    );
    assert!(stand.asked().is_empty(), "{:?}", stand.asked());
}

#[test]
fn the_security_switch_on_refuses_before_the_firmware_or_the_helper_is_asked() {
    let firmware = Firmware::new(true);
    let (mut device, stand, data) = device(Some("rpiboot"), Some(&firmware), Manager::Stops);
    let refused = {
        let _one = SWITCH.lock().unwrap_or_else(|p| p.into_inner());
        security::with_enforced(true, || device.reboot_to("rpiboot"))
    };
    assert_eq!(refused, Err(Status::NOT_PERMITTED));
    assert_eq!(firmware.asked(), 0);
    assert!(stand.asked().is_empty(), "{:?}", stand.asked());
    assert_eq!(requested(&data), "-");
}

#[test]
fn a_firmware_that_does_not_take_the_order_restarts_nothing() {
    let firmware = Firmware::new(false);
    let (mut device, stand, data) = device(Some("rpiboot"), Some(&firmware), Manager::Stops);
    assert_eq!(off(|| device.reboot_to("rpiboot")), Err(Status::IO));
    assert_eq!(firmware.asked(), 1);
    assert_eq!(
        stand.asked(),
        ["check"],
        "only the readiness check: no reboot"
    );
    assert_eq!(requested(&data), "-");
}

#[test]
fn a_helper_that_would_refuse_the_restart_leaves_the_firmware_unasked() {
    let firmware = Firmware::new(true);
    let (mut device, stand, _data) = device(
        Some("rpiboot"),
        Some(&firmware),
        Manager::Refuses("reboot.target is masked".into()),
    );
    assert_eq!(off(|| device.reboot_to("rpiboot")), Err(Status::IO));
    assert_eq!(
        firmware.asked(),
        0,
        "the firmware's order is not set for a restart that won't come"
    );
    assert_eq!(stand.asked(), ["check"]);
}

#[test]
fn the_plain_reboot_never_asks_the_firmware() {
    let firmware = Firmware::new(true);
    let (mut device, _stand, _data) = device(Some("rpiboot"), Some(&firmware), Manager::Stops);
    assert_eq!(off(|| device.reboot()), Ok(()));
    assert_eq!(firmware.asked(), 0);
}
