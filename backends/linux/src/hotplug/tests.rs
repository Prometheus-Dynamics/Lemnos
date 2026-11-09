//! The generic device watcher over fake sysfs and `/dev` trees: a USB camera
//! (device, interface bound to `uvcvideo`, V4L2 node, media node) built with
//! the same layout the kernel uses (device directories, class and bus entries
//! as symlinks into them).

use super::*;
use crate::hotplug::uevent::Action;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::atomic::{AtomicU32, Ordering};

/// A temporary directory, removed when dropped.
struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lemnos-hotplug-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const CAMERA: Filter = Filter::subsystems(&["video4linux", "media", "usb"]);

const DEVICE: &str = "/devices/platform/usb3/3-1";
const INTERFACE: &str = "/devices/platform/usb3/3-1/3-1:1.0";
const VIDEO: &str = "/devices/platform/usb3/3-1/3-1:1.0/video4linux/video3";
const MEDIA: &str = "/devices/platform/usb3/3-1/media0";

/// Owns the fake tree; `base` is canonical so devpaths strip cleanly.
struct Camera {
    _root: TestRoot,
    base: PathBuf,
}

impl Camera {
    fn new() -> Camera {
        let root = TestRoot::new();
        let base = fs::canonicalize(&root.0).expect("canonical test root");
        let camera = Camera { _root: root, base };
        let device = camera.sys("devices/platform/usb3/3-1");
        let interface = device.join("3-1:1.0");
        let video = interface.join("video4linux/video3");
        let media = device.join("media0");
        for dir in [&device, &interface, &video, &media] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(
            device.join("uevent"),
            "DEVTYPE=usb_device\nBUSNUM=3\nDEVNUM=2\nPRODUCT=46d/825/12\n",
        )
        .unwrap();
        fs::write(
            interface.join("uevent"),
            "DEVTYPE=usb_interface\nDRIVER=uvcvideo\nINTERFACE=14/2/0\n",
        )
        .unwrap();
        fs::write(video.join("uevent"), "DEVNAME=video3\nMAJOR=81\nMINOR=3\n").unwrap();
        fs::write(media.join("uevent"), "DEVNAME=media0\nMAJOR=246\nMINOR=0\n").unwrap();
        let bus = camera.sys("bus/usb/devices");
        fs::create_dir_all(&bus).unwrap();
        symlink(&device, bus.join("3-1")).unwrap();
        symlink(&interface, bus.join("3-1:1.0")).unwrap();
        let class = camera.sys("class/video4linux");
        fs::create_dir_all(&class).unwrap();
        symlink(&video, class.join("video3")).unwrap();
        let class = camera.sys("class/media");
        fs::create_dir_all(&class).unwrap();
        symlink(&media, class.join("media0")).unwrap();
        camera
    }

    fn sys(&self, relative: &str) -> PathBuf {
        self.base.join("sys").join(relative)
    }

    fn paths(&self) -> LinuxPaths {
        LinuxPaths::new()
            .with_sys_class_root(self.base.join("sys/class"))
            .with_sys_bus_root(self.base.join("sys/bus"))
            .with_dev_root(self.base.join("dev"))
    }
}

/// `(action, devpath, subsystem, devname)` of an event, for assertions.
type Seen = (String, String, Option<String>, Option<String>);

fn seen(item: UeventRef<'_>) -> Seen {
    match item {
        UeventRef::Event(msg) => describe(&msg),
        UeventRef::Overflow => ("overflow".into(), String::new(), None, None),
    }
}

fn describe(msg: &UeventMsg<'_>) -> Seen {
    (
        msg.action_str().to_owned(),
        msg.devpath().to_owned(),
        msg.subsystem().map(str::to_owned),
        msg.devname().map(str::to_owned),
    )
}

fn read_all(watcher: &mut DeviceWatcher) -> (usize, Vec<Seen>) {
    let mut out = Vec::new();
    let count = watcher.read(|item| out.push(seen(item))).expect("read");
    (count, out)
}

fn coldplug_all(watcher: &mut DeviceWatcher) -> Vec<Seen> {
    let mut out = Vec::new();
    watcher.coldplug(|item| out.push(seen(item)));
    out.sort();
    out
}

fn event(action: &str, devpath: &str, subsystem: &str, devname: Option<&str>) -> Seen {
    (
        action.into(),
        devpath.into(),
        Some(subsystem.into()),
        devname.map(str::to_owned),
    )
}

#[test]
fn filters_match_subsystems_and_device_types() {
    let device =
        UeventMsg::parse(b"add@/devices/x/3-1\0SUBSYSTEM=usb\0DEVTYPE=usb_device\0").unwrap();
    let interface =
        UeventMsg::parse(b"bind@/devices/x/3-1:1.0\0SUBSYSTEM=usb\0DEVTYPE=usb_interface\0")
            .unwrap();
    let gpio = UeventMsg::parse(b"add@/devices/y\0SUBSYSTEM=gpio\0").unwrap();
    assert!(CAMERA.matches(&device));
    assert!(CAMERA.matches(&interface));
    assert!(!CAMERA.matches(&gpio));
    let devices_only = CAMERA.with_devtypes(&["usb_device"]);
    assert!(devices_only.matches(&device));
    assert!(!devices_only.matches(&interface));
    assert_eq!(device.action(), Action::Add);
    assert_eq!(interface.action(), Action::Bind);
}

#[test]
fn coldplug_reports_the_present_devices_in_the_kernel_shape() {
    let camera = Camera::new();
    let mut watcher = DeviceWatcher::rescan(camera.paths(), CAMERA);
    let mut expected = vec![
        event("add", DEVICE, "usb", None),
        event("add", INTERFACE, "usb", None),
        event("add", MEDIA, "media", Some("media0")),
        event("add", VIDEO, "video4linux", Some("video3")),
    ];
    expected.sort();
    assert_eq!(coldplug_all(&mut watcher), expected);
}

#[test]
fn coldplug_honours_the_device_type_filter() {
    let camera = Camera::new();
    let filter = Filter::subsystems(&["usb", "video4linux"]).with_devtypes(&["usb_device"]);
    let mut watcher = DeviceWatcher::rescan(camera.paths(), filter);
    assert_eq!(
        coldplug_all(&mut watcher),
        vec![event("add", DEVICE, "usb", None)]
    );
}

#[test]
fn rescan_reports_bind_unbind_removal_and_replug() {
    let camera = Camera::new();
    let mut watcher = DeviceWatcher::rescan(camera.paths(), CAMERA);
    assert_eq!(watcher.source(), DeviceWatchSource::Rescan);
    assert!(watcher.fd().is_none());
    // The baseline holds the camera, so nothing is pending.
    assert_eq!(read_all(&mut watcher), (0, vec![]));

    let interface = camera.sys("devices/platform/usb3/3-1/3-1:1.0/uevent");
    fs::write(&interface, "DEVTYPE=usb_interface\nINTERFACE=14/2/0\n").unwrap();
    assert_eq!(
        read_all(&mut watcher),
        (
            1,
            vec![("unbind".into(), INTERFACE.into(), Some("usb".into()), None)]
        )
    );
    fs::write(
        &interface,
        "DEVTYPE=usb_interface\nDRIVER=uvcvideo\nINTERFACE=14/2/0\n",
    )
    .unwrap();
    assert_eq!(
        read_all(&mut watcher).1,
        vec![("bind".into(), INTERFACE.into(), Some("usb".into()), None)]
    );

    // Unplugged: the V4L2 node's class entry goes away.
    fs::remove_file(camera.sys("class/video4linux/video3")).unwrap();
    assert_eq!(
        read_all(&mut watcher).1,
        vec![(
            "remove".into(),
            VIDEO.into(),
            Some("video4linux".into()),
            Some("video3".into())
        )]
    );

    // Replugged on the same port with a new device number: remove, then add.
    fs::write(
        camera.sys("devices/platform/usb3/3-1/uevent"),
        "DEVTYPE=usb_device\nBUSNUM=3\nDEVNUM=5\nPRODUCT=46d/825/12\n",
    )
    .unwrap();
    assert_eq!(
        read_all(&mut watcher).1,
        vec![
            ("remove".into(), DEVICE.into(), Some("usb".into()), None),
            ("add".into(), DEVICE.into(), Some("usb".into()), None),
        ]
    );
}

#[test]
fn inotify_reports_class_and_dev_entries_of_the_filter() {
    let camera = Camera::new();
    let base = camera.base.clone();
    // Directories the watcher watches must exist before it starts.
    fs::create_dir_all(base.join("dev")).unwrap();
    let mut watcher = DeviceWatcher::inotify(camera.paths(), CAMERA).expect("inotify");
    assert_eq!(watcher.source(), DeviceWatchSource::Inotify);
    assert!(watcher.fd().is_some());
    assert_eq!(read_all(&mut watcher), (0, vec![]));

    // A V4L2 node appears: its class entry, then its /dev node.
    symlink(
        camera.sys("devices/platform/usb3/3-1/3-1:1.0/video4linux/video3"),
        camera.sys("class/video4linux/video7"),
    )
    .unwrap();
    let (consumed, events) = read_all(&mut watcher);
    assert!(consumed >= 1);
    assert_eq!(
        events,
        vec![event(
            "add",
            "/class/video4linux/video7",
            "video4linux",
            None
        )]
    );

    fs::write(base.join("dev/video0"), "").unwrap();
    fs::write(base.join("dev/tty9"), "").unwrap(); // not in the filter: consumed, not reported
    fs::write(base.join("dev/media0"), "").unwrap();
    let (_, events) = read_all(&mut watcher);
    assert_eq!(
        events,
        vec![
            event("add", "", "video4linux", Some("video0")),
            event("add", "", "media", Some("media0")),
        ]
    );

    fs::set_permissions(base.join("dev/media0"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(base.join("dev/video0")).unwrap();
    let (_, events) = read_all(&mut watcher);
    assert_eq!(
        events,
        vec![
            event("change", "", "media", Some("media0")),
            event("remove", "", "video4linux", Some("video0")),
        ]
    );
    assert_eq!(read_all(&mut watcher), (0, vec![]));
}

#[test]
fn inotify_and_rescan_watchers_share_the_coldplug_shape() {
    let camera = Camera::new();
    fs::create_dir_all(camera.base.join("dev")).unwrap();
    let mut inotify = DeviceWatcher::inotify(camera.paths(), CAMERA).expect("inotify");
    let mut rescan = DeviceWatcher::rescan(camera.paths(), CAMERA);
    // Both report the same action, subsystem and device node for the V4L2 node.
    let node_of = |found: Vec<Seen>| {
        found
            .into_iter()
            .find(|(_, _, subsystem, _)| subsystem.as_deref() == Some("video4linux"))
            .map(|(action, _, subsystem, devname)| (action, subsystem, devname))
    };
    assert_eq!(
        node_of(coldplug_all(&mut inotify)),
        Some((
            "add".into(),
            Some("video4linux".into()),
            Some("video3".into())
        ))
    );
    assert_eq!(
        node_of(coldplug_all(&mut rescan)),
        Some((
            "add".into(),
            Some("video4linux".into()),
            Some("video3".into())
        ))
    );
}

#[test]
fn new_prefers_inotify_on_test_roots_and_rescan_needs_no_descriptor() {
    let camera = Camera::new();
    let watcher = DeviceWatcher::new(camera.paths(), CAMERA);
    assert_eq!(watcher.source(), DeviceWatchSource::Inotify);
    let rescan = DeviceWatcher::rescan(camera.paths(), CAMERA);
    assert!(rescan.fd().is_none());
    assert!(rescan.wait(None).unwrap(), "a rescan is always due");
}

#[test]
fn paths_helpers_name_the_sysfs_mount() {
    let paths = LinuxPaths::new();
    assert_eq!(paths.sys_root(), Path::new("/sys"));
    let custom = LinuxPaths::new().with_sys_class_root("/tmp/fake/sys/class");
    assert_eq!(custom.sys_root(), Path::new("/tmp/fake/sys"));
}
