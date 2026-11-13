//! The simulated board every built-in driver the simulator can host is walked on.
//!
//! [`walk`] builds one board: the buses and chips the devices use, the chip descriptions the
//! description drivers read (written under the given root from `drivers/generic-sensor`'s test
//! chips), and the fake sysfs trees the sysfs drivers read, all under that root. It returns the
//! board, the device entries in board-file form, and the root. `lemnos-ctl`'s show walk and any
//! other walk over the built-in drivers use it, so a driver is simulated in one place.
//!
//! [`SIMULATED`] lists the built-in drivers that have a device in [`walk`]'s board. [`NOT_SIMULATED`]
//! lists every other built-in driver by name, with the reason the simulator cannot host it.

use std::io;
use std::path::{Path, PathBuf};

use lemnos_sim::{Layout, SimBoard};

/// One built-in driver's device in the walk, and the chip or tree behind it.
pub struct Walk {
    /// The simulated board: attach it to the host as the backend.
    pub board: SimBoard,
    /// The `[[devices]]` entries, in board-file form (without the header).
    pub devices: String,
    /// The root the chip descriptions and fake sysfs trees were written under.
    pub root: PathBuf,
}

/// The built-in drivers with a device in [`walk`]'s board.
pub const SIMULATED: &[&str] = &[
    "bmi088",
    "gpio-power-switch",
    "ina2xx",
    "orientation",
    "pwm-channel-fan",
    "thermal-zone",
    "hwmon-fan",
    "ws2812",
    "status-light",
    "indicator",
    "fan-control",
    "host-clock",
    "hw-watchdog",
];

/// The built-in drivers the simulator cannot host, each with the reason: their hardware is a kernel
/// device node, a vendor block or a destructive system action, which the simulator does not model.
pub const NOT_SIMULATED: &[(&str, &str)] = &[
    (
        "a733-di",
        "an Allwinner image block: a kernel device node and vendor ioctls",
    ),
    (
        "a733-video",
        "an Allwinner video block: a kernel device node and vendor ioctls",
    ),
    (
        "g2d",
        "a 2D graphics block: a kernel device node and its ioctls",
    ),
    (
        "isp600",
        "an image signal processor: a kernel device node and its stream",
    ),
    (
        "k230-capture",
        "a K230 capture receiver: a kernel device node and a stream",
    ),
    (
        "k230-vpu",
        "a K230 video block: a kernel device node and vendor ioctls",
    ),
    (
        "k230isp",
        "a K230 image signal processor: a kernel device node and its stream",
    ),
    (
        "k230isp-frames",
        "a K230 image signal processor's frames: a kernel device node and its stream",
    ),
    ("nonai2d", "a 2D block: a kernel device node and its ioctls"),
    (
        "pisp-be",
        "a Raspberry Pi image pipeline back end: a kernel device node and its stream",
    ),
    (
        "pisp-fe",
        "a Raspberry Pi image pipeline front end: a kernel device node and its stream",
    ),
    (
        "rga",
        "a Rockchip 2D block: a kernel device node and its ioctls",
    ),
    (
        "rk-iep",
        "a Rockchip image enhancement block: a kernel device node and its ioctls",
    ),
    (
        "rk-mpp",
        "a Rockchip codec: a kernel device node and vendor commands",
    ),
    (
        "rkcif",
        "a Rockchip camera receiver: a kernel device node and its stream",
    ),
    (
        "rkisp",
        "a Rockchip image signal processor: a kernel device node and its stream",
    ),
    (
        "rknpu",
        "a Rockchip neural accelerator: a kernel device node and its ioctls",
    ),
    (
        "rkvpss",
        "a Rockchip image processor: a kernel device node and its ioctls",
    ),
    (
        "rp1-csi2",
        "a camera receiver on the RP1: a kernel device node and its stream",
    ),
    (
        "sunxi-vin",
        "an Allwinner camera input: a kernel device node and its stream",
    ),
    (
        "system-flash",
        "the system image flasher: writes a block device; never walked on a simulated board",
    ),
    (
        "system-reboot",
        "the system reboot: reboots the host; never walked",
    ),
    (
        "uvc",
        "a UVC camera: a kernel video device node and its stream",
    ),
    (
        "v4l2-stateless",
        "a V4L2 stateless codec: a kernel video device node and its requests",
    ),
    (
        "vglite",
        "a vector graphics block: a kernel device node and its ioctls",
    ),
    (
        "vip9000",
        "a neural accelerator: a kernel device node and its vendor interface",
    ),
];

/// The built-in drivers the simulator could host but this walk does not bind yet, each with the
/// fake still to write. They are named, so the set of built-in drivers is always whole.
pub const NOT_WALKED: &[(&str, &str)] = &[
    (
        "named-files",
        "the `lemnos/` names are snapshots of the image's own paths (data, boot, sys_block, module): the walk has no fake of them yet",
    ),
    (
        "generic-accelerometer",
        "a description's `description` setting is a file path under the `std` feature and the text otherwise, and a build that unifies features turns it on: the walk cannot pick one form for every build (see the report)",
    ),
    (
        "generic-gyroscope",
        "a description's `description` setting is a file path under the `std` feature and the text otherwise: the walk cannot pick one form for every build",
    ),
    (
        "generic-magnetometer",
        "a description's `description` setting is a file path under the `std` feature and the text otherwise: the walk cannot pick one form for every build",
    ),
    (
        "generic-temperature",
        "a description's `description` setting is a file path under the `std` feature and the text otherwise: the walk cannot pick one form for every build",
    ),
    (
        "generic-power-monitor",
        "a description's `description` setting is a file path under the `std` feature and the text otherwise: the walk cannot pick one form for every build",
    ),
    (
        "coral-loader",
        "the USB accelerator's firmware protocol is not faked yet",
    ),
    ("gpu-lease", "the DRM node it opens is not faked yet"),
    (
        "modbus-accelerometer",
        "a Modbus RTU responder is not on the simulated UART yet",
    ),
    (
        "modbus-gyroscope",
        "a Modbus RTU responder is not on the simulated UART yet",
    ),
    (
        "modbus-magnetometer",
        "a Modbus RTU responder is not on the simulated UART yet",
    ),
    (
        "modbus-power-monitor",
        "a Modbus RTU responder is not on the simulated UART yet",
    ),
    (
        "modbus-temperature",
        "a Modbus RTU responder is not on the simulated UART yet",
    ),
    (
        "raw-bus",
        "closed by default: a board that opens its buses by config is not written yet",
    ),
    (
        "system-identity",
        "the serial and data directory it reads are not faked yet",
    ),
    ("system-slots", "the slot store directory is not faked yet"),
    ("usb-gadget", "the USB function it exposes is not faked yet"),
    (
        "vcm",
        "its chip (the VCM driver chip's register map) is not faked yet",
    ),
    (
        "ov9782",
        "its sensor register map, mode table and stream are not faked yet",
    ),
    (
        "bmm150",
        "its chip (identity register and the vendor's measurement registers) is not faked yet; the generic description tests already drive a BMM150 on the simulated bus",
    ),
];

/// Builds the walk's board under `root`. Writes the chip descriptions and the fake sysfs trees
/// there; nothing outside `root` is touched, and nothing real is opened.
pub fn walk(root: &Path) -> io::Result<Walk> {
    let board = SimBoard::new();

    // The I2C bus of the generic chips: each chip at its own address, with the identity its
    // description reads (the chip ID registers the files name).
    // The sysfs trees: a thermal zone and a hwmon fan under one fake /sys, the clock source and the
    // RTC the host clock reads, and the watchdog the watchdog driver keeps alive with the node it opens.
    let sys = root.join("sys");
    let zone = sys.join("class/thermal/thermal_zone0");
    std::fs::create_dir_all(&zone)?;
    std::fs::write(zone.join("temp"), "41000\n")?;
    std::fs::write(zone.join("type"), "cpu-thermal\n")?;
    let hwmon = sys.join("class/hwmon/hwmon2");
    std::fs::create_dir_all(&hwmon)?;
    std::fs::write(hwmon.join("name"), "pwmfan\n")?;
    std::fs::write(hwmon.join("pwm1"), "128\n")?;
    std::fs::write(hwmon.join("pwm1_enable"), "1\n")?;
    std::fs::write(hwmon.join("fan1_input"), "1800\n")?;
    let clocksource = sys.join("devices/system/clocksource/clocksource0");
    std::fs::create_dir_all(&clocksource)?;
    std::fs::write(clocksource.join("current_clocksource"), "tsc\n")?;
    let rtc = sys.join("class/rtc/rtc0");
    std::fs::create_dir_all(&rtc)?;
    std::fs::write(rtc.join("hctosys"), "1\n")?;
    let watchdog = sys.join("class/watchdog/watchdog0");
    std::fs::create_dir_all(&watchdog)?;
    for (attr, value) in [
        ("identity", "sp5100\n"),
        ("timeout", "15\n"),
        ("min_timeout", "1\n"),
        ("max_timeout", "60\n"),
        ("timeleft", "14\n"),
        ("bootstatus", "0\n"),
        ("state", "active\n"),
    ] {
        std::fs::write(watchdog.join(attr), value)?;
    }
    let dev = root.join("dev");
    std::fs::create_dir_all(&dev)?;
    std::fs::write(dev.join("watchdog0"), "")?;

    // The I2C bus of the power monitor and the inertial chip: the INA226 with its identity words,
    // and the BMI088's two chips on their own addresses.
    board.add_line("gpio-power");
    let board_i2c = board.add_i2c("i2c-1");
    board_i2c.attach(0x40, Layout::Words);
    board_i2c.set_word(0x40, 0xfe, &0x5449u16.to_be_bytes());
    board_i2c.set_word(0x40, 0xff, &0x2260u16.to_be_bytes());
    board_i2c.attach(0x18, Layout::Bytes);
    board_i2c.set_bytes(0x18, 0x00, &[0x1e]);
    board_i2c.attach(0x68, Layout::Bytes);
    board_i2c.set_bytes(0x68, 0x00, &[0x0f]);

    // The PWM channel and the pixel strip the fan and light drivers use.
    board.add_pwm("pwm-fan");
    board.add_spi("spidev0.0");

    let devices = format!(
        "[[devices]]\nid = \"power\"\ndriver = \"ina2xx\"\nbus = \"i2c-1\"\nlabel = \"Board power\"\n\
config = {{ chip = \"ina226\", shunt_ohms = 0.01, max_current_a = 8.192 }}\n\
[[devices]]\nid = \"imu\"\ndriver = \"bmi088\"\nbus = \"i2c-1\"\n\
[[devices]]\nid = \"orientation\"\ndriver = \"orientation\"\nuses = {{ accel = \"imu\", gyro = \"imu\" }}\n\
[[devices]]\nid = \"switch\"\ndriver = \"gpio-power-switch\"\nbus = \"gpio-power\"\n\
[[devices]]\nid = \"clock\"\ndriver = \"host-clock\"\nconfig = {{ sys_root = \"{}\" }}\n\
[[devices]]\nid = \"watchdog\"\ndriver = \"hw-watchdog\"\n\
config = {{ device = \"watchdog0\", sys_root = \"{}\", dev_root = \"{}\", keepalive = true }}\n\
[[devices]]\nid = \"zone\"\ndriver = \"thermal-zone\"\nconfig = {{ path = \"{}\" }}\n\
[[devices]]\nid = \"fan-hwmon\"\ndriver = \"hwmon-fan\"\nconfig = {{ sys_root = \"{}\", name = \"pwmfan\", max_rpm = 3000 }}\n\
[[devices]]\nid = \"pwm-fan\"\ndriver = \"pwm-channel-fan\"\nbus = \"pwm-fan\"\nconfig = {{ max_rpm = 3000 }}\n\
[[devices]]\nid = \"light\"\ndriver = \"ws2812\"\nbus = \"spidev0.0\"\nconfig = {{ count = 8 }}\n\
[[devices]]\nid = \"status\"\ndriver = \"status-light\"\nuses = {{ pixels = \"light\" }}\nconfig = {{ count = 8 }}\n\
[[devices]]\nid = \"indicator\"\ndriver = \"indicator\"\nuses = {{ pixels = \"light\" }}\n\
[[devices]]\nid = \"fan-control\"\ndriver = \"fan-control\"\nuses = {{ fan = \"pwm-fan\", t0 = \"zone\" }}\n",
        sys.display(),
        sys.display(),
        dev.display(),
        zone.display(),
        sys.display(),
    );
    Ok(Walk {
        board,
        devices,
        root: root.to_path_buf(),
    })
}
