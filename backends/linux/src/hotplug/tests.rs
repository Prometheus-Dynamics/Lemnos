//! Board hints over fake sysfs and `/dev` trees, built with the same layout the kernel uses
//! (device directories, class and bus entries as symlinks into them). The watcher's own tests are
//! in `lemnos-linux-hotplug`.

use super::*;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

/// A temporary directory, removed when dropped.
struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lemnos-hints-{}-{}",
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

fn hints_read(hints: &mut BoardHints) -> Vec<String> {
    let mut seen = Vec::new();
    hints.read(&mut |hint| seen.push(format!("{hint:?}")));
    seen
}

#[test]
fn a_camera_leaving_and_returning_hints_the_board_names_it_goes_by() {
    let camera = Camera::new();
    let names = ["usb:3-1", "usb:046d:0825", "usb:9-9", "usb:1a2b:3c4d"];
    for inotify in [false, true] {
        let watcher = if inotify {
            DeviceWatcher::inotify(camera.paths(), USB).expect("inotify")
        } else {
            DeviceWatcher::rescan(camera.paths(), USB)
        };
        let source = watcher.source();
        let mut hints = UsbHints::new(watcher, names).unwrap();
        assert_eq!(hints_read(&mut hints), Vec::<String>::new(), "{source:?}");

        // Unplugged: the device and its interface leave the bus's list.
        let bus = camera.sys("bus/usb/devices");
        fs::remove_file(bus.join("3-1:1.0")).unwrap();
        fs::remove_file(bus.join("3-1")).unwrap();
        let left = hints_read(&mut hints);
        assert!(
            left.contains(&"Changed(\"usb:3-1\")".to_string()),
            "{source:?} {left:?}"
        );
        assert!(
            left.contains(&"Changed(\"usb:046d:0825\")".to_string()),
            "{source:?} {left:?}"
        );
        assert!(
            !left.contains(&"Changed(\"usb:9-9\")".to_string()),
            "{source:?} {left:?}"
        );
        assert_eq!(hints_read(&mut hints), Vec::<String>::new(), "{source:?}");

        // Plugged back in.
        symlink(camera.sys("devices/platform/usb3/3-1"), bus.join("3-1")).unwrap();
        symlink(
            camera.sys("devices/platform/usb3/3-1/3-1:1.0"),
            bus.join("3-1:1.0"),
        )
        .unwrap();
        let back = hints_read(&mut hints);
        assert!(
            back.contains(&"Changed(\"usb:3-1\")".to_string()),
            "{source:?} {back:?}"
        );
        assert!(
            back.contains(&"Changed(\"usb:046d:0825\")".to_string()),
            "{source:?} {back:?}"
        );
    }
}

#[test]
fn waiting_for_hints_comes_back_by_the_timeout_and_with_what_happened() {
    use lemnos_core::HintSource;
    use std::time::{Duration, Instant};

    let camera = Camera::new();
    let watcher = DeviceWatcher::inotify(camera.paths(), USB).expect("inotify");
    let mut hints = UsbHints::new(watcher, ["usb:3-1"]).unwrap();
    let mut seen = Vec::new();
    let start = Instant::now();
    assert!(hints.wait(Duration::from_millis(30), &mut |hint| {
        seen.push(format!("{hint:?}"))
    }));
    assert!(seen.is_empty(), "{seen:?}");
    assert!(start.elapsed() >= Duration::from_millis(30));

    fs::remove_file(camera.sys("bus/usb/devices/3-1")).unwrap();
    let start = Instant::now();
    assert!(hints.wait(Duration::from_secs(20), &mut |hint| {
        seen.push(format!("{hint:?}"))
    }));
    assert_eq!(seen, ["Changed(\"usb:3-1\")"]);
    assert!(start.elapsed() < Duration::from_secs(10));
}

// Board names that are not USB: adapters, nodes and chips coming and going.

/// A machine's sysfs and `/dev` with the class and bus directories in place and nothing in
/// them yet.
struct Machine {
    _root: TestRoot,
    base: PathBuf,
}

/// One piece of hardware: where the kernel lists it, where its device directory is, and the
/// board name it goes by.
struct Part {
    name: &'static str,
    listed: &'static str,
    device: &'static str,
}

const PARTS: [Part; 7] = [
    Part {
        name: "i2c-7",
        listed: "bus/i2c/devices/i2c-7",
        device: "devices/platform/soc/fe805000.i2c/i2c-7",
    },
    Part {
        name: "spidev0.1",
        listed: "class/spidev/spidev0.1",
        device: "devices/platform/soc/fe204000.spi/spi_master/spi0/spi0.1/spidev/spidev0.1",
    },
    Part {
        name: "gpiochip4:2",
        listed: "bus/gpio/devices/gpiochip4",
        device: "devices/platform/soc/1f000d0000.gpio/gpiochip4",
    },
    Part {
        name: "pwmchip2:1",
        listed: "class/pwm/pwmchip2",
        device: "devices/platform/soc/fe20c000.pwm/pwm/pwmchip2",
    },
    Part {
        name: "ttyUSB0",
        listed: "class/tty/ttyUSB0",
        device: "devices/platform/usb1/1-1/1-1:1.0/ttyUSB0/tty/ttyUSB0",
    },
    Part {
        name: "video3",
        listed: "class/video4linux/video3",
        device: "devices/platform/csi/video4linux/video3",
    },
    Part {
        name: "media0",
        listed: "class/media/media0",
        device: "devices/platform/csi/media0",
    },
];

impl Machine {
    fn new() -> Self {
        let root = TestRoot::new();
        let base = fs::canonicalize(&root.0).expect("canonical test root");
        let machine = Self { _root: root, base };
        for part in &PARTS {
            let listed = machine.sys(part.listed);
            fs::create_dir_all(listed.parent().unwrap()).unwrap();
            fs::create_dir_all(machine.sys(part.device)).unwrap();
        }
        fs::create_dir_all(machine.base.join("dev")).unwrap();
        machine
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

    /// The kernel lists the part: its entry appears.
    fn arrive(&self, part: &Part) {
        symlink(self.sys(part.device), self.sys(part.listed)).unwrap();
    }

    fn leave(&self, part: &Part) {
        fs::remove_file(self.sys(part.listed)).unwrap();
    }
}

#[test]
fn every_kind_of_board_name_is_hinted_when_what_is_behind_it_arrives_and_when_it_leaves() {
    let names: Vec<&str> = PARTS.iter().map(|part| part.name).collect();
    for inotify in [false, true] {
        let machine = Machine::new();
        let filter = BoardHints::filter_for(names.iter().copied()).unwrap();
        assert_eq!(filter, BOARD);
        let watcher = if inotify {
            DeviceWatcher::inotify(machine.paths(), filter).expect("inotify")
        } else {
            DeviceWatcher::rescan(machine.paths(), filter)
        };
        let source = watcher.source();
        let mut hints = BoardHints::new(watcher, names.iter().copied()).unwrap();
        assert_eq!(hints_read(&mut hints), Vec::<String>::new(), "{source:?}");
        for part in &PARTS {
            let only = vec![format!("Changed({:?})", part.name)];
            machine.arrive(part);
            let mut came = hints_read(&mut hints);
            came.dedup();
            assert_eq!(came, only, "{source:?}: {} arriving", part.name);
            assert_eq!(hints_read(&mut hints), Vec::<String>::new(), "{source:?}");
            machine.leave(part);
            let mut went = hints_read(&mut hints);
            went.dedup();
            assert_eq!(went, only, "{source:?}: {} leaving", part.name);
        }
    }
}

#[test]
fn a_part_no_board_name_is_behind_hints_nothing() {
    let machine = Machine::new();
    let watcher = DeviceWatcher::rescan(machine.paths(), BOARD);
    // The board names other adapters, chips and nodes than the ones that come.
    let names = [
        "i2c-1",
        "spidev1.0",
        "gpiochip0:2",
        "pwmchip0:0",
        "ttyAMA0",
        "video0",
        "media1",
    ];
    let mut hints = BoardHints::new(watcher, names).unwrap();
    for part in &PARTS {
        machine.arrive(part);
    }
    assert_eq!(hints_read(&mut hints), Vec::<String>::new());
}
