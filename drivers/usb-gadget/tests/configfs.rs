//! The driver over the Linux gadget backend, against a stand-in for configfs, a FunctionFS
//! mount and `/sys/class/udc` made of plain directories and files under the temporary
//! directory. The tree is made by the privileged setup (`lemnos_linux::prepare_gadget`) with a
//! fake mount, as root would make it; the driver then attaches to it. Nothing here goes near
//! the real ones.

#![cfg(target_os = "linux")]

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use lemnos_core::{
    Config, ConfigValue, Driver, Health, LocalCaller, Provider, Resources, Result, Status, Tag,
    UsbFunctionHandle,
};
use lemnos_driver_usb_gadget::{BACKOFF_FIRST_NS, NOT_THERE, POLL_NS, UsbGadget, settings};
use lemnos_linux::{FunctionFsGadget, Mounts, Owner, Roots, prepare_gadget};

const UDC: &str = "fe980000.usb";

/// The mounts of a stand-in: a FunctionFS mount is the three files the kernel makes in it.
struct FakeMounts;

impl Mounts for FakeMounts {
    fn functionfs(&mut self, target: &Path, _options: &str) -> io::Result<()> {
        for file in ["ep0", "ep81", "ep02"] {
            fs::write(target.join(file), "")?;
        }
        Ok(())
    }

    fn unmount(&mut self, target: &Path) -> io::Result<()> {
        // A stand-in mount point's files go with it.
        let _ = fs::remove_file(target.join("ep0"));
        Ok(())
    }
}

/// The board file's settings, as the driver and the setup both read them.
fn entries() -> Vec<(&'static str, ConfigValue<'static>)> {
    vec![
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
    ]
}

/// What the board's setup leaves for the controller `UDC`: the configfs tree, the mount, and
/// the kernel's own entries.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("lemnos-usb-gadget-{}-{name}", std::process::id()));
        let tree = Self { root };
        fs::create_dir_all(tree.root.join("udc").join(UDC)).unwrap();
        fs::create_dir_all(tree.root.join("config/usb_gadget")).unwrap();
        fs::create_dir_all(tree.root.join("ffs")).unwrap();
        // The gadget directory and its `max_speed`, as a kernel with the attribute has them.
        fs::create_dir_all(tree.gadget().join("functions")).unwrap();
        let owner = Owner {
            uid: fs::metadata(&tree.root).unwrap().uid(),
            gid: fs::metadata(&tree.root).unwrap().gid(),
        };
        let (set, functions) = settings(&Config::new(&entries())).unwrap();
        let roots = Roots {
            configfs: &tree.root.join("config"),
            mounts: &tree.root.join("ffs"),
            udc_class: &tree.root.join("udc"),
        };
        fs::write(tree.gadget().join("max_speed"), "").unwrap();
        fs::write(tree.gadget().join("UDC"), "").unwrap();
        prepare_gadget(
            &roots,
            UDC,
            owner,
            set.bytes(),
            functions.as_slice(),
            &mut FakeMounts,
        )
        .unwrap();
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

    /// The FunctionFS files, as a mount made them: removed to stand for a mount that is gone.
    fn unmount_functionfs(&self) {
        let _ = fs::remove_file(self.mount().join("ep0"));
    }

    fn mount_functionfs(&self) {
        FakeMounts.functionfs(&self.mount(), "").unwrap();
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
    fn provided(&self, need: &str) -> bool {
        need == "function"
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
        Ok(lemnos_core::Held::boxed(Box::new(
            gadget.ok_or(Status::NOT_FOUND)?,
        )))
    }
}

fn bind(tree: &mut Tree) -> UsbGadget {
    let entries = entries();
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
    assert_eq!(tree.entries("configs"), ["c.1"], "binding touches nothing");
    assert_eq!(tree.attribute("UDC"), "\n", "the setup left it unbound");

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
    assert_eq!(
        tree.entries("configs"),
        ["c.1"],
        "the setup's configuration stays"
    );
    assert_eq!(tree.attribute("UDC"), "\n", "unbound");
    assert!(tree.gadget().join(format!("functions/ffs.{UDC}")).is_dir());
}

#[test]
fn a_board_without_the_mount_is_retried_and_comes_up_once_it_is_there() {
    let mut tree = Tree::new("late");
    tree.unmount_functionfs();
    let mut driver = bind(&mut tree);
    assert_eq!(driver.tick(0), Some(BACKOFF_FIRST_NS));
    assert_eq!(driver.health(), Health::Failed(NOT_THERE));
    assert_eq!(tree.entries("configs"), ["c.1"], "nothing more is built");
    assert_eq!(tree.attribute("UDC"), "\n", "not bound");

    assert_eq!(driver.tick(BACKOFF_FIRST_NS), Some(3 * BACKOFF_FIRST_NS));
    tree.mount_functionfs();
    assert_eq!(
        driver.tick(3 * BACKOFF_FIRST_NS),
        Some(3 * BACKOFF_FIRST_NS + POLL_NS)
    );
    assert_eq!(driver.health(), Health::Ok);
    assert_eq!(tree.attribute("UDC"), "fe980000.usb\n");
}
