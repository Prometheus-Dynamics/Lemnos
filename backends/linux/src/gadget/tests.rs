//! The gadget against a stand-in tree of plain directories and files under the temporary
//! directory, and the session against a scripted wire. Nothing here goes near the real
//! configfs, a controller or a FunctionFS mount.

use std::collections::VecDeque;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use lemnos_core::{Provider, Status};
use lemnos_measure::test_alloc::allocations;

use super::*;
use crate::LinuxBoard;

const UDC: &str = "udc0";
const WAIT: Duration = Duration::from_millis(200);
/// A data stage begun and only looked at: the driver's way.
const NOW: Duration = Duration::ZERO;

/// Enables `gadget` and waits for its bring-up off the caller's thread: what a driver's ticks do,
/// in one call.
fn up<F: UsbFunction<Error = ErrorKind> + ?Sized>(
    gadget: &mut F,
    set: &[u8],
) -> Result<(), ErrorKind> {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        match gadget.enable(set) {
            Err(ErrorKind::Busy) if gadget.enable_pending() && Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(1));
            }
            other => return other,
        }
    }
}

/// Waits until `done` holds, for what a gadget's own thread does after the caller has gone on.
fn eventually(done: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < until, "never came");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Waits until the gadget's own thread has done what it was given: a disable's unbind and close.
fn settled(gadget: &FunctionFsGadget) {
    Work::wait_idle(&gadget.work.shared);
}

/// Drops `gadget` and waits until its thread has unbound and closed what it had.
fn gone(gadget: FunctionFsGadget) {
    let shared = Arc::clone(&gadget.work.shared);
    drop(gadget);
    Work::wait_idle(&shared);
}

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
    config_root: PathBuf,
    mounts_root: PathBuf,
    udc_root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "lemnos-gadget-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let tree = Self {
            config_root: root.join("config"),
            mounts_root: root.join("ffs"),
            udc_root: root.join("udc"),
            root,
        };
        fs::create_dir_all(tree.gadget_dir().join("functions/ffs.udc0")).unwrap();
        fs::create_dir_all(tree.gadget_dir().join("configs")).unwrap();
        fs::create_dir_all(tree.gadget_dir().join("strings")).unwrap();
        fs::write(tree.gadget_dir().join("UDC"), "").unwrap();
        fs::write(tree.gadget_dir().join("max_speed"), "").unwrap();
        fs::create_dir_all(tree.mount()).unwrap();
        for file in ["ep0", "ep1", "ep2"] {
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

    /// The three roots the setup takes, in this tree.
    fn roots(&self) -> Roots<'_> {
        Roots {
            configfs: &self.config_root,
            mounts: &self.mounts_root,
            udc_class: &self.udc_root,
        }
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

/// Records what the setup asked of the mounts.
#[derive(Debug, Default)]
struct Recorder {
    mounts: Vec<(String, PathBuf, String)>,
    unmounts: Vec<PathBuf>,
    /// How many mounts f_fs refuses next (`EBUSY`): the instance is mounted elsewhere or has open
    /// files.
    busy: usize,
}

impl Mounts for Recorder {
    fn functionfs(&mut self, source: &str, target: &Path, options: &str) -> io::Result<()> {
        if self.busy > 0 {
            self.busy -= 1;
            return Err(io::Error::from_raw_os_error(libc::EBUSY));
        }
        self.mounts.push((
            source.to_string(),
            target.to_path_buf(),
            options.to_string(),
        ));
        Ok(())
    }

    fn unmount(&mut self, target: &Path) -> io::Result<()> {
        self.unmounts.push(target.to_path_buf());
        Ok(())
    }
}

/// A scratch directory under TMPDIR, removed when the guard drops, pass or panic. It derefs to
/// `path`, a file or directory inside `dir`, so the guard must stay bound while `path` is used.
struct Scratch {
    dir: PathBuf,
    path: PathBuf,
}

impl Scratch {
    fn of_dir(dir: PathBuf) -> Self {
        Self {
            path: dir.clone(),
            dir,
        }
    }
}

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The owner the tree's own files have, so `chown` can be checked without root.
fn own() -> Owner {
    // A directory this test makes, so its owner is the test's own user, whatever owns the
    // system's temporary directory (root, in CI).
    // One name per call: tests run in parallel, and each removes its own when done.
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("lemnos-gadget-owner-{}-{n}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let _scratch = Scratch::of_dir(dir.clone());
    let meta = fs::metadata(&dir).unwrap();
    Owner {
        uid: meta.uid(),
        gid: meta.gid(),
    }
}

fn ncm() -> GadgetFunction {
    GadgetFunction::Ncm {
        host: [0x02, 0, 0, 0, 0, 0x01],
        device: [0x02, 0, 0, 0, 0, 0x02],
        ifname: None,
    }
}

/// The setup, with the functions given, as root would run it.
fn setup_with(tree: &Tree, owner: Owner, functions: &[GadgetFunction]) -> io::Result<Recorder> {
    let mut recorder = Recorder::default();
    prepare(&tree.roots(), UDC, owner, &set(), functions, &mut recorder)?;
    Ok(recorder)
}

fn setup(tree: &Tree) -> Recorder {
    setup_with(tree, own(), &[GadgetFunction::Ffs]).unwrap()
}

#[test]
fn setup_writes_the_identity_the_configuration_and_the_function_and_leaves_udc_unbound() {
    let tree = Tree::new();
    let recorder = setup(&tree);

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
        ("UDC", "\n"),
    ];
    for (file, value) in expected {
        assert_eq!(tree.attribute(file), value, "{file}");
    }
    // The link's target is the directory named through a descriptor (`/proc/self/fd/N/ffs.udc0`);
    // configfs resolves it and shows its own, so the name is what is checked.
    assert!(
        fs::read_link(tree.gadget_dir().join("configs/c.1/ffs.udc0"))
            .unwrap()
            .ends_with("ffs.udc0")
    );
    // The fixture's mount is already there, with its ep0: it is reused, not mounted again.
    assert!(
        recorder.mounts.is_empty(),
        "a mount already there is reused"
    );
}

#[test]
fn setup_makes_the_network_and_console_functions_in_the_order_listed() {
    let tree = Tree::new();
    let functions = [GadgetFunction::Acm, ncm(), GadgetFunction::Ffs];
    setup_with(&tree, own(), &functions).unwrap();

    assert_eq!(
        tree.entries("configs/c.1"),
        [
            "MaxPower",
            "acm.GS0",
            "bmAttributes",
            "ffs.udc0",
            "ncm.usb0",
            "strings"
        ],
        "every link and attribute of the configuration is there"
    );
    assert_eq!(
        tree.attribute("functions/ncm.usb0/host_addr"),
        "02:00:00:00:00:01\n"
    );
    assert_eq!(
        tree.attribute("functions/ncm.usb0/dev_addr"),
        "02:00:00:00:00:02\n"
    );
    assert!(tree.gadget_dir().join("functions/acm.GS0").is_dir());
    assert!(tree.gadget_dir().join("functions/ffs.udc0").is_dir());
    assert!(
        fs::read_link(tree.gadget_dir().join("configs/c.1/acm.GS0")).is_ok(),
        "the serial function is linked in"
    );
}

#[test]
fn setup_hands_udc_to_the_owner_at_mode_0600_and_nothing_else_to_anyone() {
    let tree = Tree::new();
    setup(&tree);
    let meta = fs::metadata(tree.gadget_dir().join("UDC")).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_eq!(meta.uid(), own().uid);
    assert_eq!(meta.gid(), own().gid);
}

#[test]
fn setup_refuses_a_controller_that_is_not_there_and_makes_nothing() {
    let tree = Tree::new();
    fs::remove_dir_all(tree.root.join("udc").join(UDC)).unwrap();
    let error = setup_with(&tree, own(), &[GadgetFunction::Ffs]).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
    assert!(tree.entries("configs").is_empty());
    assert!(!tree.gadget_dir().join("idVendor").exists());
}

#[test]
fn enable_writes_descriptors_to_ep0_then_binds_and_touches_no_configfs_directory() {
    let tree = Tree::new();
    setup(&tree);
    let configs_before = tree.entries("configs");
    let mut gadget = tree.gadget();
    up(&mut gadget, &set()).unwrap();

    // `ep0` got the descriptors of both speeds without their configuration headers, then
    // every string of the table.
    let mut blob = Vec::new();
    blob.extend(3u32.to_le_bytes());
    blob.extend(0u32.to_le_bytes()); // the descriptors' length, set below
    // FS | HS | MS OS | ALL_CTRL_RECIP
    blob.extend(75u32.to_le_bytes());
    blob.extend(3u32.to_le_bytes());
    blob.extend(3u32.to_le_bytes());
    blob.extend(2u32.to_le_bytes()); // two Microsoft OS 1.0 sections
    blob.extend(function(64));
    blob.extend(function(512));
    // The link's compatible ID section and its GUID property section (the golden bytes of
    // both are in the FunctionFS tests).
    blob.extend(
        functionfs::os_compat_section(&[CompatEntry {
            interface: 0,
            compatible: ms_os::LINK_COMPATIBLE_ID,
            sub_compatible: [0; 8],
        }])
        .unwrap(),
    );
    blob.extend(
        functionfs::os_prop_section(
            0,
            &[ExtProp {
                name: ms_os::LINK_GUID_PROPERTY,
                data: ms_os::LINK_GUID,
            }],
        )
        .unwrap(),
    );
    let descriptors_length = blob.len() as u32;
    blob[4..8].copy_from_slice(&descriptors_length.to_le_bytes());
    blob.extend(2u32.to_le_bytes());
    blob.extend(45u32.to_le_bytes());
    blob.extend(5u32.to_le_bytes());
    blob.extend(1u32.to_le_bytes());
    blob.extend(0x0409u16.to_le_bytes());
    blob.extend(b"Maker\0Thing\x000001\0data\0main\0");
    assert_eq!(fs::read(tree.mount().join("ep0")).unwrap(), blob);
    assert_eq!(
        tree.attribute("UDC"),
        "udc0\n",
        "bound after the descriptors"
    );
    assert_eq!(tree.entries("configs"), configs_before);

    assert_eq!(up(&mut gadget, &set()), Err(ErrorKind::Busy));
}

#[test]
fn enable_before_setup_is_not_found_and_touches_nothing() {
    let tree = Tree::new();
    let mut gadget = tree.gadget();
    assert_eq!(up(&mut gadget, &set()), Err(ErrorKind::NotFound));
    assert_eq!(tree.attribute("UDC"), "", "not even an unbind");
    assert!(tree.entries("configs").is_empty());
}

#[test]
fn a_controller_that_let_go_keeps_ep0_for_the_bind_again() {
    let tree = Tree::new();
    setup(&tree);
    let mut gadget = tree.gadget();
    up(&mut gadget, &set()).unwrap();
    assert_eq!(tree.attribute("UDC"), "udc0\n");
    // The controller let go: the kernel unbound the function, and the session saw the unbind.
    fs::write(tree.gadget_dir().join("UDC"), "\n").unwrap();
    gadget.session.as_mut().unwrap().released = true;
    assert!(
        gadget.endpoint_handles().is_err(),
        "nothing is lent while down"
    );
    assert_eq!(gadget.disable(), Ok(()));
    assert!(gadget.session.is_some(), "ep0 stays open across the let-go");
    assert_eq!(tree.attribute("UDC"), "\n");
    assert_eq!(
        up(&mut gadget, &set()),
        Ok(()),
        "bound again with the attribute"
    );
    assert_eq!(tree.attribute("UDC"), "udc0\n");
    assert!(!gadget.session.as_ref().unwrap().released);
    assert!(gadget.endpoint_handles().is_ok());
}

/// A session the controller let go of holds ep0 itself: its bind again does not look for the
/// mount, which a daemon that took the session up from the store does not see (an earlier daemon
/// mounted it, in its own mount namespace). Seen in QEMU: every retry failed `NotFound`.
#[test]
fn a_let_go_session_is_bound_again_without_the_mount_in_view() {
    let tree = Tree::new();
    setup(&tree);
    let mut gadget = tree.gadget();
    up(&mut gadget, &set()).unwrap();
    fs::write(tree.gadget_dir().join("UDC"), "\n").unwrap();
    gadget.session.as_mut().unwrap().released = true;
    assert_eq!(gadget.disable(), Ok(()));
    // The mount is out of this daemon's view: its ep0 is not there.
    fs::remove_file(tree.mount().join("ep0")).unwrap();
    assert_eq!(
        up(&mut gadget, &set()),
        Ok(()),
        "bound again on the held ep0"
    );
    assert_eq!(tree.attribute("UDC"), "udc0\n");
    assert!(!gadget.session.as_ref().unwrap().released);
    // A fresh setup still needs the mount: without a session, the same tree is not ready.
    gone(gadget);
    let mut fresh = tree.gadget();
    assert_eq!(up(&mut fresh, &set()), Err(ErrorKind::NotFound));
}

#[test]
fn disable_and_drop_unbind_and_leave_the_setup_as_it_made_it() {
    let tree = Tree::new();
    setup(&tree);
    let mut gadget = tree.gadget();
    assert_eq!(gadget.disable(), Ok(()), "never enabled");
    assert_eq!(tree.attribute("UDC"), "\n", "as the setup left it: unbound");
    up(&mut gadget, &set()).unwrap();
    assert_eq!(tree.attribute("UDC"), "udc0\n");
    gadget.disable().unwrap();
    settled(&gadget);
    assert_eq!(tree.attribute("UDC"), "\n", "unbound");
    assert_eq!(tree.entries("configs"), ["c.1"], "the configuration stays");
    assert_eq!(gadget.next_event(), Err(ErrorKind::Unavailable));
    assert_eq!(gadget.disable(), Ok(()));

    up(&mut gadget, &set()).unwrap();
    gone(gadget);
    assert_eq!(tree.attribute("UDC"), "\n", "drop unbinds");
    assert_eq!(tree.entries("configs"), ["c.1"]);
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
        assert_eq!(up(&mut gadget, &set()), Err(ErrorKind::NotFound), "{what}");
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
    setup(&tree);
    // The second endpoint file is not there: everything up to it has been done by then.
    fs::remove_file(tree.mount().join("ep2")).unwrap();
    let mut gadget = tree.gadget();
    assert_eq!(up(&mut gadget, &set()), Err(ErrorKind::NotFound));
    assert_eq!(tree.attribute("UDC"), "\n", "no half-built gadget: unbound");
    assert_eq!(
        tree.entries("configs"),
        ["c.1"],
        "and the setup's configuration as it was"
    );
    assert_eq!(gadget.next_event(), Err(ErrorKind::Unavailable));

    // Once the piece is there the same call goes through.
    fs::write(tree.mount().join("ep2"), "").unwrap();
    up(&mut gadget, &set()).unwrap();
    assert_eq!(tree.attribute("UDC"), "udc0\n");
}

#[test]
fn what_an_earlier_setup_left_is_removed_before_the_gadget_is_made() {
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
    fs::create_dir_all(dir.join("functions/ncm.usb0")).unwrap();
    fs::write(dir.join("UDC"), "udc0\n").unwrap();

    // Bound, a tree that differs is not remade under its binding.
    let error = setup_with(&tree, own(), &[GadgetFunction::Ffs]).unwrap_err();
    assert_eq!(errno_of(&error), Some(libc::EBUSY), "{error}");
    assert!(dir.join("configs/c.7").is_dir(), "left as it was");
    fs::write(dir.join("UDC"), "\n").unwrap();

    let recorder = setup(&tree);
    assert_eq!(tree.entries("configs"), ["c.1"]);
    assert_eq!(tree.entries("strings"), ["0x409"]);
    assert!(
        dir.join("functions/ffs.udc0").is_dir(),
        "the link went, not what it led to"
    );
    assert!(
        !dir.join("functions/ncm.usb0").exists(),
        "a function left by an earlier run"
    );
    assert_eq!(tree.attribute("UDC"), "\n", "unbound before it was remade");
    assert!(
        recorder.unmounts.is_empty(),
        "a mount with ep0 in it is never unmounted"
    );
}

#[test]
fn a_set_linux_cannot_carry_is_refused_before_anything_is_touched() {
    let tree = Tree::new();
    let mut gadget = tree.gadget();
    assert_eq!(up(&mut gadget, &[1, 2, 3]), Err(ErrorKind::InvalidInput));

    // A fourth alternate setting: FunctionFS takes three.
    let mut body = function(64);
    for alternate in 1..=3 {
        body.extend_from_slice(&[9, 4, 0, alternate, 0, 0xff, 0, 0, 0]);
    }
    let alternates = [&DEVICE[..], &configuration(&body), &table()].concat();
    assert!(UsbDescriptors::parse(&alternates).is_ok());
    assert_eq!(up(&mut gadget, &alternates), Err(ErrorKind::Unsupported));

    // An endpoint address in two settings.
    let mut body = function(64);
    body.extend_from_slice(&[9, 4, 0, 1, 1, 0xff, 0, 0, 0]);
    body.extend_from_slice(&[7, 5, 0x81, 1, 0, 1, 1]);
    let twice = [&DEVICE[..], &configuration(&body), &table()].concat();
    assert!(UsbDescriptors::parse(&twice).is_ok());
    assert_eq!(up(&mut gadget, &twice), Err(ErrorKind::Unsupported));

    // No interface at all.
    let mut empty = configuration(&[]);
    empty[4] = 0;
    let empty = [&DEVICE[..], &empty, &table()].concat();
    assert_eq!(up(&mut gadget, &empty), Err(ErrorKind::Unsupported));

    // A product string configfs has no room for.
    let long = "x".repeat(126);
    let mut strings = vec![4, 3, 0x09, 0x04];
    for text in ["Maker", &long, "0001", "data", "main"] {
        strings.extend(string(text));
    }
    let long = [&DEVICE[..], &configuration(&function(64)), &strings].concat();
    assert_eq!(up(&mut gadget, &long), Err(ErrorKind::Unsupported));

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
    setup(&tree);
    for file in ["ep5", "ep3", "ep4"] {
        fs::write(tree.mount().join(file), "").unwrap();
    }
    let mut gadget = tree.gadget();
    // 1023 bytes a frame at full speed, three transactions of 1024 a microframe at high speed.
    up(&mut gadget, &streaming(1023, 1024 | 2 << 11)).unwrap();
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
    assert_eq!(fs::read(tree.mount().join("ep5")).unwrap(), interval);
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
    setup(&tree);
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
    for file in ["ep5", "ep3", "ep4"] {
        fs::write(tree.mount().join(file), "").unwrap();
    }
    let mut gadget = tree.gadget();
    up(&mut gadget, &set).unwrap();
    tree.event(ENABLE);
    gadget.next_event().unwrap();
    fs::write(tree.mount().join("ep5"), [5u8; 64]).unwrap();
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
    let descriptors = [&device[..], &config].concat();
    let mut recorder = Recorder::default();
    prepare(
        &tree.roots(),
        UDC,
        own(),
        &descriptors,
        &[GadgetFunction::Ffs],
        &mut recorder,
    )
    .unwrap();
    assert!(tree.entries("strings").is_empty());
    assert_eq!(
        tree.entries("configs/c.1"),
        ["MaxPower", "bmAttributes", "ffs.udc0"]
    );
    assert!(
        !tree.gadget_dir().join("max_speed").exists(),
        "an attribute the kernel lacks is not made"
    );
    let mut gadget = tree.gadget();
    up(&mut gadget, &descriptors).unwrap();
    let ep0 = fs::read(tree.mount().join("ep0")).unwrap();
    assert_eq!(
        ep0[8..16],
        [73, 0, 0, 0, 3, 0, 0, 0],
        "full speed only, with the OS sections"
    );
    assert_eq!(
        ep0[ep0.len() - 16..],
        [2, 0, 0, 0, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    );
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
    setup(&tree);
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
    up(&mut *function, &set()).unwrap();
    let gadget = sys.join("kernel/config/usb_gadget/lemnos-udc0");
    assert_eq!(fs::read_to_string(gadget.join("UDC")).unwrap(), "udc0\n");
    drop(function);
    eventually(|| fs::read_to_string(gadget.join("UDC")).unwrap() == "\n");
    assert!(board.usb_function("udc:udc0").is_ok());
}

#[test]
fn events_requests_and_endpoint_bytes_go_through_the_files() {
    let tree = Tree::new();
    setup(&tree);
    let mut gadget = tree.gadget();
    up(&mut gadget, &set()).unwrap();
    let mut data = [0u8; 8];
    assert_eq!(gadget.next_event(), Ok(None));
    assert_eq!(gadget.read(2, &mut data, WAIT), Err(ErrorKind::Unavailable));

    tree.event(BIND);
    tree.event(ENABLE);
    assert_eq!(gadget.next_event(), Ok(Some(UsbFunctionEvent::Configured)));

    // The host's bytes on the OUT endpoint, and ours on the IN endpoint.
    fs::write(tree.mount().join("ep2"), [1, 2, 3]).unwrap();
    assert_eq!(gadget.read(2, &mut data, WAIT), Ok(3));
    assert_eq!(data[..3], [1, 2, 3]);
    assert_eq!(gadget.write(0x81, &[4, 5], WAIT), Ok(2));
    assert_eq!(fs::read(tree.mount().join("ep1")).unwrap(), [4, 5]);
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
    settled(&gadget);
    fs::write(tree.mount().join("ep0"), "").unwrap();
    up(&mut gadget, &set()).unwrap();
    assert_eq!(gadget.next_event(), Ok(None));
}

#[test]
fn nothing_allocates_once_the_function_is_enabled() {
    let tree = Tree::new();
    setup(&tree);
    let mut gadget = tree.gadget();
    up(&mut gadget, &set()).unwrap();
    tree.event(ENABLE);
    tree.kernel_says(&[0xc0, 8, 0, 0, 0, 0, 2, 0, 4, 0, 0, 0]);
    fs::write(tree.mount().join("ep2"), [1, 2, 3]).unwrap();
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
    /// The errno the next call answers with, instead of what FunctionFS says on success. A data
    /// stage's is its end's, as FunctionFS's write or read of the stage answers it.
    fail: Option<i32>,
    /// The host ends no data stage until the test says so.
    holds: bool,
    /// A data stage was begun and not yet reported ended.
    under_way: bool,
    /// How it ended, once the host ended it.
    ended: Option<io::Result<usize>>,
    /// A request read from `ep0` is unanswered: until a stall or a data stage ends it.
    request_pending: bool,
}

impl FakeWire {
    /// The host ends the stage under way: all of `len`, or as the scripted failure says.
    fn end_stage(&mut self, len: usize) {
        if self.under_way && self.ended.is_none() {
            self.ended = Some(self.answer(len));
            self.request_pending = false;
        }
    }

    fn begin(&mut self, len: usize) {
        self.under_way = true;
        self.ended = None;
        if !self.holds {
            self.end_stage(len);
        }
    }
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

    fn begin_send(&mut self, data: &[u8]) -> io::Result<()> {
        self.did.push(Did::Send(data.to_vec()));
        self.begin(data.len());
        Ok(())
    }

    fn begin_receive(&mut self, len: usize) -> io::Result<()> {
        self.did.push(Did::Receive(len));
        self.begin(len);
        Ok(())
    }

    fn stage_end(&mut self, _into: &mut [u8], _timeout: Duration) -> Option<io::Result<usize>> {
        let done = self.ended.take()?;
        self.under_way = false;
        Some(done)
    }

    fn stage_under_way(&self) -> bool {
        self.under_way && self.ended.is_none()
    }

    fn stall(&mut self, request_is_in: bool) -> io::Result<()> {
        self.did.push(Did::Stall { request_is_in });
        self.request_pending = false;
        self.answer(())
    }

    fn request_pending(&self) -> io::Result<bool> {
        Ok(self.request_pending)
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

#[test]
fn a_request_the_daemon_before_read_and_did_not_answer_is_stalled_before_anything_is_served() {
    let mut session = session();
    session.wire.request_pending = true;
    let setup = request(0xc1, 64);
    let said = session.drain(Some(setup_bytes(&setup)));
    assert_eq!(
        session.wire.did,
        [Did::Stall {
            request_is_in: true
        }]
    );
    assert!(
        said[0].contains("did not answer was refused (stalled)"),
        "{said:?}"
    );
    assert!(said[0].contains("0xc1 0x01"), "{said:?}");
    // Nothing more to tell: the driver starts from the recorded state with nothing pending.
    assert_eq!(session.next_event(), Ok(None));
    // A request whose data comes from the host is stalled the other way.
    let mut session = session_out_pending();
    let said = session.drain(Some(setup_bytes(&request(0x41, 8))));
    assert_eq!(
        session.wire.did,
        [Did::Stall {
            request_is_in: false
        }]
    );
    assert_eq!(said.len(), 1);
}

fn session_out_pending() -> Session<FakeWire> {
    let mut session = session();
    session.wire.request_pending = true;
    session
}

#[test]
fn what_the_host_did_while_no_daemon_ran_is_taken_and_its_request_stalled() {
    let mut session = session();
    session.wire.events.extend([
        Event::Enable,
        Event::Suspend,
        Event::Setup(request(0xc1, 4)),
        Event::Resume,
    ]);
    let said = session.drain(None);
    assert!(session.configured && !session.suspended);
    assert_eq!(
        session.wire.did,
        [
            Did::Forget,
            Did::Stall {
                request_is_in: true
            }
        ]
    );
    assert!(
        said.iter()
            .any(|l| l.contains("the host made while no daemon ran was refused (stalled)")),
        "{said:?}"
    );
    assert!(
        said.iter()
            .any(|l| l.contains("3 event(s)") && l.contains("host configured")),
        "{said:?}"
    );
    // The request is not handed on: the driver is told nothing it would answer late.
    assert_eq!(session.next_event(), Ok(None));
}

#[test]
fn an_unrecorded_pending_request_is_ended_off_the_tick_and_nothing_is_read_until_it_has() {
    let mut session = session();
    session.wire.request_pending = true;
    session.wire.holds = true;
    session.wire.events.push_back(Event::Disable);
    let said = session.drain(None);
    assert_eq!(session.wire.did, [Did::Receive(STALE_READ)]);
    assert!(said[0].contains("did not record is pending"), "{said:?}");
    // While it runs, no event is read: a read now would be the request's data stage.
    assert_eq!(session.next_event(), Ok(None));
    assert_eq!(session.wire.events.len(), 1);
    // The host ends it; then the events come, and the end is nobody's answer.
    session.wire.end_stage(0);
    session.configured = true;
    assert_eq!(
        session.next_event(),
        Ok(Some(UsbFunctionEvent::Deconfigured))
    );
    assert!(session.stage.is_none());
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
    assert_eq!(session.control_in(&[1], NOW), Err(ErrorKind::InvalidInput));
    assert_eq!(
        session.control_out(&mut data, NOW),
        Err(ErrorKind::InvalidInput)
    );

    session
        .wire
        .events
        .push_back(Event::Setup(request(0xc0, 2)));
    session.next_event().unwrap();
    assert_eq!(
        session.control_out(&mut data, NOW),
        Err(ErrorKind::InvalidInput)
    );
    assert_eq!(session.control_in(&[1, 2, 3], NOW), Ok(2));
    assert_eq!(session.control_in(&[1], NOW), Err(ErrorKind::InvalidInput));

    session
        .wire
        .events
        .push_back(Event::Setup(request(0x40, 3)));
    session.next_event().unwrap();
    assert_eq!(session.control_in(&[1], NOW), Err(ErrorKind::InvalidInput));
    assert_eq!(
        session.control_out(&mut data[..2], NOW),
        Err(ErrorKind::InvalidInput)
    );
    assert_eq!(session.control_out(&mut data, NOW), Ok(3));

    session
        .wire
        .events
        .push_back(Event::Setup(request(0x40, 0)));
    session.next_event().unwrap();
    assert_eq!(session.control_out(&mut [], NOW), Ok(0));

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

/// FunctionFS waits in a data stage until the host ends it. The session begins it and is told
/// later: a host that holds it costs the stage, and nothing waits for it.
#[test]
fn a_data_stage_the_host_has_not_ended_goes_on_and_its_end_comes_later() {
    let mut session = session();
    session.wire.holds = true;
    let mut data = [0u8; 8];
    assert_eq!(session.control_end(&mut data), Err(ErrorKind::InvalidInput));

    session
        .wire
        .events
        .push_back(Event::Setup(request(0xc0, 2)));
    session.next_event().unwrap();
    session.wire.events.push_back(Event::Suspend);
    assert_eq!(session.control_in(&[1, 2, 3], NOW), Err(ErrorKind::Timeout));
    assert!(session.wire.stage_under_way());
    // While it is under way the request is answered, and nothing new is read.
    assert_eq!(session.control_in(&[1], NOW), Err(ErrorKind::InvalidInput));
    assert_eq!(session.control_stall(), Err(ErrorKind::InvalidInput));
    assert_eq!(session.next_event(), Ok(None));
    assert_eq!(session.control_end(&mut data), Ok(None));
    // The host takes it.
    session.wire.end_stage(2);
    assert_eq!(session.control_end(&mut data), Ok(Some(2)));
    assert_eq!(session.control_end(&mut data), Err(ErrorKind::InvalidInput));
    assert_eq!(session.next_event(), Ok(Some(UsbFunctionEvent::Suspended)));

    // A stage from the host: its bytes come with its end, into room for wLength.
    session
        .wire
        .events
        .push_back(Event::Setup(request(0x40, 3)));
    session.next_event().unwrap();
    assert_eq!(session.control_out(&mut data, NOW), Err(ErrorKind::Timeout));
    session.wire.end_stage(3);
    assert_eq!(
        session.control_end(&mut data[..2]),
        Err(ErrorKind::InvalidInput)
    );
    assert_eq!(session.control_end(&mut data), Ok(Some(3)));

    // A stage the host abandons (a new request, a reset) ends Unavailable.
    session
        .wire
        .events
        .push_back(Event::Setup(request(0xc0, 2)));
    session.next_event().unwrap();
    assert_eq!(session.control_in(&[1, 2], NOW), Err(ErrorKind::Timeout));
    session.wire.fail = Some(libc::ESHUTDOWN);
    session.wire.end_stage(2);
    assert_eq!(session.control_end(&mut data), Err(ErrorKind::Unavailable));
    assert_eq!(session.control_end(&mut data), Err(ErrorKind::InvalidInput));
    assert_eq!(
        session.wire.did,
        [
            Did::Send(vec![1, 2]),
            Did::Receive(3),
            Did::Send(vec![1, 2]),
        ]
    );
}

/// A stage that ended and was never asked about does not hold the next request up.
#[test]
fn an_ended_stage_nobody_asked_about_gives_way_to_the_next_request() {
    let mut session = session();
    session.wire.holds = true;
    session
        .wire
        .events
        .push_back(Event::Setup(request(0xc0, 2)));
    session.next_event().unwrap();
    assert_eq!(session.control_in(&[1, 2], NOW), Err(ErrorKind::Timeout));
    session.wire.end_stage(2);
    session
        .wire
        .events
        .push_back(Event::Setup(request(0xc0, 1)));
    assert!(matches!(
        session.next_event(),
        Ok(Some(UsbFunctionEvent::Setup { length: 1, .. }))
    ));
    session.wire.holds = false;
    assert_eq!(session.control_in(&[7], NOW), Ok(1));
    assert_eq!(session.control_end(&mut []), Err(ErrorKind::InvalidInput));
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
            session.control_in(&[1], NOW),
            Err(ErrorKind::Unavailable),
            "{errno}"
        );
        assert_eq!(session.control_in(&[1], NOW), Err(ErrorKind::InvalidInput));

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

/// The NCM function with the name `template`, as a board's `ifname=` gives it.
fn named_ncm(template: &str) -> GadgetFunction {
    use lemnos_hal::usb_descriptors::IfName;
    GadgetFunction::Ncm {
        host: [0x02, 0, 0, 0, 0, 0x01],
        device: [0x02, 0, 0, 0, 0, 0x02],
        ifname: IfName::new(template),
    }
}

#[test]
fn setup_writes_the_board_template_to_the_ncm_ifname_and_writes_none_without_one() {
    let tree = Tree::new();
    setup_with(&tree, own(), &[GadgetFunction::Ffs, named_ncm("lemnos%d")]).unwrap();
    assert_eq!(tree.attribute("functions/ncm.usb0/ifname"), "lemnos%d\n");

    let tree = Tree::new();
    setup_with(&tree, own(), &[GadgetFunction::Ffs, ncm()]).unwrap();
    assert!(!tree.gadget_dir().join("functions/ncm.usb0/ifname").exists());
}

#[test]
fn the_interface_name_is_read_back_once_bound_and_not_before() {
    let tree = Tree::new();
    setup_with(&tree, own(), &[GadgetFunction::Ffs, named_ncm("lemnos%d")]).unwrap();
    let mut gadget = tree.gadget();
    let mut buffer = [0u8; 16];
    assert_eq!(gadget.interface_name(&mut buffer), Ok(None), "not bound");

    // The kernel names the interface when the gadget binds; the stand-in tree says so itself.
    fs::write(
        tree.gadget_dir().join("functions/ncm.usb0/ifname"),
        "lemnos0\n",
    )
    .unwrap();
    up(&mut gadget, &set()).unwrap();
    assert_eq!(gadget.interface_name(&mut buffer), Ok(Some(7)));
    assert_eq!(&buffer[..7], b"lemnos0");
    assert_eq!(
        gadget.interface_name(&mut [0u8; 3]),
        Err(ErrorKind::InvalidInput),
        "a buffer too small for the name"
    );
    gadget.disable().unwrap();
    settled(&gadget);
    assert_eq!(gadget.interface_name(&mut buffer), Ok(None), "down again");
}

#[test]
fn a_kernel_that_takes_no_interface_name_refuses_the_board_by_name() {
    for code in [libc::ENOENT, libc::EPERM, libc::EINVAL, libc::EACCES] {
        let refused = ifname_refused(io::Error::from_raw_os_error(code));
        assert_eq!(refused.kind(), io::ErrorKind::Unsupported, "errno {code}");
        assert!(refused.to_string().contains("5.12"), "{refused}");
    }
    // Any other failure is what it was.
    let other = ifname_refused(io::Error::from_raw_os_error(libc::ENOSPC));
    assert_eq!(other.raw_os_error(), Some(libc::ENOSPC));
}

#[test]
fn a_functionfs_mount_already_there_is_reused_and_never_unmounted_or_remounted() {
    let tree = Tree::new();
    // The mount a holder or an earlier daemon left: ep0 is in it.
    let mount = tree.mounts_root.join(UDC);
    fs::create_dir_all(&mount).unwrap();
    fs::write(mount.join("ep0"), b"").unwrap();

    let recorder = setup(&tree);

    assert!(
        recorder.mounts.is_empty(),
        "no new mount: {:?}",
        recorder.mounts
    );
    assert!(
        recorder.unmounts.is_empty(),
        "no unmount: {:?}",
        recorder.unmounts
    );
    // The configuration is still linked to the function, which is kept.
    let config = tree.config_root.join("usb_gadget");
    assert!(config.exists());
}

#[test]
fn a_functionfs_mount_that_is_not_there_is_mounted_as_before() {
    let tree = Tree::new();
    // The fixture's mount has ep0 in it: without ep0 the mount is not there.
    fs::remove_file(tree.mount().join("ep0")).unwrap();
    let recorder = setup(&tree);
    assert_eq!(recorder.mounts.len(), 1);
    assert_eq!(recorder.unmounts.len(), 1);
}

#[test]
fn the_link_presents_its_microsoft_os_descriptors_on_every_gadget() {
    // configfs: the OS descriptors are on, with the vendor code and signature, and the
    // configuration is linked under `os_desc`, which is what makes the kernel answer for it.
    let tree = Tree::new();
    setup_with(&tree, own(), &[GadgetFunction::Ffs, ncm()]).unwrap();
    assert_eq!(tree.attribute("os_desc/use"), "1\n");
    assert_eq!(tree.attribute("os_desc/b_vendor_code"), "0x51\n");
    assert_eq!(tree.attribute("os_desc/qw_sign"), "MSFT100\n");
    assert!(
        fs::read_link(tree.gadget_dir().join("os_desc/c.1"))
            .unwrap()
            .ends_with("c.1")
    );
    // The NCM interface's compatible ID is written raw: six bytes, no newline.
    let ncm_id = tree
        .gadget_dir()
        .join("functions/ncm.usb0/os_desc/interface.ncm/compatible_id");
    assert_eq!(fs::read(ncm_id).unwrap(), b"WINNCM");

    // The blob ep0 takes: the OS flag, two sections counted before the descriptors, and the
    // link's GUID as UTF-8 with a NUL (the kernel converts it to UTF-16 for the host).
    let plan = Plan::of(&UsbDescriptors::parse(&set()).unwrap()).unwrap();
    let flags = u32::from_le_bytes(plan.descriptors[8..12].try_into().unwrap());
    assert_ne!(flags & functionfs::HAS_MS_OS_DESC, 0);
    assert_eq!(&plan.descriptors[20..24], &2u32.to_le_bytes());
    let guid = b"{CF13388F-BE34-4A1E-94C2-9AAFEF9A04E9}\0";
    assert!(plan.descriptors.windows(guid.len()).any(|w| w == guid));
    // The link's compatible ID section: WinUSB for interface 0 only.
    let winusb = b"WINUSB\0\0";
    assert!(plan.descriptors.windows(winusb.len()).any(|w| w == winusb));
}

#[test]
fn a_second_setup_takes_the_previous_os_link_down_first() {
    let tree = Tree::new();
    setup_with(&tree, own(), &[GadgetFunction::Ffs]).unwrap();
    setup_with(&tree, own(), &[GadgetFunction::Ffs]).unwrap();
    assert!(
        fs::read_link(tree.gadget_dir().join("os_desc/c.1"))
            .unwrap()
            .ends_with("c.1")
    );
    let links = fs::read_dir(tree.gadget_dir().join("os_desc"))
        .unwrap()
        .filter(|entry| entry.as_ref().unwrap().file_type().unwrap().is_symlink())
        .count();
    assert_eq!(links, 1, "one configuration under os_desc, not two");
}

/// A gadget helper that keeps root, in the test's stand-in tree: its setup is [`prepare_for_helper`],
/// its bind and unbind write `UDC` as the helper does. `failing` setups fail first with `ENOENT`,
/// as a helper whose step failed would answer. Every call is recorded in order.
struct Helped {
    configfs: PathBuf,
    mounts: PathBuf,
    udc: PathBuf,
    failing: u32,
    /// The errno each `bind` fails with, while set.
    bind_errno: Option<i32>,
    binds_without_network: bool,
    calls: Vec<&'static str>,
}

impl Helped {
    fn roots(&self) -> Roots<'_> {
        Roots {
            configfs: &self.configfs,
            mounts: &self.mounts,
            udc_class: &self.udc,
        }
    }
}

impl GadgetHelper for Helped {
    fn setup(&mut self, index: u32) -> io::Result<SerialString> {
        assert_eq!(index, 0);
        self.calls.push("setup");
        if self.failing > 0 {
            self.failing -= 1;
            return Err(io::Error::from_raw_os_error(libc::ENOENT));
        }
        let roots = self.roots();
        let owner = own();
        prepare_for_helper(
            &roots,
            UDC,
            owner,
            &set(),
            &[GadgetFunction::Ffs],
            &mut Recorder::default(),
        )?;
        // The FunctionFS mount is what makes `ep0`: the stand-in mount does not, so it is made here.
        fs::write(self.mounts.join(UDC).join("ep0"), "")?;
        Ok(SerialString::Absent)
    }

    fn bind(&mut self, index: u32) -> io::Result<()> {
        assert_eq!(index, 0);
        self.calls.push("bind");
        if self.binds_without_network {
            // As `Trees::bind` does once the kernel refused the network device for the boot, before
            // the bind without it, which may then fail on its own (the controller busy).
            let _ = fs::remove_dir(
                self.configfs
                    .join("usb_gadget/lemnos-udc0/functions/ncm.usb0"),
            );
        }
        if let Some(errno) = self.bind_errno {
            return Err(io::Error::from_raw_os_error(errno));
        }
        bind(&self.roots(), UDC)
    }

    fn unbind(&mut self, index: u32) -> io::Result<()> {
        assert_eq!(index, 0);
        self.calls.push("unbind");
        unbind(&self.roots(), UDC)
    }

    fn soft_connect(&mut self, index: u32, connect: bool) -> io::Result<()> {
        assert_eq!(index, 0);
        self.calls
            .push(if connect { "connect" } else { "disconnect" });
        Trees::new(&self.roots()).soft_connect(UDC, connect)
    }
}

/// The helper as the gadget holds it, and the shared handle the test reads its calls through.
fn helped(tree: &Tree, failing: u32) -> Arc<Mutex<Helped>> {
    Arc::new(Mutex::new(Helped {
        configfs: tree.config_root.clone(),
        mounts: tree.mounts_root.clone(),
        udc: tree.udc_root.clone(),
        failing,
        bind_errno: None,
        binds_without_network: false,
        calls: Vec::new(),
    }))
}

fn with_helper(tree: &Tree, helper: &Arc<Mutex<Helped>>) -> FunctionFsGadget {
    let shared: SharedHelper = helper.clone();
    tree.gadget().with_helper(shared, 0)
}

#[test]
fn with_a_helper_the_gadget_is_made_and_bound_through_it_and_the_daemon_writes_no_udc() {
    let tree = Tree::new();
    let helper = helped(&tree, 0);
    let mut gadget = with_helper(&tree, &helper);
    // The helper is asked to make it (here: adopt the stand-in's), then to bind, in that order;
    // a tree that was there before this daemon may be held by a host, so the bind is followed by
    // the pullup's pulse.
    up(&mut gadget, &set()).unwrap();
    assert_eq!(
        helper.lock().unwrap().calls,
        [
            "setup",
            "unbind",
            "bind",
            "disconnect",
            "connect",
            "unbind",
            "bind"
        ]
    );
    assert_eq!(tree.attribute("UDC"), "udc0\n");
    // The helper's setup leaves UDC as root's: the owner hand-over (mode 0600) is not made.
    let mode = fs::metadata(tree.gadget_dir().join("UDC"))
        .unwrap()
        .permissions()
        .mode();
    assert_ne!(mode & 0o777, 0o600);
    assert_eq!(up(&mut gadget, &set()), Err(ErrorKind::Busy));
}

/// A tree a daemon before this one left (a host may still hold it configured) is bound, then taken
/// off the bus by the controller's pullup for the gap and put back, then bound once more (a
/// controller `connect` restarts without the bind's steps may stay deaf), so the host enumerates it
/// again.
#[test]
fn a_gadget_a_host_may_hold_is_pulsed_off_the_bus_after_its_bind() {
    let tree = Tree::new();
    let first = helped(&tree, 0);
    let mut gadget = with_helper(&tree, &first);
    up(&mut gadget, &set()).unwrap();
    gone(gadget);
    // The next daemon finds the tree there: when it came off its controller is not known.
    let next = helped(&tree, 0);
    let mut gadget = with_helper(&tree, &next);
    up(&mut gadget, &set()).unwrap();
    assert_eq!(
        next.lock().unwrap().calls,
        [
            "setup",
            "unbind",
            "bind",
            "disconnect",
            "connect",
            "unbind",
            "bind"
        ]
    );
    assert_eq!(
        fs::read_to_string(tree.udc_root.join(UDC).join("soft_connect")).unwrap(),
        "connect"
    );
    assert_eq!(tree.attribute("UDC"), "udc0\n");
}

/// The controller's directory is found from the class entry's link, read and not followed: only
/// a link that climbs, then names plain entries down to the controller's own name, is taken.
#[test]
fn the_controllers_directory_is_found_only_through_a_link_of_the_kernels_shape() {
    use std::os::unix::fs::symlink;
    let root = std::env::temp_dir().join(format!("lemnos-udc-link-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let class = root.join("class/udc");
    let device = root.join("devices/platform/c.usb/udc/c.usb");
    fs::create_dir_all(&class).unwrap();
    fs::create_dir_all(&device).unwrap();
    fs::write(device.join("marker"), "here").unwrap();
    for target in ["elsewhere/late", "elsewhere/abs", "devices/gap"] {
        fs::create_dir_all(root.join(target)).unwrap();
    }
    symlink(
        "../../devices/platform/c.usb/udc/c.usb",
        class.join("c.usb"),
    )
    .unwrap();
    symlink(
        "../../devices/platform/c.usb/udc/other",
        class.join("named-wrong"),
    )
    .unwrap();
    // Each of these ends in its own entry's name, so only its shape refuses it.
    symlink("../../devices/../elsewhere/late", class.join("late")).unwrap();
    symlink(root.join("elsewhere/abs"), class.join("abs")).unwrap();
    symlink("../../devices//gap", class.join("gap")).unwrap();
    let dir = lemnos_linux_sys::confined::Dir::open_path(&class).unwrap();
    let found = super::tree::controller_dir(&dir, &class, "c.usb").unwrap();
    assert_eq!(found.read("marker").unwrap(), b"here");
    for name in ["named-wrong", "late", "abs", "gap"] {
        assert!(
            super::tree::controller_dir(&dir, &class, name).is_err(),
            "{name} is refused"
        );
    }
    assert_eq!(
        super::tree::controller_dir(&dir, &class, "missing")
            .err()
            .and_then(|e| e.raw_os_error()),
        Some(libc::ENOENT)
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_controller_absent_at_start_is_retried_and_comes_up_without_a_restart() {
    let tree = Tree::new();
    let helper = helped(&tree, 0);
    let mut gadget = with_helper(&tree, &helper);
    // UDC not there yet (the controller is not bound, or is in host role): the helper's setup
    // fails with the missing controller, and nothing is bound.
    let udc = tree.udc_root.join(UDC);
    fs::remove_dir_all(&udc).unwrap();
    assert_eq!(up(&mut gadget, &set()), Err(ErrorKind::NotFound));
    assert_eq!(helper.lock().unwrap().calls, ["setup"]);
    // The controller appears (what its uevent says): the next attempt, by the same gadget and
    // the same helper, brings it up. No restart of anything.
    fs::create_dir_all(&udc).unwrap();
    up(&mut gadget, &set()).unwrap();
    assert_eq!(
        helper.lock().unwrap().calls,
        ["setup", "setup", "unbind", "bind"]
    );
    assert_eq!(tree.attribute("UDC"), "udc0\n");
}

#[test]
fn a_setup_that_fails_n_times_then_succeeds_is_retried_each_time() {
    let tree = Tree::new();
    let helper = helped(&tree, 3);
    let mut gadget = with_helper(&tree, &helper);
    for attempt in 0..3 {
        assert_eq!(
            up(&mut gadget, &set()),
            Err(ErrorKind::NotFound),
            "attempt {attempt} fails"
        );
    }
    up(&mut gadget, &set()).unwrap();
    assert_eq!(
        helper.lock().unwrap().calls,
        ["setup", "setup", "setup", "setup", "unbind", "bind"]
    );
}

#[test]
fn teardown_then_setup_again_makes_the_gadget_again_and_binds_it() {
    let tree = Tree::new();
    let helper = helped(&tree, 0);
    let mut gadget = with_helper(&tree, &helper);
    up(&mut gadget, &set()).unwrap();
    gadget.disable().unwrap();
    settled(&gadget);
    assert_eq!(
        helper.lock().unwrap().calls,
        [
            "setup",
            "unbind",
            "bind",
            "disconnect",
            "connect",
            "unbind",
            "bind",
            "unbind"
        ]
    );
    // unbind wrote the empty line: nothing is bound.
    assert_eq!(tree.attribute("UDC").trim(), "");
    // The helper takes the gadget down for good, then makes it again on the next enable.
    teardown(&tree.roots(), UDC, &mut Recorder::default()).unwrap();
    assert!(!tree.gadget_dir().join("functions/ffs.udc0").exists());
    // The stand-in unmount does nothing, so the mount's ep0 stays; a real unmount takes it with
    // the mount. Remove it as the unmount would, so the setup makes the mount again.
    fs::remove_file(tree.mount().join("ep0")).unwrap();
    up(&mut gadget, &set()).unwrap();
    assert_eq!(
        helper.lock().unwrap().calls,
        [
            "setup",
            "unbind",
            "bind",
            "disconnect",
            "connect",
            "unbind",
            "bind",
            "unbind",
            "setup",
            "unbind",
            "bind",
            "disconnect",
            "connect",
            "unbind",
            "bind"
        ]
    );
}

#[test]
fn the_controller_name_is_one_directory_name_for_the_helper() {
    // The controller name is one directory name: a name with a separator is never a path the
    // helper writes under.
    assert!(is_controller_name("fe980000.usb"));
    assert!(!is_controller_name("../x"));
    assert!(!is_controller_name("a/b"));
    assert!(!is_controller_name(""));
}

/// What a helper's setup does on `trees`, with the functions the link needs.
fn helper_setup(trees: &mut Trees, recorder: &mut Recorder) -> io::Result<Outcome> {
    trees.setup_for_helper(UDC, own(), &set(), &[GadgetFunction::Ffs], recorder)
}

#[test]
fn a_tree_that_is_the_boards_is_adopted_bound_or_not_and_never_mounted_again() {
    let tree = Tree::new();
    let mut trees = Trees::new(&tree.roots());
    let mut recorder = Recorder::default();
    // The image left a bare gadget: not the board's, unbound, so it is made.
    assert_eq!(
        helper_setup(&mut trees, &mut recorder).unwrap(),
        Outcome::Remade
    );
    // A helper that starts again finds its own tree and takes it as it is.
    let mut again = Trees::new(&tree.roots());
    assert_eq!(
        helper_setup(&mut again, &mut recorder).unwrap(),
        Outcome::Adopted
    );
    fs::write(tree.gadget_dir().join("UDC"), "udc0\n").unwrap();
    assert_eq!(
        helper_setup(&mut again, &mut recorder).unwrap(),
        Outcome::Adopted
    );
    assert_eq!(
        tree.attribute("UDC"),
        "udc0\n",
        "a bound gadget stays bound"
    );
    assert!(recorder.mounts.is_empty() && recorder.unmounts.is_empty());
}

/// The test set with no serial string: the device descriptor's `iSerialNumber` is 0.
fn set_without_serial() -> Vec<u8> {
    let mut bytes = set();
    bytes[16] = 0;
    bytes
}

#[test]
fn the_serial_a_setup_reports_is_the_trees_own_made_or_adopted() {
    // Made with a serial: the tree has it.
    let tree = Tree::new();
    let mut trees = Trees::new(&tree.roots());
    let mut recorder = Recorder::default();
    helper_setup(&mut trees, &mut recorder).unwrap();
    assert_eq!(trees.serial_string(UDC).unwrap(), SerialString::Set);
    // Adopted: the same tree is there, and a setup that now names no serial finds the tree's own
    // string: no stale "no serial".
    let mut again = Trees::new(&tree.roots());
    let outcome = again
        .setup_for_helper(
            UDC,
            own(),
            &set_without_serial(),
            &[GadgetFunction::Ffs],
            &mut recorder,
        )
        .unwrap();
    assert_eq!(outcome, Outcome::Adopted);
    assert_eq!(again.serial_string(UDC).unwrap(), SerialString::Set);
    // Made with no serial: the tree has none.
    let bare = Tree::new();
    let mut trees = Trees::new(&bare.roots());
    trees
        .setup_for_helper(
            UDC,
            own(),
            &set_without_serial(),
            &[GadgetFunction::Ffs],
            &mut recorder,
        )
        .unwrap();
    assert_eq!(trees.serial_string(UDC).unwrap(), SerialString::Absent);
    // An empty serial file is none.
    fs::create_dir_all(bare.gadget_dir().join("strings/0x409")).unwrap();
    fs::write(bare.gadget_dir().join("strings/0x409/serialnumber"), "\n").unwrap();
    assert_eq!(trees.serial_string(UDC).unwrap(), SerialString::Absent);
}

#[test]
fn a_tree_of_the_same_name_with_other_content_is_remade_only_while_unbound() {
    let tree = Tree::new();
    let mut trees = Trees::new(&tree.roots());
    let mut recorder = Recorder::default();
    helper_setup(&mut trees, &mut recorder).unwrap();
    fs::write(tree.gadget_dir().join("idVendor"), "0x1d6b\n").unwrap();
    fs::write(tree.gadget_dir().join("UDC"), "udc0\n").unwrap();
    let error = helper_setup(&mut trees, &mut recorder).unwrap_err();
    assert_eq!(errno_of(&error), Some(libc::EBUSY), "{error}");
    assert_eq!(tree.attribute("idVendor"), "0x1d6b\n", "left as it was");
    fs::write(tree.gadget_dir().join("UDC"), "\n").unwrap();
    assert_eq!(
        helper_setup(&mut trees, &mut recorder).unwrap(),
        Outcome::Remade
    );
    assert_ne!(tree.attribute("idVendor"), "0x1d6b\n");
}

#[test]
fn a_tree_the_helper_did_not_make_is_left_alone_and_named() {
    let tree = Tree::new();
    fs::create_dir_all(tree.gadget_dir().join("functions/mass_storage.0")).unwrap();
    let mut trees = Trees::new(&tree.roots());
    let mut recorder = Recorder::default();
    let error = helper_setup(&mut trees, &mut recorder).unwrap_err();
    assert_eq!(errno_of(&error), Some(libc::EEXIST), "{error}");
    let text = error.to_string();
    assert!(
        text.contains("mass_storage.0") && text.contains("left alone"),
        "{text}"
    );
    assert!(tree.is_bare(), "nothing was made or removed");
    assert!(tree.gadget_dir().join("functions/mass_storage.0").is_dir());
    // A configuration of another name is foreign too.
    fs::remove_dir(tree.gadget_dir().join("functions/mass_storage.0")).unwrap();
    fs::create_dir_all(tree.gadget_dir().join("configs/mine")).unwrap();
    let error = helper_setup(&mut trees, &mut recorder).unwrap_err();
    assert_eq!(errno_of(&error), Some(libc::EEXIST), "{error}");
    assert!(tree.gadget_dir().join("configs/mine").is_dir());
}

#[test]
fn nothing_is_followed_out_of_the_directories_the_helper_opened() {
    // `usb_gadget` is a link to somewhere else: the setup is refused and nothing lands there.
    let tree = Tree::new();
    let outside = tree.root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::remove_dir_all(tree.config_root.join("usb_gadget")).unwrap();
    std::os::unix::fs::symlink(&outside, tree.config_root.join("usb_gadget")).unwrap();
    let mut trees = Trees::new(&tree.roots());
    let mut recorder = Recorder::default();
    let error = helper_setup(&mut trees, &mut recorder).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ELOOP), "{error}");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    // The gadget directory itself a link: refused the same way, and its target is untouched.
    let tree = Tree::new();
    let elsewhere = tree.root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::remove_dir_all(tree.gadget_dir()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, tree.gadget_dir()).unwrap();
    let error = helper_setup(&mut Trees::new(&tree.roots()), &mut recorder).unwrap_err();
    assert_eq!(errno_of(&error), Some(libc::EEXIST), "{error}");
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    // Names that are paths never reach the kernel.
    for name in ["..", "../x", "a/b", "/etc", ""] {
        let mut trees = Trees::new(&tree.roots());
        assert!(trees.bind(name).is_err(), "{name:?}");
        assert!(trees.unbind(name).is_err(), "{name:?}");
        assert!(trees.teardown(name, &mut recorder).is_err(), "{name:?}");
        assert!(
            trees
                .setup_for_helper(name, own(), &set(), &[GadgetFunction::Ffs], &mut recorder)
                .is_err(),
            "{name:?}"
        );
    }
}

#[test]
fn a_configfs_directory_that_is_not_a_mounted_configfs_is_not_there_yet() {
    let tree = Tree::new();
    let mut trees = Trees::new(&tree.roots()).verifying_configfs();
    let error = trees.open().unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ENOENT), "{error}");
    let error = helper_setup(&mut trees, &mut Recorder::default()).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ENOENT), "{error}");
}

/// A `udc:` name that is the controller's platform device (`4100000.usb`) resolves to the controller's own entry
/// in `/sys/class/udc`, whatever its index is; a name of an entry is itself; a name of no device, or of two, is none.
#[test]
fn a_platform_device_name_resolves_to_its_controller_entry() {
    let root = std::env::temp_dir().join(format!("lemnos-udc-resolve-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let class = root.join("class/udc");
    fs::create_dir_all(&class).unwrap();
    // The kernel's layout: the controller under its platform device, and the class entry a symlink to it.
    let device =
        root.join("devices/platform/soc/4100000.usb/musb-hdrc.1.auto/udc/musb-hdrc.1.auto");
    fs::create_dir_all(&device).unwrap();
    std::os::unix::fs::symlink(&device, class.join("musb-hdrc.1.auto")).unwrap();
    assert_eq!(
        resolve_udc(&class, "4100000.usb").as_deref(),
        Some("musb-hdrc.1.auto"),
        "the platform device's name finds the entry"
    );
    assert_eq!(
        resolve_udc(&class, "musb-hdrc.1.auto").as_deref(),
        Some("musb-hdrc.1.auto"),
        "an entry's own name is itself"
    );
    assert_eq!(
        resolve_udc(&class, "4100001.usb"),
        None,
        "no such device: none"
    );
    // A second controller under the same device makes the name ambiguous: none.
    let second =
        root.join("devices/platform/soc/4100000.usb/musb-hdrc.2.auto/udc/musb-hdrc.2.auto");
    fs::create_dir_all(&second).unwrap();
    std::os::unix::fs::symlink(&second, class.join("musb-hdrc.2.auto")).unwrap();
    assert_eq!(
        resolve_udc(&class, "4100000.usb"),
        None,
        "two entries: none"
    );
    let _ = fs::remove_dir_all(&root);
}

/// A daemon started again in a fresh mount namespace (each start gets its own) can't see the mount
/// the last one made. While a holder keeps that instance's endpoints, f_fs refuses a second mount
/// (EBUSY): the tree is left as it is, bound, and the setup says why. Once the holder lets go the
/// mount is taken here and the tree is the same one: adopted, not remade.
#[test]
fn a_held_functionfs_instance_another_namespace_mounted_is_left_alone_until_let_go() {
    let tree = Tree::new();
    let mut trees = Trees::new(&tree.roots());
    let mut recorder = Recorder::default();
    helper_setup(&mut trees, &mut recorder).unwrap();
    fs::write(tree.gadget_dir().join("UDC"), "udc0\n").unwrap();
    // The mount went with the last daemon's namespace.
    let _ = fs::remove_file(tree.mount().join("ep0"));
    let mut held = Recorder {
        busy: 1,
        ..Recorder::default()
    };
    let mut again = Trees::new(&tree.roots());
    let error = helper_setup(&mut again, &mut held).unwrap_err();
    assert_eq!(errno_of(&error), Some(libc::EBUSY), "{error}");
    assert!(error.to_string().contains("still held"), "{error}");
    assert_eq!(tree.attribute("UDC"), "udc0\n", "still bound");
    assert!(held.unmounts.is_empty());
    let mut free = Recorder::default();
    assert_eq!(
        helper_setup(&mut again, &mut free).unwrap(),
        Outcome::Adopted
    );
    assert_eq!(free.mounts.len(), 1, "mounted here once: {:?}", free.mounts);
    assert!(free.unmounts.is_empty());
}

/// A holder that never lets go does not keep the board off USB: past the limit the tree is unbound,
/// taken down and made again on a fresh instance, and the setup says it took over.
#[test]
fn a_functionfs_instance_held_past_the_limit_is_taken_over() {
    let tree = Tree::new();
    let mut trees = Trees::new(&tree.roots());
    let mut recorder = Recorder::default();
    helper_setup(&mut trees, &mut recorder).unwrap();
    fs::write(tree.gadget_dir().join("UDC"), "udc0\n").unwrap();
    let _ = fs::remove_file(tree.mount().join("ep0"));
    // Within the limit: left alone.
    let mut waiting = Trees::new(&tree.roots());
    let mut held = Recorder {
        busy: 1,
        ..Recorder::default()
    };
    let error = helper_setup(&mut waiting, &mut held).unwrap_err();
    assert_eq!(errno_of(&error), Some(libc::EBUSY), "{error}");
    assert!(error.to_string().contains("then taken over"), "{error}");
    assert_eq!(tree.attribute("UDC"), "udc0\n");
    // Past it: the held instance's probe is refused once more, and then a fresh one is made.
    let mut past = Trees::new(&tree.roots()).with_held_limit(std::time::Duration::ZERO);
    let mut held = Recorder {
        busy: 1,
        ..Recorder::default()
    };
    assert_eq!(
        helper_setup(&mut past, &mut held).unwrap(),
        Outcome::TakenOver
    );
    assert_ne!(tree.attribute("UDC"), "udc0\n", "unbound");
    assert_eq!(
        held.mounts.len(),
        1,
        "a fresh instance mounted: {:?}",
        held.mounts
    );
}

/// A test kernel that refuses every bind while the tree has a network function: the state after
/// the controller's device came back.
fn refuses_network(gadget: &Dir) -> bool {
    gadget
        .open_dir("functions")
        .is_ok_and(|functions| functions.exists("ncm.usb0"))
}

/// A test kernel that refuses every bind.
fn refuses_all(_: &Dir) -> bool {
    true
}

static REFUSED_ONCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A test kernel that refuses the first bind only.
fn refuses_once(_: &Dir) -> bool {
    !REFUSED_ONCE.swap(true, std::sync::atomic::Ordering::SeqCst)
}

/// The network function marked, so a fresh one is told from it (a fresh one has no mark).
fn mark_network(tree: &Tree) {
    fs::write(tree.gadget_dir().join("functions/ncm.usb0/seen"), "").unwrap();
}

fn network_marked(tree: &Tree) -> bool {
    tree.gadget_dir().join("functions/ncm.usb0/seen").exists()
}

/// Every function of the tree is linked in its configuration: the network one too when `network`.
fn linked(tree: &Tree, network: bool) -> bool {
    let config = tree.gadget_dir().join("configs/c.1");
    let link = |name: &str| fs::symlink_metadata(config.join(name)).is_ok();
    link(&format!("ffs.{UDC}")) && link("acm.GS0") && link("ncm.usb0") == network
}

fn network_setup(trees: &mut Trees, recorder: &mut Recorder) -> io::Result<Outcome> {
    let functions = [GadgetFunction::Ffs, ncm(), GadgetFunction::Acm];
    trees.setup_for_helper(UDC, own(), &set(), &functions, recorder)
}

/// A remake keeps a network function that is the board file's: the kernel's network device goes
/// only with the function's directory, so the interface keeps its name and index. One whose
/// addresses differ from the board file's is made anew.
#[test]
fn a_remake_keeps_the_boards_network_function_and_renews_only_one_that_differs() {
    let tree = Tree::new();
    let mut trees = Trees::new(&tree.roots());
    let mut recorder = Recorder::default();
    network_setup(&mut trees, &mut recorder).unwrap();
    mark_network(&tree);
    // Something else of the tree differs, unbound: remade, the network function kept and linked.
    fs::write(tree.gadget_dir().join("idVendor"), "0x1d6b\n").unwrap();
    assert_eq!(
        network_setup(&mut trees, &mut recorder).unwrap(),
        Outcome::Remade
    );
    assert!(network_marked(&tree), "the network function was kept");
    assert!(linked(&tree, true));
    assert_ne!(
        tree.attribute("idVendor"),
        "0x1d6b\n",
        "the rest made again"
    );
    // Its addresses differ from the board file's: a new function, a new device.
    fs::write(
        tree.gadget_dir().join("functions/ncm.usb0/host_addr"),
        "02:00:00:00:00:99\n",
    )
    .unwrap();
    assert_eq!(
        network_setup(&mut trees, &mut recorder).unwrap(),
        Outcome::Remade
    );
    assert!(
        !network_marked(&tree),
        "a network function that differs is made anew"
    );
    assert!(linked(&tree, true));
}

/// A takeover (a FunctionFS instance held past the limit) makes the FunctionFS function again and
/// keeps the board's network function, so the interface stays what it was.
#[test]
fn a_takeover_keeps_the_boards_network_function() {
    let tree = Tree::new();
    let mut trees = Trees::new(&tree.roots());
    let mut recorder = Recorder::default();
    network_setup(&mut trees, &mut recorder).unwrap();
    mark_network(&tree);
    fs::write(tree.gadget_dir().join("UDC"), "udc0\n").unwrap();
    let _ = fs::remove_file(tree.mount().join("ep0"));
    let mut past = Trees::new(&tree.roots()).with_held_limit(std::time::Duration::ZERO);
    let mut held = Recorder {
        busy: 1,
        ..Recorder::default()
    };
    assert_eq!(
        network_setup(&mut past, &mut held).unwrap(),
        Outcome::TakenOver
    );
    assert!(network_marked(&tree), "the network function was kept");
    assert!(linked(&tree, true));
    assert_eq!(held.mounts.len(), 1, "a fresh instance mounted");
}

#[test]
fn a_bind_the_kernel_refuses_with_a_network_function_is_tried_on_a_fresh_one_then_without() {
    let tree = Tree::new();
    let mut trees = Trees::new(&tree.roots());
    let mut recorder = Recorder::default();
    network_setup(&mut trees, &mut recorder).unwrap();
    mark_network(&tree);
    assert!(linked(&tree, true));
    trees.refuses = Some(refuses_network);
    // Refused with the function, and refused again on a fresh one: the gadget is bound without
    // it, the rest linked as before.
    trees.bind(UDC).unwrap();
    assert_eq!(tree.attribute("UDC"), "udc0\n");
    assert!(!tree.gadget_dir().join("functions/ncm.usb0").exists());
    assert!(linked(&tree, false));
    trees.unbind(UDC).unwrap();
    // The setup after makes the network function again, as the board file has it, and every
    // bind after takes it out before the write: the kernel never sees it.
    assert_eq!(
        network_setup(&mut trees, &mut recorder).unwrap(),
        Outcome::Remade
    );
    assert!(linked(&tree, true));
    trees.refuses = Some(refuses_all);
    trees.bind(UDC).unwrap_err();
    assert!(!tree.gadget_dir().join("functions/ncm.usb0").exists());
    trees.refuses = None;
    network_setup(&mut trees, &mut recorder).unwrap();
    trees.bind(UDC).unwrap();
    assert!(linked(&tree, false));
}

#[test]
fn a_bind_refused_once_works_on_a_fresh_network_function() {
    let tree = Tree::new();
    let mut trees = Trees::new(&tree.roots());
    let mut recorder = Recorder::default();
    network_setup(&mut trees, &mut recorder).unwrap();
    mark_network(&tree);
    trees.refuses = Some(refuses_once);
    trees.bind(UDC).unwrap();
    assert_eq!(tree.attribute("UDC"), "udc0\n");
    assert!(tree.gadget_dir().join("functions/ncm.usb0").is_dir());
    assert!(!network_marked(&tree), "a fresh function, never bound");
    assert!(linked(&tree, true));
    // The tree is the board file's: adopted.
    trees.unbind(UDC).unwrap();
    trees.refuses = None;
    assert_eq!(
        network_setup(&mut trees, &mut recorder).unwrap(),
        Outcome::Adopted
    );
}

#[test]
fn a_bind_refused_with_and_without_the_network_function_leaves_a_fresh_one_unbound() {
    let tree = Tree::new();
    let mut trees = Trees::new(&tree.roots());
    let mut recorder = Recorder::default();
    network_setup(&mut trees, &mut recorder).unwrap();
    mark_network(&tree);
    trees.refuses = Some(refuses_all);
    assert_eq!(errno_of(&trees.bind(UDC).unwrap_err()), Some(libc::EBUSY));
    assert_ne!(tree.attribute("UDC"), "udc0\n");
    assert!(tree.gadget_dir().join("functions/ncm.usb0").is_dir());
    assert!(!network_marked(&tree));
    assert!(linked(&tree, true));
    // Not the network function's doing: the next bind that works has it.
    mark_network(&tree);
    trees.refuses = Some(refuses_all);
    trees.bind(UDC).unwrap_err();
    assert!(
        !network_marked(&tree),
        "the function a bind failed on is never bound again"
    );
    trees.refuses = None;
    trees.bind(UDC).unwrap();
    assert!(linked(&tree, true));
}

#[test]
fn a_failed_bind_of_a_gadget_with_no_network_function_leaves_its_tree_as_it_is() {
    let tree = Tree::new();
    let mut trees = Trees::new(&tree.roots());
    let mut recorder = Recorder::default();
    helper_setup(&mut trees, &mut recorder).unwrap();
    let before = tree.entries("functions");
    trees.refuses = Some(refuses_all);
    trees.bind(UDC).unwrap_err();
    trees.bind(UDC).unwrap_err();
    assert_eq!(tree.entries("functions"), before);
    trees.refuses = None;
    assert_eq!(
        helper_setup(&mut trees, &mut recorder).unwrap(),
        Outcome::Adopted
    );
}

#[test]
fn a_gadget_bound_without_its_network_function_says_so_and_keeps_its_session_through_a_let_go() {
    let tree = Tree::new();
    let helper = helped(&tree, 0);
    let mut gadget = with_helper(&tree, &helper);
    // A refused bind is its kind, with no note.
    helper.lock().unwrap().bind_errno = Some(libc::EBUSY);
    assert_eq!(up(&mut gadget, &set()), Err(ErrorKind::Busy));
    assert_eq!(gadget.enable_note(), None);
    helper.lock().unwrap().bind_errno = None;
    up(&mut gadget, &set()).unwrap();
    assert_eq!(gadget.enable_note(), None);

    // The controller let go; the helper's bind works without the network function: the note
    // says so, and the session the holder has stays.
    fs::write(tree.gadget_dir().join("UDC"), "\n").unwrap();
    gadget.session.as_mut().unwrap().released = true;
    gadget.disable().unwrap();
    settled(&gadget);
    let network = tree.gadget_dir().join("functions/ncm.usb0");
    fs::create_dir(&network).unwrap();
    helper.lock().unwrap().binds_without_network = true;
    helper.lock().unwrap().calls.clear();
    up(&mut gadget, &set()).unwrap();
    assert_eq!(helper.lock().unwrap().calls, ["bind"]);
    assert_eq!(gadget.enable_note(), Some(NETWORK_REFUSED));
    assert!(gadget.session.as_ref().is_some_and(|s| !s.released));
    // A failed bind on a let-go keeps the session too.
    fs::write(tree.gadget_dir().join("UDC"), "\n").unwrap();
    gadget.session.as_mut().unwrap().released = true;
    gadget.disable().unwrap();
    settled(&gadget);
    helper.lock().unwrap().bind_errno = Some(libc::EBUSY);
    assert_eq!(up(&mut gadget, &set()), Err(ErrorKind::Busy));
    assert!(gadget.session.is_some());
}

#[test]
fn a_network_function_lost_in_an_attempt_that_failed_is_still_named_by_the_bind_after_it() {
    let tree = Tree::new();
    let helper = helped(&tree, 0);
    let mut gadget = with_helper(&tree, &helper);
    up(&mut gadget, &set()).unwrap();
    assert_eq!(gadget.enable_note(), None);
    // The controller let go. The first bind after it loses the network function (the kernel
    // refused it) and then fails busy; the next one binds without it, and the note must say so.
    fs::write(tree.gadget_dir().join("UDC"), "\n").unwrap();
    gadget.session.as_mut().unwrap().released = true;
    gadget.disable().unwrap();
    settled(&gadget);
    fs::create_dir(tree.gadget_dir().join("functions/ncm.usb0")).unwrap();
    helper.lock().unwrap().binds_without_network = true;
    helper.lock().unwrap().bind_errno = Some(libc::EBUSY);
    assert_eq!(up(&mut gadget, &set()), Err(ErrorKind::Busy));
    assert!(!tree.gadget_dir().join("functions/ncm.usb0").exists());
    helper.lock().unwrap().bind_errno = None;
    up(&mut gadget, &set()).unwrap();
    assert_eq!(gadget.enable_note(), Some(NETWORK_REFUSED));
}

// ---- A session parked for the next function on the controller (`UsbFunction::park`) ----

fn parked_gadget(tree: &Tree, helper: &Arc<Mutex<Helped>>, parking: &Parking) -> FunctionFsGadget {
    with_helper(tree, helper).parking(parking.clone())
}

#[test]
fn a_parked_session_is_taken_up_by_the_next_function_with_nothing_written_and_its_state_told_again()
{
    let tree = Tree::new();
    let helper = helped(&tree, 0);
    let parking = Parking::default();
    let mut old = parked_gadget(&tree, &helper, &parking);
    up(&mut old, &set()).unwrap();
    {
        let session = old.session.as_mut().unwrap();
        session.configured = true;
        session.suspended = true;
    }
    assert!(old.park());
    assert!(old.session.is_none());
    assert!(!old.park(), "parked once");
    helper.lock().unwrap().calls.clear();

    let mut new = parked_gadget(&tree, &helper, &parking);
    up(&mut new, &set()).unwrap();
    assert_eq!(new.took_up(), Some(TookUp::Adopted));
    assert!(
        helper.lock().unwrap().calls.is_empty(),
        "nothing set up, bound or unbound"
    );
    assert_eq!(new.next_event(), Ok(Some(UsbFunctionEvent::Configured)));
    assert_eq!(new.next_event(), Ok(Some(UsbFunctionEvent::Suspended)));
    // The old function's drop leaves the session the new one took up.
    gone(old);
    assert!(helper.lock().unwrap().calls.is_empty());
    assert!(new.session.is_some());
}

#[test]
fn a_parked_session_with_other_descriptors_is_taken_down_and_the_next_function_starts_fresh() {
    let tree = Tree::new();
    let helper = helped(&tree, 0);
    let parking = Parking::default();
    let mut old = parked_gadget(&tree, &helper, &parking);
    up(&mut old, &set()).unwrap();
    assert!(old.park());
    helper.lock().unwrap().calls.clear();

    let mut new = parked_gadget(&tree, &helper, &parking);
    up(&mut new, &set_without_serial()).unwrap();
    assert_eq!(new.took_up(), Some(TookUp::Replaced));
    let calls = helper.lock().unwrap().calls.clone();
    assert_eq!(
        calls.first(),
        Some(&"unbind"),
        "taken down first: {calls:?}"
    );
    assert!(
        calls.contains(&"setup") && calls.contains(&"bind"),
        "{calls:?}"
    );
    gone(old);
    assert!(new.session.is_some());
}

#[test]
fn a_parked_session_nothing_took_up_is_taken_down_by_the_drop_of_the_function_that_parked_it() {
    let tree = Tree::new();
    let helper = helped(&tree, 0);
    let parking = Parking::default();
    let mut old = parked_gadget(&tree, &helper, &parking);
    up(&mut old, &set()).unwrap();
    assert!(old.park());
    helper.lock().unwrap().calls.clear();
    gone(old);
    assert_eq!(helper.lock().unwrap().calls, ["unbind"], "never orphaned");
    // Nothing is left to take up: the next function enables as any does.
    let mut new = parked_gadget(&tree, &helper, &parking);
    up(&mut new, &set()).unwrap();
    assert_eq!(new.took_up(), None);
}

#[test]
fn a_function_with_no_session_or_one_the_controller_let_go_parks_nothing() {
    let tree = Tree::new();
    let helper = helped(&tree, 0);
    let parking = Parking::default();
    let mut gadget = parked_gadget(&tree, &helper, &parking);
    assert!(!gadget.park(), "not enabled");
    up(&mut gadget, &set()).unwrap();
    gadget.session.as_mut().unwrap().released = true;
    assert!(!gadget.park(), "let go by the controller");
    // Without the daemon's parking, nothing is parked either.
    let mut alone = with_helper(&tree, &helper);
    fs::write(tree.gadget_dir().join("UDC"), "\n").unwrap();
    gone(gadget);
    up(&mut alone, &set()).unwrap();
    assert!(!alone.park());
}

/// GADGET-REBIND-VISIBLE: the helper's trees tell each bind that follows a let-go, with the way
/// the gadget was let go and how long `UDC` had been clear. A tree made fresh, unbound though
/// never bound (as every enable does), is not a let-go. The wait itself is the daemon's.
#[test]
fn the_trees_tell_a_bind_after_a_let_go_and_how_long_udc_was_clear() {
    let tree = Tree::new();
    let mut trees = Trees::new(&tree.roots());
    let mut recorder = Recorder::default();
    fs::remove_dir_all(tree.gadget_dir()).unwrap();
    assert_eq!(
        helper_setup(&mut trees, &mut recorder).unwrap(),
        Outcome::Made
    );
    trees.unbind(UDC).unwrap();
    trees.bind(UDC).unwrap();
    assert_eq!(
        trees.take_rebind(UDC),
        None,
        "a fresh tree has no host to tell"
    );

    // Bound, unbound, and bound again 30 ms later.
    trees.unbind(UDC).unwrap();
    assert_eq!(tree.attribute("UDC").trim(), "");
    std::thread::sleep(std::time::Duration::from_millis(30));
    trees.bind(UDC).unwrap();
    let rebind = trees.take_rebind(UDC).expect("the rebind is told");
    assert_eq!(rebind.path, "unbound");
    assert!(rebind.clear_for >= std::time::Duration::from_millis(30));
    assert_eq!(trees.take_rebind(UDC), None, "told once");

    // The next daemon's helper finds a tree the last one made, still bound: adopted, and that is
    // the path told.
    let tree = Tree::new();
    let mut first = Trees::new(&tree.roots());
    helper_setup(&mut first, &mut recorder).unwrap();
    first.bind(UDC).unwrap();
    let mut again = Trees::new(&tree.roots());
    assert_eq!(
        helper_setup(&mut again, &mut recorder).unwrap(),
        Outcome::Adopted
    );
    again.unbind(UDC).unwrap();
    again.bind(UDC).unwrap();
    assert_eq!(again.take_rebind(UDC).unwrap().path, "adopted");
    assert_eq!(tree.attribute("UDC").trim(), UDC);
}

/// GADGET-REBIND-VISIBLE, the daemon's side: a gadget taken off a controller it was bound to is
/// bound, then kept off the bus by the controller's pullup for the gap, so a host that held it
/// configured sees it go even where the controller kept its pullup through the unbind. Each such
/// bind is pulsed once, with no wait before it; a stand-in controller that has no `soft_connect`
/// (a write the kernel refuses) gets the gap with `UDC` clear instead. (The gap is
/// [`REBIND_GAP`]; 300 ms here, and 20 ms in this crate's other tests, which rebind hundreds of
/// times.)
#[test]
fn a_gadget_that_was_bound_is_kept_off_the_bus_for_the_gap_at_its_next_bind() {
    let gap = std::time::Duration::from_millis(300);
    let tree = Tree::new();
    setup(&tree);
    let soft_connect = tree.udc_root.join(UDC).join("soft_connect");
    let mut gadget = tree.gadget().with_rebind_gap(gap);
    up(&mut gadget, &set()).unwrap();
    assert_eq!(tree.attribute("UDC").trim(), UDC);
    assert_eq!(fs::read_to_string(&soft_connect).unwrap(), "connect\n");

    // Off and on again at once, as a driver that lets go and brings the function up does.
    fs::remove_file(&soft_connect).unwrap();
    let counter = Arc::clone(&gadget.wired.gap_waits);
    let waits = || counter.load(std::sync::atomic::Ordering::Relaxed);
    let before = std::time::Instant::now();
    let waited_before = waits();
    gadget.disable().unwrap();
    settled(&gadget);
    assert_eq!(tree.attribute("UDC").trim(), "");
    // Counted, not timed: the disable's own time is its files' close, which waits on the
    // kernel and stretches with the machine's load (280 ms seen in a loaded gate).
    assert_eq!(waits(), waited_before, "the disable waited no gap");
    up(&mut gadget, &set()).unwrap();
    assert!(
        before.elapsed() >= gap,
        "on the bus again {:?} after the unbind began",
        before.elapsed()
    );
    assert_eq!(waits(), waited_before + 1, "the bind waited the gap, once");
    assert_eq!(tree.attribute("UDC").trim(), UDC);
    assert_eq!(fs::read_to_string(&soft_connect).unwrap(), "connect\n");

    // A controller whose pullup cannot be taken off: `UDC` is kept clear for the gap instead.
    gadget.disable().unwrap();
    settled(&gadget);
    fs::remove_file(&soft_connect).unwrap();
    fs::create_dir(&soft_connect).unwrap();
    let began = std::time::Instant::now();
    up(&mut gadget, &set()).unwrap();
    assert!(
        began.elapsed() >= gap,
        "bound again after {:?}",
        began.elapsed()
    );
    assert_eq!(tree.attribute("UDC").trim(), UDC);
    fs::remove_dir(&soft_connect).unwrap();
}

/// A stand-in for the service manager's descriptor store, failing as systemd does: it keeps a copy
/// of each descriptor by name, closes every copy of a name it is told to let go, answers a barrier
/// only once it has acted on what came before (here: when the test says), and past its
/// `FileDescriptorStoreMax` drops what it is given without telling the sender (systemd logs it and
/// closes it; `sd_notify` still says it was sent).
struct Store {
    kept: Mutex<Vec<(String, std::os::fd::OwnedFd)>>,
    let_gone: Mutex<Vec<String>>,
    held_barriers: Mutex<Vec<std::io::PipeWriter>>,
    answer_at_once: std::sync::atomic::AtomicBool,
    max: usize,
}

impl Store {
    fn new(max: usize) -> Arc<Self> {
        Arc::new(Self {
            kept: Mutex::new(Vec::new()),
            let_gone: Mutex::new(Vec::new()),
            held_barriers: Mutex::new(Vec::new()),
            answer_at_once: std::sync::atomic::AtomicBool::new(true),
            max,
        })
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .kept
            .lock()
            .unwrap()
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
        names.sort();
        names
    }

    /// What the store passes the next start: a copy of each, with its name.
    fn given(&self) -> Vec<(String, std::os::fd::OwnedFd)> {
        self.kept
            .lock()
            .unwrap()
            .iter()
            .map(|(n, fd)| (n.clone(), fd.try_clone().unwrap()))
            .collect()
    }

    fn hold_barriers(&self) {
        self.answer_at_once.store(false, Ordering::SeqCst);
    }

    fn answer_barriers(&self) {
        self.held_barriers.lock().unwrap().clear();
        self.answer_at_once.store(true, Ordering::SeqCst);
    }
}

impl FdStore for Store {
    fn keep(&self, name: &str, fd: std::os::fd::BorrowedFd<'_>) -> io::Result<bool> {
        let mut kept = self.kept.lock().unwrap();
        if kept.len() < self.max {
            kept.push((name.to_string(), fd.try_clone_to_owned()?));
        }
        Ok(true)
    }

    fn let_go(&self, name: &str) {
        self.kept.lock().unwrap().retain(|(n, _)| n != name);
        self.let_gone.lock().unwrap().push(name.to_string());
    }

    fn barrier(&self) -> Option<std::os::fd::OwnedFd> {
        let (reader, writer) = std::io::pipe().ok()?;
        if self.answer_at_once.load(Ordering::SeqCst) {
            drop(writer);
        } else {
            self.held_barriers.lock().unwrap().push(writer);
        }
        Some(std::os::fd::OwnedFd::from(reader))
    }
}

fn kept_names() -> Vec<String> {
    let mut names: Vec<String> = keep::names(UDC, 2).collect();
    names.sort();
    names
}

/// A daemon's gadget, its parking keeping files in `store`.
fn keeping_gadget(tree: &Tree, store: &Arc<Store>) -> (FunctionFsGadget, Parking) {
    let parking = Parking::default();
    parking.keep_in(store.clone());
    (tree.gadget().parking(parking.clone()), parking)
}

#[test]
fn a_kept_gadget_stays_bound_through_a_stop_and_the_next_daemon_takes_it_up_as_it_was() {
    let tree = Tree::new();
    setup(&tree);
    let store = Store::new(64);
    let (mut gadget, parking) = keeping_gadget(&tree, &store);
    up(&mut gadget, &set()).unwrap();
    assert_eq!(
        store.names(),
        kept_names(),
        "the record, ep0 and both endpoints"
    );
    tree.event(ENABLE);
    assert_eq!(gadget.next_event(), Ok(Some(UsbFunctionEvent::Configured)));
    // The daemon stops: nothing is unbound, nothing let go.
    parking.stopping();
    gone(gadget);
    assert_eq!(tree.attribute("UDC"), "udc0\n", "still bound");
    assert!(store.let_gone.lock().unwrap().is_empty());
    assert_eq!(store.names(), kept_names());
    // The next daemon: its parking takes the kept files up.
    let ep0_before = fs::read(tree.mount().join("ep0")).unwrap();
    let next = Parking::default();
    next.keep_in(store.clone());
    let lines = next.adopt_kept(store.given(), Some(&[UDC.to_string()]));
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("host configured"), "{lines:?}");
    let mut gadget = tree.gadget().parking(next.clone());
    up(&mut gadget, &set()).unwrap();
    assert_eq!(gadget.took_up(), Some(TookUp::Adopted));
    assert_eq!(
        fs::read(tree.mount().join("ep0")).unwrap(),
        ep0_before,
        "no descriptors written again"
    );
    assert_eq!(tree.attribute("UDC"), "udc0\n", "UDC untouched");
    assert_eq!(
        gadget.next_event(),
        Ok(Some(UsbFunctionEvent::Configured)),
        "the host's state as the daemon before saw it"
    );
    assert!(gadget.endpoint_handles().is_ok(), "the pair is lent again");
    // Taken down on purpose now: let go of in the store, and unbound.
    gadget.disable().unwrap();
    settled(&gadget);
    assert_eq!(tree.attribute("UDC"), "\n");
    assert!(store.names().is_empty(), "{:?}", store.names());
}

#[test]
fn without_a_service_manager_a_stop_takes_the_gadget_off_and_the_next_start_sets_it_up_again() {
    // A BusyBox init gives no store (no NOTIFY_SOCKET): the daemon makes no parking at all
    // (service/lemnosd/src/main.rs), so a stop keeps nothing and takes the gadget down, and the
    // next start is a first start.
    let tree = Tree::new();
    setup(&tree);
    let mut gadget = tree.gadget();
    up(&mut gadget, &set()).unwrap();
    tree.event(ENABLE);
    assert_eq!(gadget.next_event(), Ok(Some(UsbFunctionEvent::Configured)));
    gone(gadget);
    assert_eq!(tree.attribute("UDC"), "\n", "off the bus at the stop");
    let mut gadget = tree.gadget();
    up(&mut gadget, &set()).unwrap();
    assert_eq!(gadget.took_up(), None, "nothing was kept to take up");
    assert_eq!(tree.attribute("UDC"), "udc0\n", "set up and bound again");
}

#[test]
fn a_gadget_taken_down_on_purpose_leaves_the_store_and_the_next_enable_waits_for_it() {
    let tree = Tree::new();
    setup(&tree);
    let store = Store::new(64);
    let (mut gadget, _parking) = keeping_gadget(&tree, &store);
    up(&mut gadget, &set()).unwrap();
    store.hold_barriers();
    gadget.disable().unwrap();
    settled(&gadget);
    assert_eq!(tree.attribute("UDC"), "\n");
    let mut gone = store.let_gone.lock().unwrap().clone();
    gone.sort();
    assert_eq!(gone, kept_names());
    // The store has not yet closed its copies: no fresh instance is described.
    assert_eq!(up(&mut gadget, &set()), Err(ErrorKind::Busy));
    assert_eq!(gadget.enable_note(), Some(KEPT_CLOSING));
    assert_eq!(tree.attribute("UDC"), "\n");
    store.answer_barriers();
    up(&mut gadget, &set()).unwrap();
    assert_eq!(tree.attribute("UDC"), "udc0\n");
    assert_eq!(store.names(), kept_names(), "kept again");
}

#[test]
fn kept_files_that_are_not_whole_or_not_the_boards_are_let_go_and_the_gadget_set_up_again() {
    // The store was full: it kept the record and ep0 only, and said nothing.
    let tree = Tree::new();
    setup(&tree);
    let store = Store::new(2);
    let (mut gadget, parking) = keeping_gadget(&tree, &store);
    up(&mut gadget, &set()).unwrap();
    parking.stopping();
    gone(gadget);
    let next = Parking::default();
    next.keep_in(store.clone());
    let lines = next.adopt_kept(store.given(), Some(&[UDC.to_string()]));
    assert!(lines[0].contains("not whole"), "{lines:?}");
    assert!(store.names().is_empty(), "let go: {:?}", store.names());
    let mut gadget = tree.gadget().parking(next);
    up(&mut gadget, &set()).unwrap();
    assert_eq!(gadget.took_up(), None, "set up afresh");
    gone(gadget);
    // Kept for a controller the board has no gadget on: let go.
    let store = Store::new(64);
    let (mut gadget, parking) = keeping_gadget(&tree, &store);
    up(&mut gadget, &set()).unwrap();
    parking.stopping();
    gone(gadget);
    let next = Parking::default();
    next.keep_in(store.clone());
    let lines = next.adopt_kept(store.given(), Some(&["other.usb".to_string()]));
    assert!(
        lines[0].contains("no gadget on that controller"),
        "{lines:?}"
    );
    assert!(store.names().is_empty());
}

#[test]
fn a_kept_session_whose_set_changed_is_replaced_and_let_go() {
    let tree = Tree::new();
    setup(&tree);
    let store = Store::new(64);
    let (mut gadget, parking) = keeping_gadget(&tree, &store);
    up(&mut gadget, &set()).unwrap();
    parking.stopping();
    gone(gadget);
    let next = Parking::default();
    next.keep_in(store.clone());
    next.adopt_kept(store.given(), Some(&[UDC.to_string()]));
    let mut gadget = tree.gadget().parking(next);
    // Another set (no serial string): the kept session is not this one's.
    assert_eq!(up(&mut gadget, &set_without_serial()), Ok(()));
    assert_eq!(gadget.took_up(), Some(TookUp::Replaced));
    assert!(
        store.let_gone.lock().unwrap().len() >= kept_names().len(),
        "the old files let go"
    );
}

#[test]
fn a_bind_waits_while_another_network_device_has_the_interfaces_name() {
    let tree = Tree::new();
    setup_with(&tree, own(), &[GadgetFunction::Ffs, ncm()]).unwrap();
    fs::write(
        tree.gadget_dir().join("functions/ncm.usb0/ifname"),
        "lemnos%d\n",
    )
    .unwrap();
    let net = tree.root.join("net");
    fs::create_dir_all(net.join("lemnos0")).unwrap();
    let mut gadget = tree.gadget();
    assert_eq!(up(&mut gadget, &set()), Err(ErrorKind::Busy));
    assert_eq!(gadget.enable_note(), Some(NETWORK_NAME_HELD));
    assert_eq!(tree.attribute("UDC"), "\n", "not bound");
    fs::remove_dir_all(net.join("lemnos0")).unwrap();
    up(&mut gadget, &set()).unwrap();
    assert_eq!(tree.attribute("UDC"), "udc0\n");
    // The kernel named it another than the board's first: said, and kept as the note.
    fs::write(
        tree.gadget_dir().join("functions/ncm.usb0/ifname"),
        "lemnos1\n",
    )
    .unwrap();
    fs::create_dir_all(net.join("lemnos1")).unwrap();
    fs::write(net.join("lemnos1/ifindex"), "7\n").unwrap();
    assert_eq!(gadget.wired.network_now(), Some(("lemnos1".to_string(), 7)));
    gadget.check_name(Some("lemnos%d"));
    assert_eq!(gadget.enable_note(), Some(NETWORK_NAME_OTHER));
}

/// A helper whose setup does not answer until the test lets it: one stuck in a write the kernel
/// does not finish (the vudc bind that waited 80 s on a board), or one that never answers.
struct Stalled {
    inner: Helped,
    gate: Arc<(Mutex<bool>, std::sync::Condvar)>,
    setups: Arc<AtomicU32>,
}

impl GadgetHelper for Stalled {
    fn setup(&mut self, index: u32) -> io::Result<SerialString> {
        self.setups.fetch_add(1, Ordering::SeqCst);
        let (open, opened) = &*self.gate;
        let mut open = open.lock().unwrap();
        while !*open {
            open = opened.wait(open).unwrap();
        }
        drop(open);
        self.inner.setup(index)
    }

    fn bind(&mut self, index: u32) -> io::Result<()> {
        self.inner.bind(index)
    }

    fn unbind(&mut self, index: u32) -> io::Result<()> {
        self.inner.unbind(index)
    }
    fn soft_connect(&mut self, index: u32, connect: bool) -> io::Result<()> {
        self.inner.soft_connect(index, connect)
    }
}

/// A helper that does not answer never holds the caller: every enable returns at once, says it is
/// under way (and, past the slow mark, that it is still waiting), and the helper is asked once,
/// never again while that request may still be carried out. Once it answers, the gadget comes up.
#[test]
fn a_helper_that_does_not_answer_never_holds_the_caller_and_is_never_asked_again() {
    let tree = Tree::new();
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let setups = Arc::new(AtomicU32::new(0));
    let stalled: SharedHelper = Arc::new(Mutex::new(Stalled {
        inner: Helped {
            configfs: tree.config_root.clone(),
            mounts: tree.mounts_root.clone(),
            udc: tree.udc_root.clone(),
            failing: 0,
            bind_errno: None,
            binds_without_network: false,
            calls: Vec::new(),
        },
        gate: Arc::clone(&gate),
        setups: Arc::clone(&setups),
    }));
    let mut gadget = tree.gadget().with_helper(stalled, 0);
    let began = Instant::now();
    let mut slowest = Duration::ZERO;
    while began.elapsed() < Duration::from_millis(400) {
        let call = Instant::now();
        assert_eq!(gadget.enable(&set()), Err(ErrorKind::Busy));
        slowest = slowest.max(call.elapsed());
        assert!(gadget.enable_pending());
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        slowest < Duration::from_millis(50),
        "an enable took {slowest:?}"
    );
    assert_eq!(
        setups.load(Ordering::SeqCst),
        1,
        "asked once, and not again"
    );
    assert_eq!(gadget.enable_note(), Some(SETTING_UP_SLOW));
    assert_eq!(tree.attribute("UDC"), "", "nothing bound meanwhile");
    // A disable while it waits undoes it when it ends: never bound behind the caller's back.
    gadget.disable().unwrap();
    {
        let (open, opened) = &*gate;
        *open.lock().unwrap() = true;
        opened.notify_all();
    }
    settled(&gadget);
    assert_ne!(
        tree.attribute("UDC"),
        "udc0\n",
        "the late answer bound nothing"
    );
    // Asked again now, it comes up.
    up(&mut gadget, &set()).unwrap();
    assert_eq!(tree.attribute("UDC"), "udc0\n");
    assert_eq!(setups.load(Ordering::SeqCst), 2);
}
