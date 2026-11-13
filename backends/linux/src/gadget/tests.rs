//! The gadget against a stand-in tree of plain directories and files under the temporary
//! directory, and the session against a scripted wire. Nothing here goes near the real
//! configfs, a controller or a FunctionFS mount.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};

use lemnos_core::{Provider, Status};
use lemnos_measure::test_alloc::allocations;

use super::*;
use crate::LinuxBoard;

const UDC: &str = "udc0";
const WAIT: Duration = Duration::from_millis(200);

fn string(text: &str) -> Vec<u8> {
    let mut bytes = vec![0, 3];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes[0] = bytes.len() as u8;
    bytes
}

/// The function's descriptors at one speed: a vendor interface named by string 4 with a bulk
/// endpoint each way.
fn function(packet: u16) -> Vec<u8> {
    let [low, high] = packet.to_le_bytes();
    let mut bytes = vec![9, 4, 0, 0, 2, 0xff, 0, 0, 4];
    bytes.extend_from_slice(&[7, 5, 0x81, 2, low, high, 0]);
    bytes.extend_from_slice(&[7, 5, 0x02, 2, low, high, 0]);
    bytes
}

fn configuration(body: &[u8]) -> Vec<u8> {
    let total = (9 + body.len()) as u16;
    let mut bytes = vec![9, 2, 0, 0, 1, 1, 5, 0xc0, 125];
    bytes[2..4].copy_from_slice(&total.to_le_bytes());
    bytes.extend_from_slice(body);
    bytes
}

const DEVICE: [u8; 18] = [
    18, 1, 0x00, 0x02, 0xef, 2, 1, 64, 0x09, 0x12, 0x34, 0x12, 0x02, 0x01, 1, 2, 3, 1,
];

fn table() -> Vec<u8> {
    let mut bytes = vec![4, 3, 0x09, 0x04];
    for text in ["Maker", "Thing", "0001", "data", "main"] {
        bytes.extend(string(text));
    }
    bytes
}

/// Device, both speeds and five strings.
fn set() -> Vec<u8> {
    [
        &DEVICE[..],
        &configuration(&function(64)),
        &configuration(&function(512)),
        &table(),
    ]
    .concat()
}

/// A stand-in for configfs, a FunctionFS mount and `/sys/class/udc`, as the image leaves them.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "lemnos-gadget-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let tree = Self { root };
        fs::create_dir_all(tree.gadget_dir().join("functions/ffs.udc0")).unwrap();
        fs::create_dir_all(tree.gadget_dir().join("configs")).unwrap();
        fs::create_dir_all(tree.gadget_dir().join("strings")).unwrap();
        fs::write(tree.gadget_dir().join("UDC"), "").unwrap();
        fs::write(tree.gadget_dir().join("max_speed"), "").unwrap();
        fs::create_dir_all(tree.mount()).unwrap();
        for file in ["ep0", "ep81", "ep02"] {
            fs::write(tree.mount().join(file), "").unwrap();
        }
        fs::create_dir_all(tree.root.join("udc").join(UDC)).unwrap();
        tree
    }

    fn gadget_dir(&self) -> PathBuf {
        self.root.join("config/usb_gadget/lemnos-udc0")
    }

    fn mount(&self) -> PathBuf {
        self.root.join("ffs").join(UDC)
    }

    fn gadget(&self) -> FunctionFsGadget {
        FunctionFsGadget::new(
            &self.root.join("config"),
            &self.root.join("ffs"),
            &self.root.join("udc"),
            UDC,
        )
        .unwrap()
    }

    /// An attribute of the gadget, by its path inside the gadget directory.
    fn attribute(&self, file: &str) -> String {
        fs::read_to_string(self.gadget_dir().join(file)).unwrap()
    }

    /// The names in a directory of the gadget, sorted.
    fn entries(&self, dir: &str) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.gadget_dir().join(dir))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// Whether the gadget is as the image left it: unbound, no configuration, no strings.
    fn is_bare(&self) -> bool {
        self.entries("configs").is_empty()
            && self.entries("strings").is_empty()
            && self.attribute("UDC").trim().is_empty()
            && self.gadget_dir().join("functions/ffs.udc0").is_dir()
    }

    /// The kernel's side of `ep0`: what the function reads next.
    fn kernel_says(&self, bytes: &[u8]) {
        let mut ep0 = OpenOptions::new()
            .append(true)
            .open(self.mount().join("ep0"))
            .unwrap();
        ep0.write_all(bytes).unwrap();
    }

    fn event(&self, kind: u8) {
        self.kernel_says(&[0, 0, 0, 0, 0, 0, 0, 0, kind, 0, 0, 0]);
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const BIND: u8 = 0;
const UNBIND: u8 = 1;
const ENABLE: u8 = 2;

#[test]
fn enable_writes_the_identity_the_configuration_and_the_function_then_binds() {
    let tree = Tree::new();
    let mut gadget = tree.gadget();
    gadget.enable(&set()).unwrap();

    let expected = [
        ("idVendor", "0x1209\n"),
        ("idProduct", "0x1234\n"),
        ("bcdDevice", "0x0102\n"),
        ("bcdUSB", "0x0200\n"),
        ("bDeviceClass", "0xef\n"),
        ("bDeviceSubClass", "0x02\n"),
        ("bDeviceProtocol", "0x01\n"),
        ("bMaxPacketSize0", "0x40\n"),
        ("max_speed", "high-speed\n"),
        ("strings/0x409/manufacturer", "Maker\n"),
        ("strings/0x409/product", "Thing\n"),
        ("strings/0x409/serialnumber", "0001\n"),
        ("configs/c.1/MaxPower", "250\n"),
        ("configs/c.1/bmAttributes", "0xc0\n"),
        ("configs/c.1/strings/0x409/configuration", "main\n"),
        ("UDC", "udc0\n"),
    ];
    for (file, value) in expected {
        assert_eq!(tree.attribute(file), value, "{file}");
    }
    assert_eq!(
        fs::read_link(tree.gadget_dir().join("configs/c.1/ffs.udc0")).unwrap(),
        tree.gadget_dir().join("functions/ffs.udc0")
    );

    // `ep0` got the descriptors of both speeds without their configuration headers, then
    // every string of the table.
    let mut blob = Vec::new();
    blob.extend(3u32.to_le_bytes());
    blob.extend(66u32.to_le_bytes());
    blob.extend(83u32.to_le_bytes()); // FS | HS | VIRTUAL_ADDR | ALL_CTRL_RECIP
    blob.extend(3u32.to_le_bytes());
    blob.extend(3u32.to_le_bytes());
    blob.extend(function(64));
    blob.extend(function(512));
    blob.extend(2u32.to_le_bytes());
    blob.extend(45u32.to_le_bytes());
    blob.extend(5u32.to_le_bytes());
    blob.extend(1u32.to_le_bytes());
    blob.extend(0x0409u16.to_le_bytes());
    blob.extend(b"Maker\0Thing\x000001\0data\0main\0");
    assert_eq!(fs::read(tree.mount().join("ep0")).unwrap(), blob);

    assert_eq!(gadget.enable(&set()), Err(ErrorKind::Busy));
}

#[test]
fn disable_and_drop_take_the_gadget_apart_and_leave_what_the_image_made() {
    let tree = Tree::new();
    let mut gadget = tree.gadget();
    assert_eq!(gadget.disable(), Ok(()), "never enabled");
    assert_eq!(tree.attribute("UDC"), "", "and nothing was touched");
    gadget.enable(&set()).unwrap();
    assert!(!tree.is_bare());
    gadget.disable().unwrap();
    assert!(tree.is_bare());
    assert_eq!(tree.attribute("UDC"), "\n", "unbound");
    assert_eq!(gadget.next_event(), Err(ErrorKind::Unavailable));
    assert_eq!(gadget.disable(), Ok(()));

    gadget.enable(&set()).unwrap();
    assert_eq!(tree.entries("configs"), ["c.1"]);
    drop(gadget);
    assert!(tree.is_bare());
}

#[test]
fn a_missing_piece_is_not_found_and_nothing_is_made() {
    type Remove = fn(&Tree);
    let missing: [(&str, Remove); 5] = [
        ("the gadget directory", |tree| {
            fs::remove_dir_all(tree.gadget_dir()).unwrap();
        }),
        ("the function directory", |tree| {
            fs::remove_dir_all(tree.gadget_dir().join("functions/ffs.udc0")).unwrap();
        }),
        ("the mount", |tree| {
            fs::remove_file(tree.mount().join("ep0")).unwrap();
        }),
        ("the mount point", |tree| {
            fs::remove_dir_all(tree.mount()).unwrap();
        }),
        ("the controller", |tree| {
            fs::remove_dir_all(tree.root.join("udc").join(UDC)).unwrap();
        }),
    ];
    for (what, remove) in missing {
        let tree = Tree::new();
        remove(&tree);
        let had_ep0 = tree.mount().join("ep0").exists();
        let mut gadget = tree.gadget();
        assert_eq!(gadget.enable(&set()), Err(ErrorKind::NotFound), "{what}");
        if tree.gadget_dir().exists() {
            assert!(tree.entries("configs").is_empty(), "{what}");
            assert!(tree.entries("strings").is_empty(), "{what}");
            assert!(!tree.gadget_dir().join("idVendor").exists(), "{what}");
            assert_eq!(tree.attribute("UDC"), "", "{what}: not even an unbind");
        } else {
            assert!(
                !tree
                    .root
                    .join("config/usb_gadget")
                    .join("lemnos-udc0")
                    .exists()
            );
        }
        assert_eq!(
            tree.mount().join("ep0").exists(),
            had_ep0,
            "{what}: ep0 is never made"
        );
        if had_ep0 {
            assert_eq!(fs::read(tree.mount().join("ep0")).unwrap(), b"", "{what}");
        }
        assert_eq!(gadget.next_event(), Err(ErrorKind::Unavailable), "{what}");
    }
}

#[test]
fn a_failure_part_way_undoes_the_steps_before_it() {
    let tree = Tree::new();
    // The second endpoint file is not there: everything up to it has been done by then.
    fs::remove_file(tree.mount().join("ep02")).unwrap();
    let mut gadget = tree.gadget();
    assert_eq!(gadget.enable(&set()), Err(ErrorKind::NotFound));
    assert!(tree.is_bare(), "no half-built gadget");
    assert_eq!(gadget.next_event(), Err(ErrorKind::Unavailable));

    // Once the piece is there the same call goes through.
    fs::write(tree.mount().join("ep02"), "").unwrap();
    gadget.enable(&set()).unwrap();
    assert_eq!(tree.attribute("UDC"), "udc0\n");
}

#[test]
fn what_an_earlier_run_left_is_removed_before_the_gadget_is_made() {
    let tree = Tree::new();
    let dir = tree.gadget_dir();
    fs::create_dir_all(dir.join("configs/c.7/strings/0x407")).unwrap();
    fs::write(dir.join("configs/c.7/MaxPower"), "2\n").unwrap();
    fs::write(dir.join("configs/c.7/strings/0x407/configuration"), "alt\n").unwrap();
    std::os::unix::fs::symlink(
        dir.join("functions/ffs.udc0"),
        dir.join("configs/c.7/ffs.udc0"),
    )
    .unwrap();
    fs::create_dir_all(dir.join("strings/0x407")).unwrap();
    fs::write(dir.join("strings/0x407/product"), "old\n").unwrap();
    fs::write(dir.join("UDC"), "udc0\n").unwrap();

    let mut gadget = tree.gadget();
    gadget.enable(&set()).unwrap();
    assert_eq!(tree.entries("configs"), ["c.1"]);
    assert_eq!(tree.entries("strings"), ["0x409"]);
    assert!(
        dir.join("functions/ffs.udc0").is_dir(),
        "the link went, not what it led to"
    );
}

#[test]
fn a_set_linux_cannot_carry_is_refused_before_anything_is_touched() {
    let tree = Tree::new();
    let mut gadget = tree.gadget();
    assert_eq!(gadget.enable(&[1, 2, 3]), Err(ErrorKind::InvalidInput));

    // A fourth alternate setting: FunctionFS takes three.
    let mut body = function(64);
    for alternate in 1..=3 {
        body.extend_from_slice(&[9, 4, 0, alternate, 0, 0xff, 0, 0, 0]);
    }
    let alternates = [&DEVICE[..], &configuration(&body), &table()].concat();
    assert!(UsbDescriptors::parse(&alternates).is_ok());
    assert_eq!(gadget.enable(&alternates), Err(ErrorKind::Unsupported));

    // An endpoint address in two settings.
    let mut body = function(64);
    body.extend_from_slice(&[9, 4, 0, 1, 1, 0xff, 0, 0, 0]);
    body.extend_from_slice(&[7, 5, 0x81, 1, 0, 1, 1]);
    let twice = [&DEVICE[..], &configuration(&body), &table()].concat();
    assert!(UsbDescriptors::parse(&twice).is_ok());
    assert_eq!(gadget.enable(&twice), Err(ErrorKind::Unsupported));

    // No interface at all.
    let mut empty = configuration(&[]);
    empty[4] = 0;
    let empty = [&DEVICE[..], &empty, &table()].concat();
    assert_eq!(gadget.enable(&empty), Err(ErrorKind::Unsupported));

    // A product string configfs has no room for.
    let long = "x".repeat(126);
    let mut strings = vec![4, 3, 0x09, 0x04];
    for text in ["Maker", &long, "0001", "data", "main"] {
        strings.extend(string(text));
    }
    let long = [&DEVICE[..], &configuration(&function(64)), &strings].concat();
    assert_eq!(gadget.enable(&long), Err(ErrorKind::Unsupported));

    assert!(tree.is_bare());
    assert!(!tree.gadget_dir().join("idVendor").exists());
    assert_eq!(fs::read(tree.mount().join("ep0")).unwrap(), b"");
}

/// The function with a second setting of its interface that adds an isochronous IN endpoint
/// of `full` bytes a frame at full speed and `high` (`wMaxPacketSize`, transactions in bits 11
/// and 12) at high speed.
fn streaming(full: u16, high: u16) -> Vec<u8> {
    let mut speeds = Vec::new();
    for (bulk, iso, interval) in [(64u16, full, 1u8), (512, high, 1)] {
        let mut body = function(bulk);
        body.extend_from_slice(&[9, 4, 0, 1, 3, 0xff, 0, 0, 0]);
        // FunctionFS takes an address in one setting only, so the second has its own.
        let [low, hi] = bulk.to_le_bytes();
        body.extend_from_slice(&[7, 5, 0x84, 2, low, hi, 0]);
        body.extend_from_slice(&[7, 5, 0x05, 2, low, hi, 0]);
        let [low, hi] = iso.to_le_bytes();
        body.extend_from_slice(&[7, 5, 0x83, 0x05, low, hi, interval]);
        speeds.push(configuration(&body));
    }
    [&DEVICE[..], &speeds[0], &speeds[1], &table()].concat()
}

#[test]
fn alternate_settings_and_isochronous_endpoints_are_carried() {
    let tree = Tree::new();
    for file in ["ep83", "ep84", "ep05"] {
        fs::write(tree.mount().join(file), "").unwrap();
    }
    let mut gadget = tree.gadget();
    // 1023 bytes a frame at full speed, three transactions of 1024 a microframe at high speed.
    gadget.enable(&streaming(1023, 1024 | 2 << 11)).unwrap();
    let ep0 = fs::read(tree.mount().join("ep0")).unwrap();
    assert!(
        ep0.windows(7)
            .any(|d| d == [7, 5, 0x83, 0x05, 0x00, 0x14, 1]),
        "the high-speed descriptor goes to FunctionFS as it was"
    );
    tree.event(ENABLE);
    assert_eq!(gadget.next_event(), Ok(Some(UsbFunctionEvent::Configured)));

    // A write is at most one interval, at the larger speed's size.
    let interval = vec![7u8; 3072];
    assert_eq!(gadget.write(0x83, &interval, WAIT), Ok(3072));
    assert_eq!(fs::read(tree.mount().join("ep83")).unwrap(), interval);
    assert_eq!(
        gadget.write(0x83, &[0; 3073], WAIT),
        Err(ErrorKind::InvalidInput)
    );
    assert_eq!(gadget.set_halt(0x83, true), Err(ErrorKind::Unsupported));
    // The second setting's bulk endpoints are endpoints like any other.
    assert_eq!(gadget.write(0x84, &[1, 2], WAIT), Ok(2));

    // The host picks a setting: FunctionFS says ENABLE again, and nothing of before is kept.
    tree.event(ENABLE);
    assert_eq!(gadget.next_event(), Ok(Some(UsbFunctionEvent::Configured)));
    // Writes go on past more than the transfers there are, and allocate nothing.
    let (written, count) = allocations(|| {
        (0..ISO_TRANSFERS * 3)
            .map(|_| gadget.write(0x83, &[1; 16], WAIT))
            .all(|written| written == Ok(16))
    });
    assert_eq!((written, count), (true, 0));
}

#[test]
fn an_isochronous_out_endpoint_reads_one_interval_at_a_time() {
    let tree = Tree::new();
    let mut set = streaming(64, 64);
    // Make the isochronous endpoint an OUT one, at both speeds.
    let at: Vec<usize> = set
        .windows(4)
        .enumerate()
        .filter(|(_, d)| *d == [7, 5, 0x83, 0x05])
        .map(|(at, _)| at + 2)
        .collect();
    assert_eq!(at.len(), 2);
    for at in at {
        set[at] = 0x03;
    }
    for file in ["ep03", "ep84", "ep05"] {
        fs::write(tree.mount().join(file), "").unwrap();
    }
    let mut gadget = tree.gadget();
    gadget.enable(&set).unwrap();
    tree.event(ENABLE);
    gadget.next_event().unwrap();
    fs::write(tree.mount().join("ep03"), [5u8; 64]).unwrap();
    let mut short = [0u8; 63];
    assert_eq!(
        gadget.read(3, &mut short, WAIT),
        Err(ErrorKind::InvalidInput),
        "a read has room for a whole interval"
    );
    let mut data = [0u8; 64];
    assert_eq!(gadget.read(3, &mut data, WAIT), Ok(64));
    assert_eq!(data, [5; 64]);
}

#[test]
fn one_speed_and_no_strings_writes_only_what_the_set_has() {
    let tree = Tree::new();
    fs::remove_file(tree.gadget_dir().join("max_speed")).unwrap();
    let mut device = DEVICE;
    device[14..17].fill(0);
    let mut body = function(64);
    body[8] = 0;
    let mut config = configuration(&body);
    config[6] = 0;
    let mut gadget = tree.gadget();
    gadget.enable(&[&device[..], &config].concat()).unwrap();
    assert!(tree.entries("strings").is_empty());
    assert_eq!(
        tree.entries("configs/c.1"),
        ["MaxPower", "bmAttributes", "ffs.udc0"]
    );
    assert!(
        !tree.gadget_dir().join("max_speed").exists(),
        "an attribute the kernel lacks is not made"
    );
    let ep0 = fs::read(tree.mount().join("ep0")).unwrap();
    assert_eq!(ep0[8..16], [81, 0, 0, 0, 3, 0, 0, 0], "full speed only");
    assert_eq!(
        ep0[ep0.len() - 16..],
        [2, 0, 0, 0, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    );

    drop(gadget);
    fs::write(tree.gadget_dir().join("max_speed"), "").unwrap();
    tree.gadget()
        .enable(&[&device[..], &config].concat())
        .unwrap();
    assert_eq!(tree.attribute("max_speed"), "full-speed\n");
}

#[test]
fn a_controller_name_is_one_directory_name_and_nothing_else() {
    for name in [
        "udc:fe980000.usb",
        "udc:musb-hdrc.2.auto",
        "udc:ci_hdrc.0",
        "udc:a",
    ] {
        assert_eq!(controller(name), Some(&name[4..]), "{name}");
    }
    let long = format!("udc:{}", "a".repeat(36));
    let refused = [
        "udc:",
        "udc",
        "udc:..",
        "udc:.hidden",
        "udc:a/b",
        "udc:../x",
        "udc:a b",
        "udc:a\n",
        "usb:1-1",
        "fe980000.usb",
        &long,
    ];
    for name in refused {
        assert_eq!(controller(name), None, "{name}");
    }
    assert!(controller(&long[..long.len() - 1]).is_some());
    let root = std::env::temp_dir();
    assert!(FunctionFsGadget::new(&root, &root, &root, "../x").is_none());
}

#[test]
fn the_board_provides_a_controller_that_is_there_to_one_owner() {
    let tree = Tree::new();
    // `/sys/class/udc` and `/sys/kernel/config` under one root, `/dev/usb-ffs` under another.
    let sys = tree.root.join("sys");
    fs::create_dir_all(sys.join("class/udc").join(UDC)).unwrap();
    fs::create_dir_all(sys.join("kernel")).unwrap();
    fs::rename(tree.root.join("config"), sys.join("kernel/config")).unwrap();
    let dev = tree.root.join("dev");
    fs::create_dir_all(&dev).unwrap();
    fs::rename(tree.root.join("ffs"), dev.join("usb-ffs")).unwrap();
    let mut board = LinuxBoard::at(&dev, &sys);

    assert!(board.provided("udc:udc0"));
    assert!(!board.provided("udc:udc1"));
    assert!(!board.provided("udc:"));
    let status =
        |board: &mut LinuxBoard, need: &str| board.usb_function(need).map(|_| ()).unwrap_err();
    assert_eq!(status(&mut board, "udc:udc1"), Status::NO_DEVICE);
    assert_eq!(status(&mut board, "udc:"), Status::NOT_FOUND);
    assert_eq!(status(&mut board, "udc:../udc0"), Status::NOT_FOUND);
    assert_eq!(status(&mut board, "gadget"), Status::NOT_FOUND);

    let mut function = board.usb_function("udc:udc0").map_err(|_| ()).unwrap();
    assert_eq!(status(&mut board, "udc:udc0"), Status::BUSY);
    function.enable(&set()).unwrap();
    let gadget = sys.join("kernel/config/usb_gadget/lemnos-udc0");
    assert_eq!(fs::read_to_string(gadget.join("UDC")).unwrap(), "udc0\n");
    drop(function);
    assert_eq!(fs::read_to_string(gadget.join("UDC")).unwrap(), "\n");
    assert!(board.usb_function("udc:udc0").is_ok());
}

#[test]
fn events_requests_and_endpoint_bytes_go_through_the_files() {
    let tree = Tree::new();
    let mut gadget = tree.gadget();
    gadget.enable(&set()).unwrap();
    let mut data = [0u8; 8];
    assert_eq!(gadget.next_event(), Ok(None));
    assert_eq!(gadget.read(2, &mut data, WAIT), Err(ErrorKind::Unavailable));

    tree.event(BIND);
    tree.event(ENABLE);
    assert_eq!(gadget.next_event(), Ok(Some(UsbFunctionEvent::Configured)));

    // The host's bytes on the OUT endpoint, and ours on the IN endpoint.
    fs::write(tree.mount().join("ep02"), [1, 2, 3]).unwrap();
    assert_eq!(gadget.read(2, &mut data, WAIT), Ok(3));
    assert_eq!(data[..3], [1, 2, 3]);
    assert_eq!(gadget.write(0x81, &[4, 5], WAIT), Ok(2));
    assert_eq!(fs::read(tree.mount().join("ep81")).unwrap(), [4, 5]);
    assert_eq!(
        gadget.read(0x81, &mut data, WAIT),
        Err(ErrorKind::InvalidInput)
    );
    assert_eq!(gadget.write(2, &[1], WAIT), Err(ErrorKind::InvalidInput));
    assert_eq!(gadget.write(0x83, &[1], WAIT), Err(ErrorKind::InvalidInput));

    // A request with data from the host: the data stage follows the event in the file.
    tree.kernel_says(&[0x40, 7, 1, 0, 2, 0, 3, 0, 4, 0, 0, 0]);
    tree.kernel_says(&[9, 8, 7]);
    let request = ControlSetup {
        request_type: 0x40,
        request: 7,
        value: 1,
        index: 2,
    };
    assert_eq!(
        gadget.next_event(),
        Ok(Some(UsbFunctionEvent::Setup {
            setup: request,
            length: 3
        }))
    );
    assert_eq!(
        gadget.control_out(&mut data[..2], WAIT),
        Err(ErrorKind::InvalidInput)
    );
    assert_eq!(gadget.control_out(&mut data, WAIT), Ok(3));
    assert_eq!(data[..3], [9, 8, 7]);

    // A request for data: the answer is cut to wLength.
    tree.kernel_says(&[0xc0, 8, 0, 0, 0, 0, 2, 0, 4, 0, 0, 0]);
    gadget.next_event().unwrap();
    assert_eq!(gadget.control_in(&[6, 6, 6, 6], WAIT), Ok(2));
    let ep0 = fs::read(tree.mount().join("ep0")).unwrap();
    assert_eq!(ep0[ep0.len() - 2..], [6, 6]);
    assert_eq!(gadget.control_in(&[6], WAIT), Err(ErrorKind::InvalidInput));

    // What only FunctionFS can do fails cleanly on plain files.
    assert!(gadget.set_halt(0x81, true).is_err());
    assert_eq!(gadget.set_halt(0x81, false), Err(ErrorKind::Unsupported));
    assert_eq!(gadget.set_halt(9, true), Err(ErrorKind::InvalidInput));

    // The controller lets go: every later call says so until the function is enabled again.
    tree.event(UNBIND);
    assert_eq!(gadget.next_event(), Err(ErrorKind::Unavailable));
    assert_eq!(gadget.next_event(), Err(ErrorKind::Unavailable));
    assert_eq!(gadget.write(0x81, &[1], WAIT), Err(ErrorKind::Unavailable));
    gadget.disable().unwrap();
    fs::write(tree.mount().join("ep0"), "").unwrap();
    gadget.enable(&set()).unwrap();
    assert_eq!(gadget.next_event(), Ok(None));
}

#[test]
fn nothing_allocates_once_the_function_is_enabled() {
    let tree = Tree::new();
    let mut gadget = tree.gadget();
    gadget.enable(&set()).unwrap();
    tree.event(ENABLE);
    tree.kernel_says(&[0xc0, 8, 0, 0, 0, 0, 2, 0, 4, 0, 0, 0]);
    fs::write(tree.mount().join("ep02"), [1, 2, 3]).unwrap();
    let mut data = [0u8; 8];
    let (done, count) = allocations(|| {
        let configured = gadget.next_event();
        let setup = gadget.next_event().map(|event| event.is_some());
        let answered = gadget.control_in(&[1, 2], WAIT);
        let idle = gadget.next_event();
        let read = gadget.read(2, &mut data, WAIT);
        let written = gadget.write(0x81, &[4, 5], WAIT);
        let wrong = gadget.read(0x81, &mut data, WAIT);
        (configured, setup, answered, idle, read, written, wrong)
    });
    assert_eq!(
        done,
        (
            Ok(Some(UsbFunctionEvent::Configured)),
            Ok(true),
            Ok(2),
            Ok(None),
            Ok(3),
            Ok(2),
            Err(ErrorKind::InvalidInput)
        )
    );
    assert_eq!(count, 0);
}

/// What a session did to its wire.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Did {
    Stall { request_is_in: bool },
    Send(Vec<u8>),
    Receive(usize),
    Halt(usize, bool),
    ClearHalt(usize),
    Read(usize),
    Write(usize, Vec<u8>),
    Forget,
}

/// A wire that answers as FunctionFS does, from a script.
#[derive(Default)]
struct FakeWire {
    events: VecDeque<Event>,
    did: Vec<Did>,
    /// The errno the next call answers with, instead of what FunctionFS says on success.
    fail: Option<i32>,
}

impl FakeWire {
    /// The scripted failure, or `ok`.
    fn answer<T>(&mut self, ok: T) -> io::Result<T> {
        match self.fail.take() {
            Some(errno) => Err(io::Error::from_raw_os_error(errno)),
            None => Ok(ok),
        }
    }
}

impl Wire for FakeWire {
    fn next_event(&mut self) -> io::Result<Option<Event>> {
        Ok(self.events.pop_front())
    }

    fn send(&mut self, data: &[u8]) -> io::Result<usize> {
        self.did.push(Did::Send(data.to_vec()));
        self.answer(data.len())
    }

    fn receive(&mut self, data: &mut [u8]) -> io::Result<usize> {
        self.did.push(Did::Receive(data.len()));
        self.answer(data.len())
    }

    fn stall(&mut self, request_is_in: bool) -> io::Result<()> {
        self.did.push(Did::Stall { request_is_in });
        self.answer(())
    }

    fn halt(&mut self, endpoint: usize, endpoint_is_in: bool) -> io::Result<()> {
        self.did.push(Did::Halt(endpoint, endpoint_is_in));
        self.answer(())
    }

    fn clear_halt(&mut self, endpoint: usize) -> io::Result<()> {
        self.did.push(Did::ClearHalt(endpoint));
        self.answer(())
    }

    fn read(&mut self, endpoint: usize, data: &mut [u8], _: Duration) -> io::Result<usize> {
        self.did.push(Did::Read(endpoint));
        self.answer(data.len())
    }

    fn write(&mut self, endpoint: usize, data: &[u8], _: Duration) -> io::Result<usize> {
        self.did.push(Did::Write(endpoint, data.to_vec()));
        self.answer(data.len())
    }

    fn forget(&mut self) {
        self.did.push(Did::Forget);
    }
}

fn request(request_type: u8, length: u16) -> Setup {
    Setup {
        request_type,
        request: 1,
        value: 0,
        index: 0,
        length,
    }
}

/// A session over a bulk IN endpoint, a bulk OUT endpoint, an isochronous IN endpoint of
/// 16 bytes an interval and an isochronous OUT one of 8.
fn session() -> Session<FakeWire> {
    Session::new(
        FakeWire::default(),
        vec![
            (0x81, None),
            (0x02, None),
            (0x83, Some(16)),
            (0x04, Some(8)),
        ],
    )
}

fn configured() -> Session<FakeWire> {
    let mut session = session();
    session.wire.events.push_back(Event::Enable);
    assert_eq!(session.next_event(), Ok(Some(UsbFunctionEvent::Configured)));
    // A new configuration starts with nothing of the one before.
    assert_eq!(session.wire.did, [Did::Forget]);
    session.wire.did.clear();
    session
}

#[test]
fn a_request_read_past_is_stalled_the_way_its_direction_needs() {
    let mut session = session();
    session.wire.events.extend([
        Event::Setup(request(0xc0, 4)),
        Event::Setup(request(0x40, 4)),
    ]);
    assert!(matches!(
        session.next_event(),
        Ok(Some(UsbFunctionEvent::Setup { length: 4, .. }))
    ));
    assert!(session.wire.did.is_empty());
    session.next_event().unwrap();
    assert_eq!(session.next_event(), Ok(None));
    assert_eq!(
        session.wire.did,
        [
            Did::Stall {
                request_is_in: true
            },
            Did::Stall {
                request_is_in: false
            }
        ]
    );
    assert_eq!(session.control_stall(), Err(ErrorKind::InvalidInput));
    assert_eq!(
        session.wire.did.len(),
        2,
        "nothing pending, nothing stalled"
    );
}

#[test]
fn a_request_is_answered_once_in_its_own_direction() {
    let mut session = session();
    let mut data = [0u8; 8];
    assert_eq!(session.control_in(&[1]), Err(ErrorKind::InvalidInput));
    assert_eq!(session.control_out(&mut data), Err(ErrorKind::InvalidInput));

    session
        .wire
        .events
        .push_back(Event::Setup(request(0xc0, 2)));
    session.next_event().unwrap();
    assert_eq!(session.control_out(&mut data), Err(ErrorKind::InvalidInput));
    assert_eq!(session.control_in(&[1, 2, 3]), Ok(2));
    assert_eq!(session.control_in(&[1]), Err(ErrorKind::InvalidInput));

    session
        .wire
        .events
        .push_back(Event::Setup(request(0x40, 3)));
    session.next_event().unwrap();
    assert_eq!(session.control_in(&[1]), Err(ErrorKind::InvalidInput));
    assert_eq!(
        session.control_out(&mut data[..2]),
        Err(ErrorKind::InvalidInput)
    );
    assert_eq!(session.control_out(&mut data), Ok(3));

    session
        .wire
        .events
        .push_back(Event::Setup(request(0x40, 0)));
    session.next_event().unwrap();
    assert_eq!(session.control_out(&mut []), Ok(0));

    session
        .wire
        .events
        .push_back(Event::Setup(request(0xc0, 1)));
    session.next_event().unwrap();
    assert_eq!(session.control_stall(), Ok(()));
    assert_eq!(session.next_event(), Ok(None));
    assert_eq!(
        session.wire.did,
        [
            Did::Send(vec![1, 2]),
            Did::Receive(3),
            Did::Receive(0),
            Did::Stall {
                request_is_in: true
            },
        ]
    );
}

#[test]
fn a_request_the_host_abandoned_is_unavailable_and_no_longer_pending() {
    for errno in [
        functionfs::ABANDONED,
        functionfs::NO_REQUEST,
        libc::ESHUTDOWN,
    ] {
        let mut session = session();
        session
            .wire
            .events
            .push_back(Event::Setup(request(0xc0, 2)));
        session.next_event().unwrap();
        session.wire.fail = Some(errno);
        assert_eq!(
            session.control_in(&[1]),
            Err(ErrorKind::Unavailable),
            "{errno}"
        );
        assert_eq!(session.control_in(&[1]), Err(ErrorKind::InvalidInput));

        session
            .wire
            .events
            .push_back(Event::Setup(request(0x40, 2)));
        session.next_event().unwrap();
        session.wire.fail = Some(errno);
        assert_eq!(
            session.control_stall(),
            Err(ErrorKind::Unavailable),
            "{errno}"
        );
        assert_eq!(session.control_stall(), Err(ErrorKind::InvalidInput));
    }
}

#[test]
fn a_reset_or_a_pull_is_one_deconfigured_and_endpoints_stop_until_configured_again() {
    let mut session = session();
    let mut data = [0u8; 4];
    // Nothing was configured: a disable changes nothing the function knew of.
    session.wire.events.extend([Event::Bind, Event::Disable]);
    assert_eq!(session.next_event(), Ok(None));
    assert_eq!(
        session.read(2, &mut data, WAIT),
        Err(ErrorKind::Unavailable)
    );
    assert_eq!(session.set_halt(2, true), Err(ErrorKind::Unavailable));

    session.wire.events.extend([
        Event::Enable,
        Event::Suspend,
        Event::Resume,
        Event::Disable,
        Event::Disable,
        Event::Enable,
    ]);
    let mut seen = Vec::new();
    while let Some(event) = session.next_event().unwrap() {
        if event == UsbFunctionEvent::Deconfigured {
            assert_eq!(session.write(0x81, &[1], WAIT), Err(ErrorKind::Unavailable));
        }
        seen.push(event);
    }
    assert_eq!(
        seen,
        [
            UsbFunctionEvent::Configured,
            UsbFunctionEvent::Suspended,
            UsbFunctionEvent::Resumed,
            UsbFunctionEvent::Deconfigured,
            UsbFunctionEvent::Configured,
        ]
    );
    assert_eq!(session.write(0x81, &[1], WAIT), Ok(1));
    // Each configuration began with what the one before left dropped.
    assert_eq!(
        session.wire.did,
        [Did::Forget, Did::Forget, Did::Write(0, vec![1])]
    );
}

#[test]
fn a_transfer_the_host_cut_is_unavailable_and_one_nothing_answered_is_a_timeout() {
    let mut session = configured();
    let mut data = [0u8; 4];
    for errno in [
        libc::EAGAIN,
        libc::ESHUTDOWN,
        libc::ECONNRESET,
        libc::ENODEV,
    ] {
        session.wire.fail = Some(errno);
        assert_eq!(
            session.read(2, &mut data, WAIT),
            Err(ErrorKind::Unavailable)
        );
        session.wire.fail = Some(errno);
        assert_eq!(session.write(0x81, &[1], WAIT), Err(ErrorKind::Unavailable));
    }
    assert_eq!(
        wire_kind(&io::ErrorKind::TimedOut.into()),
        ErrorKind::Timeout
    );
    assert_eq!(session.read(2, &mut data, WAIT), Ok(4));
    assert_eq!(
        session.read(0x81, &mut data, WAIT),
        Err(ErrorKind::InvalidInput)
    );
    assert_eq!(session.write(2, &[1], WAIT), Err(ErrorKind::InvalidInput));
    assert_eq!(
        session.read(4, &mut data, WAIT),
        Err(ErrorKind::InvalidInput)
    );
}

#[test]
fn a_halt_goes_to_the_named_endpoint_and_an_isochronous_one_has_none() {
    let mut session = configured();
    session.set_halt(0x81, true).unwrap();
    session.set_halt(0x02, true).unwrap();
    session.set_halt(0x02, false).unwrap();
    assert_eq!(
        session.wire.did,
        [Did::Halt(0, true), Did::Halt(1, false), Did::ClearHalt(1)]
    );
    assert_eq!(session.set_halt(0x83, true), Err(ErrorKind::Unsupported));
    assert_eq!(session.set_halt(0x01, true), Err(ErrorKind::InvalidInput));
    assert_eq!(session.set_halt(0x82, false), Err(ErrorKind::InvalidInput));
    session.wire.fail = Some(libc::EAGAIN);
    assert_eq!(session.set_halt(0x81, true), Err(ErrorKind::Unavailable));
    assert_eq!(session.wire.did.len(), 4);
}

#[test]
fn an_isochronous_transfer_is_one_interval() {
    let mut session = configured();
    assert_eq!(session.write(0x83, &[1; 16], WAIT), Ok(16));
    assert_eq!(session.write(0x83, &[], WAIT), Ok(0));
    assert_eq!(
        session.write(0x83, &[1; 17], WAIT),
        Err(ErrorKind::InvalidInput)
    );
    let mut data = [0u8; 8];
    assert_eq!(session.read(0x04, &mut data, WAIT), Ok(8));
    assert_eq!(
        session.read(0x04, &mut data[..7], WAIT),
        Err(ErrorKind::InvalidInput)
    );
    // A bulk endpoint takes any length; what one transfer carries is the wire's to say.
    assert_eq!(session.write(0x81, &[1; 17], WAIT), Ok(17));
    assert_eq!(
        session.wire.did,
        [
            Did::Write(2, vec![1; 16]),
            Did::Write(2, vec![]),
            Did::Read(3),
            Did::Write(0, vec![1; 17]),
        ]
    );
}

#[test]
fn an_unbind_leaves_the_session_unavailable_for_good() {
    let mut session = configured();
    session.wire.events.extend([Event::Unbind, Event::Enable]);
    assert_eq!(session.next_event(), Err(ErrorKind::Unavailable));
    assert_eq!(session.next_event(), Err(ErrorKind::Unavailable));
    assert_eq!(session.wire.events.len(), 1, "nothing more is read");
    assert_eq!(session.write(0x81, &[1], WAIT), Err(ErrorKind::Unavailable));
    assert_eq!(session.set_halt(0x81, true), Err(ErrorKind::Unavailable));
}

#[test]
fn setup_failures_say_what_was_wrong() {
    let kind = |errno| setup_kind(&io::Error::from_raw_os_error(errno));
    assert_eq!(kind(libc::ENOENT), ErrorKind::NotFound);
    assert_eq!(kind(libc::EBUSY), ErrorKind::Busy);
    assert_eq!(kind(libc::EINVAL), ErrorKind::InvalidInput);
    assert_eq!(kind(libc::EOVERFLOW), ErrorKind::InvalidInput);
    assert_eq!(kind(libc::EACCES), ErrorKind::PermissionDenied);
    assert_eq!(kind(libc::ENODEV), ErrorKind::Unavailable);
}
