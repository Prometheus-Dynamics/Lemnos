//! A gadget's `interfaces_file` is read by the program that runs the gadget: the daemon. The
//! daemon is built with std (service/shipped-features), so the file is read like the same board's
//! `interfaces` text.

mod common;

use common::start_board;
use lemnos_client::Client;
use lemnos_contract_usb_function::UsbFunction;
use lemnos_driver_usb_gadget::UsbGadget;
use lemnos_host::entry;
use lemnos_sim::SimBoard;

const INTERFACES: &str = "interface 0xff 0 0 pipe\nendpoint in bulk\nendpoint out bulk\n";

/// The gadget is bound when the program reads the file its board names; a build without a
/// filesystem refuses the file, the driver never binds, and the device is not in the list.
#[test]
fn a_program_reads_the_gadget_interfaces_file() {
    let path = std::env::temp_dir().join(format!("lemnosd-interfaces-{}", std::process::id()));
    std::fs::write(&path, INTERFACES).unwrap();
    let devices = format!(
        "[[devices]]\nid = \"gadget\"\ndriver = \"usb-gadget\"\nuses = {{ function = \"udc0\" }}\n\
         config = {{ vendor = 0x1209, product = 1, interfaces_file = \"{}\" }}\n\
         [devices.streams.data]\nbuffers = 8\nbuffer_bytes = 4096\njob_slots = 16\njob_bytes = 22\n",
        path.display()
    );
    let sim = SimBoard::new();
    let _usb = sim.add_usb_function("udc0");
    let running = start_board(&devices, vec![entry::<UsbGadget>()], sim, None);
    let client = Client::connect(&running.socket, "function").unwrap();
    // A device whose driver refused its configuration is still listed; the function only opens and
    // names its data stream once the gadget is bound and serving.
    let device = client.open::<dyn UsbFunction>("gadget");
    let mut name = [0u8; 32];
    let served = device.map(|mut device| device.data_stream(&mut name));
    let _ = std::fs::remove_file(&path);
    let len = served
        .expect("the gadget is open")
        .expect("the gadget is serving");
    assert!(len > 0);
}
