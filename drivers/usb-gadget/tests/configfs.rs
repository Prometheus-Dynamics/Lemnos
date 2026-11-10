//! The driver over the Linux gadget backend, against a stand-in for configfs, a FunctionFS
//! mount and `/sys/class/udc` made of plain directories and files under the temporary
//! directory. Nothing here goes near the real ones.

#![cfg(target_os = "linux")]

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use lemnos_core::{
    Config, ConfigValue, Driver, Health, LocalCaller, Provider, Resources, Result, Status, Tag,
    UsbFunctionHandle,
};
use lemnos_driver_usb_gadget::{BACKOFF_FIRST_NS, NOT_THERE, POLL_NS, UsbGadget};
use lemnos_linux::FunctionFsGadget;

const UDC: &str = "fe980000.usb";

/// What a board's image leaves for the controller `UDC`, and the kernel's own entries.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("lemnos-usb-gadget-{}-{name}", std::process::id()));
        let tree = Self { root };
        fs::create_dir_all(tree.gadget().join(format!("functions/ffs.{UDC}"))).unwrap();
        fs::create_dir_all(tree.gadget().join("configs")).unwrap();
        fs::create_dir_all(tree.gadget().join("strings")).unwrap();
        fs::write(tree.gadget().join("UDC"), "").unwrap();
        fs::create_dir_all(tree.root.join("udc").join(UDC)).unwrap();
        fs::create_dir_all(tree.mount()).unwrap();
        tree
    }

    fn gadget(&self) -> PathBuf {
        self.root
            .join("config/usb_gadget")
            .join(format!("lemnos-{UDC}"))
    }

    fn mount(&self) -> PathBuf {
        self.root.join("ffs").join(UDC)
    }

    /// What mounting FunctionFS and its taking the descriptors would put in the mount point.
    fn mount_functionfs(&self) {
        for file in ["ep0", "ep81", "ep02"] {
            fs::write(self.mount().join(file), "").unwrap();
        }
    }

    fn attribute(&self, file: &str) -> String {
        fs::read_to_string(self.gadget().join(file)).unwrap()
    }

    fn entries(&self, dir: &str) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.gadget().join(dir))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Provider for Tree {
    fn provided(&self, _need: &str) -> bool {
        true
    }

    fn device(&mut self, _need: &str, _tag: Tag) -> Result<LocalCaller> {
        Err(Status::NOT_SUPPORTED)
    }

    fn usb_function(&mut self, _need: &str) -> Result<UsbFunctionHandle> {
        let gadget = FunctionFsGadget::new(
            &self.root.join("config"),
            &self.root.join("ffs"),
            &self.root.join("udc"),
            UDC,
        );
        Ok(Box::new(gadget.ok_or(Status::NOT_FOUND)?))
    }
}

fn bind(tree: &mut Tree) -> UsbGadget {
    let entries = [
        ("vendor", ConfigValue::Int(0x1209)),
        ("product", ConfigValue::Int(0x0001)),
        ("release", ConfigValue::Int(0x0100)),
        ("manufacturer", ConfigValue::Str("Maker")),
        ("product_name", ConfigValue::Str("Thing")),
        ("serial", ConfigValue::Str("0001")),
        ("max_power_ma", ConfigValue::Int(250)),
        (
            "interfaces",
            ConfigValue::Str("interface 0xff 0 0 data\nendpoint in bulk\nendpoint out bulk\n"),
        ),
    ];
    let mut resources = Resources::new(tree, None);
    UsbGadget::bind(&mut resources, &Config::new(&entries))
        .map_err(|_| ())
        .expect("bind")
}

#[test]
fn the_driver_builds_the_gadget_in_configfs_and_takes_it_down_when_it_goes() {
    let mut tree = Tree::new("up");
    tree.mount_functionfs();
    let mut driver = bind(&mut tree);
    assert!(
        tree.entries("configs").is_empty(),
        "binding touches nothing"
    );

    assert_eq!(driver.tick(0), Some(POLL_NS));
    assert_eq!(driver.health(), Health::Ok);
    let expected = [
        ("idVendor", "0x1209\n"),
        ("idProduct", "0x0001\n"),
        ("bcdDevice", "0x0100\n"),
        ("bcdUSB", "0x0200\n"),
        ("bDeviceClass", "0x00\n"),
        ("bMaxPacketSize0", "0x40\n"),
        ("strings/0x409/manufacturer", "Maker\n"),
        ("strings/0x409/product", "Thing\n"),
        ("strings/0x409/serialnumber", "0001\n"),
        ("configs/c.1/MaxPower", "250\n"),
        ("configs/c.1/bmAttributes", "0x80\n"),
        ("UDC", "fe980000.usb\n"),
    ];
    for (file, value) in expected {
        assert_eq!(tree.attribute(file), value, "{file}");
    }
    assert_eq!(
        fs::read_link(tree.gadget().join(format!("configs/c.1/ffs.{UDC}"))).unwrap(),
        tree.gadget().join(format!("functions/ffs.{UDC}"))
    );
    // FunctionFS got the interface at both speeds and the four strings.
    let ep0 = fs::read(tree.mount().join("ep0")).unwrap();
    assert_eq!(ep0[..4], [3, 0, 0, 0]);
    assert_eq!(ep0[8..20], [83, 0, 0, 0, 3, 0, 0, 0, 3, 0, 0, 0]);
    assert!(ep0.ends_with(b"Maker\0Thing\x000001\0data\0"));

    // The host configures the device and asks something of it: the driver sees both.
    let mut kernel = fs::OpenOptions::new()
        .append(true)
        .open(tree.mount().join("ep0"))
        .unwrap();
    kernel
        .write_all(&[0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0])
        .unwrap();
    assert_eq!(driver.tick(POLL_NS), Some(2 * POLL_NS));
    assert!(driver.configured());
    assert_eq!(driver.health(), Health::Ok);

    drop(driver);
    assert!(tree.entries("configs").is_empty());
    assert!(tree.entries("strings").is_empty());
    assert_eq!(tree.attribute("UDC"), "\n", "unbound");
    assert!(tree.gadget().join(format!("functions/ffs.{UDC}")).is_dir());
}

#[test]
fn a_board_without_the_mount_is_retried_and_comes_up_once_it_is_there() {
    let mut tree = Tree::new("late");
    let mut driver = bind(&mut tree);
    assert_eq!(driver.tick(0), Some(BACKOFF_FIRST_NS));
    assert_eq!(driver.health(), Health::Failed(NOT_THERE));
    assert!(tree.entries("configs").is_empty(), "nothing half built");
    assert!(!tree.gadget().join("idVendor").exists());
    assert_eq!(tree.attribute("UDC"), "");

    assert_eq!(driver.tick(BACKOFF_FIRST_NS), Some(3 * BACKOFF_FIRST_NS));
    tree.mount_functionfs();
    assert_eq!(
        driver.tick(3 * BACKOFF_FIRST_NS),
        Some(3 * BACKOFF_FIRST_NS + POLL_NS)
    );
    assert_eq!(driver.health(), Health::Ok);
    assert_eq!(tree.attribute("UDC"), "fe980000.usb\n");
}
