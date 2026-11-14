//! Kernel hotplug events as hints for the host: which of a board file's names an event may be
//! about. The host is told the name and nothing else.
//!
//! Every action is the same hint, since arriving, leaving and changing all mean "look again".
//! Events the kernel dropped become [`Hint::Any`]. What an event has to be for a name to fit:
//!
//! | Board name | Fits an event of | whose device is |
//! |---|---|---|
//! | `usb:1-1.4` | `usb` | that port, a whole device (not an interface or a root hub) |
//! | `usb:1a2b:3c4d` | `usb` | a whole device with that vendor and product, or with none given |
//! | `usb:class:CC...` | `usb` | any whole device: an event doesn't carry its interfaces' classes |
//! | `i2c-N` | `i2c`, `i2c-dev` | the adapter `i2c-N` or its `/dev` node |
//! | `spidevB.C` | `spidev`, `spi` | the node `spidevB.C`, or the SPI device `spiB.C` behind it |
//! | `gpiochipN:LINE` | `gpio` | the chip `gpiochipN` |
//! | `pwmchipN:CHANNEL` | `pwm` | the chip `pwmchipN`, or a channel exported under it |
//! | a tty (`ttyUSB0`) | `tty` | that tty |
//! | `videoN`, `v4l-subdevN`, `vbiN`, `radioN` | `video4linux` | that node |
//! | `mediaN` | `media` | that node |
//!
//! Any other name (a PIO bus, a USB controller, a block's DMA memory, another device) is not
//! watched for: it binds when its retry comes round, as before.

use std::time::{Duration, Instant};

use lemnos_core::{Hint, HintSource};

use super::{DeviceWatchSource, DeviceWatcher, Filter, LinuxPaths, UeventMsg, UeventRef};
use lemnos_hal::usb_match::UsbPattern;

use crate::usb::UsbName;

/// The devices [`BoardHints`] watches when every name it was given is a USB name.
pub const USB: Filter = Filter::subsystems(&["usb"]);

/// The devices [`BoardHints`] watches otherwise: every subsystem a board name can be behind.
pub const BOARD: Filter = Filter::subsystems(&[
    "usb",
    "i2c",
    "i2c-dev",
    "spi",
    "spidev",
    "gpio",
    "pwm",
    "tty",
    "video4linux",
    "media",
]);

/// How often a watcher with no descriptor walks sysfs.
const RESCAN_EVERY: Duration = Duration::from_secs(1);

/// What an event has to be about for one board name to fit it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Fits {
    /// A USB device, by port or by vendor and product: the name itself says which.
    Usb,
    /// Any event of one subsystem. A stable name (`gpio:LABEL:LINE`, `pwm:NODE:CHANNEL`) names a
    /// chip by its label or device-tree node, which an event does not carry, so every chip of
    /// the subsystem is announced for it. A hint says only "look again", so this costs a retry.
    Subsystem(&'static str),
    /// A device of one of these subsystems whose devpath ends in one of these names. Unused
    /// places are empty.
    Node {
        subsystems: [&'static str; 2],
        ends: [String; 2],
        /// Whether the name anywhere in the devpath is enough (a chip's channels).
        within: bool,
    },
}

/// A board name hints are given for, and what fits it. Worked out once, so reading an event
/// parses nothing and allocates nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Watched {
    name: String,
    fits: Fits,
}

/// Whether `rest` is a decimal number.
fn number(rest: &str) -> bool {
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

/// `name` without `prefix`, when what is left is a decimal number.
fn numbered<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    name.strip_prefix(prefix).filter(|rest| number(rest))
}

fn node(subsystems: [&'static str; 2], ends: [String; 2], within: bool) -> Fits {
    Fits::Node {
        subsystems,
        ends,
        within,
    }
}

/// What fits `name`, or `None` for a name that is not hardware the kernel announces.
fn fits_for(name: &str) -> Option<Fits> {
    if UsbName::parse(name).is_some() {
        return Some(Fits::Usb);
    }
    // A stable name: a GPIO line by its chip's label, a PWM channel by its chip's node.
    if let Some(rest) = name.strip_prefix("gpio:")
        && rest
            .rsplit_once(':')
            .is_some_and(|(label, line)| !label.is_empty() && number(line))
    {
        return Some(Fits::Subsystem("gpio"));
    }
    if let Some(rest) = name.strip_prefix("pwm:")
        && rest
            .rsplit_once(':')
            .is_some_and(|(node, channel)| node.starts_with('/') && number(channel))
    {
        return Some(Fits::Subsystem("pwm"));
    }
    if name.contains('/') {
        return None;
    }
    // A USB device controller, by its name in `/sys/class/udc`: it appears when the controller
    // comes up or switches to the device role, and a gadget waiting for it is tried then.
    if let Some(controller) = name.strip_prefix("udc:") {
        return (!controller.is_empty())
            .then(|| node(["udc", ""], [controller.to_string(), String::new()], false));
    }
    let one =
        |subsystem: &'static str| node([subsystem, ""], [name.to_string(), String::new()], false);
    if numbered(name, "i2c-").is_some() {
        return Some(node(
            ["i2c", "i2c-dev"],
            [name.to_string(), String::new()],
            false,
        ));
    }
    if let Some(rest) = name.strip_prefix("spidev")
        && let Some((bus, select)) = rest.split_once('.')
        && number(bus)
        && number(select)
    {
        // The node, and the SPI device it is made for when a driver binds to it.
        return Some(node(
            ["spidev", "spi"],
            [name.to_string(), format!("spi{rest}")],
            false,
        ));
    }
    if let Some((chip, line)) = name.split_once(':') {
        if !number(line) {
            return None;
        }
        if numbered(chip, "gpiochip").is_some() {
            return Some(node(["gpio", ""], [chip.to_string(), String::new()], false));
        }
        if numbered(chip, "pwmchip").is_some() {
            return Some(node(["pwm", ""], [chip.to_string(), String::new()], true));
        }
        return None;
    }
    if ["video", "v4l-subdev", "vbi", "radio"]
        .iter()
        .any(|prefix| numbered(name, prefix).is_some())
    {
        return Some(one("video4linux"));
    }
    if numbered(name, "media").is_some() {
        return Some(one("media"));
    }
    (name.len() > 3 && name.starts_with("tty")).then(|| one("tty"))
}

/// Hints for the names of a board file, from a [`DeviceWatcher`].
#[derive(Debug)]
pub struct BoardHints {
    watcher: DeviceWatcher,
    watched: Vec<Watched>,
    names: Vec<String>,
    scanned: Instant,
}

/// [`BoardHints`], by the name it had when USB names were all it knew.
pub type UsbHints = BoardHints;

impl BoardHints {
    /// Watches the machine for what `names` are behind, by kernel events where they can be had.
    /// `None` when none of the names is one the kernel announces: there is nothing to watch for.
    pub fn open<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<Self> {
        let names: Vec<&str> = names.into_iter().collect();
        let filter = Self::filter_for(names.iter().copied())?;
        Self::new(DeviceWatcher::new(LinuxPaths::new(), filter), names)
    }

    /// The filter a watcher for `names` needs: [`USB`] when every watched name is a USB name,
    /// [`BOARD`] otherwise, `None` when no name is watched.
    pub fn filter_for<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<Filter> {
        let mut any = false;
        let mut only_usb = true;
        for fits in names.into_iter().filter_map(fits_for) {
            any = true;
            only_usb &= fits == Fits::Usb;
        }
        any.then_some(if only_usb { USB } else { BOARD })
    }

    /// Hints from `watcher`, which should report [`BoardHints::filter_for`] these names, for
    /// those of `names` the kernel announces. `None` when none is.
    pub fn new<'a>(
        watcher: DeviceWatcher,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Option<Self> {
        let mut watched: Vec<Watched> = names
            .into_iter()
            .filter_map(|name| {
                Some(Watched {
                    name: name.to_string(),
                    fits: fits_for(name)?,
                })
            })
            .collect();
        watched.sort_by(|a, b| a.name.cmp(&b.name));
        watched.dedup_by(|a, b| a.name == b.name);
        let names = watched.iter().map(|w| w.name.clone()).collect();
        (!watched.is_empty()).then(|| Self {
            watcher,
            watched,
            names,
            scanned: Instant::now(),
        })
    }

    /// The names hints are given for, as the board file spells them.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Reports what has happened since the last call, without waiting.
    pub fn read(&mut self, report: &mut dyn FnMut(Hint<'_>)) {
        let watched = &self.watched;
        let read = self.watcher.read(|item| match item {
            UeventRef::Event(event) => hints_for(&event, watched, report),
            UeventRef::Overflow => report(Hint::Any),
        });
        if read.is_err() {
            // Whatever was pending can't be told apart any more.
            report(Hint::Any);
        }
    }
}

impl HintSource for BoardHints {
    fn wait(&mut self, timeout: Duration, report: &mut dyn FnMut(Hint<'_>)) -> bool {
        if self.watcher.source() == DeviceWatchSource::Rescan {
            // No descriptor to wait on: sleep, and walk sysfs now and then.
            std::thread::sleep(timeout);
            if self.scanned.elapsed() < RESCAN_EVERY {
                return true;
            }
            self.scanned = Instant::now();
        } else {
            match self.watcher.wait(Some(timeout)) {
                Ok(true) => {}
                Ok(false) => return true,
                Err(_) => {
                    // Don't spin on a descriptor that fails at once.
                    std::thread::sleep(timeout);
                    return true;
                }
            }
        }
        self.read(report);
        true
    }
}

/// The last part of a devpath, if it names a whole USB device by its port.
fn port_of(devpath: &str) -> Option<&str> {
    let last = devpath.rsplit('/').next()?;
    let (bus, ports) = last.split_once('-')?;
    (number(bus) && ports.split('.').all(number)).then_some(last)
}

/// The vendor and product of an event's `PRODUCT` (`46d/825/12`: hex, no leading zeros).
fn product_of(event: &UeventMsg<'_>) -> Option<(u16, u16)> {
    let mut parts = event.get("PRODUCT")?.split('/');
    let vendor = u16::from_str_radix(parts.next()?, 16).ok()?;
    let product = u16::from_str_radix(parts.next()?, 16).ok()?;
    Some((vendor, product))
}

/// Whether a USB event may be about the USB name `name`.
fn usb_fits(name: &str, port: Option<&str>, product: Option<(u16, u16)>) -> bool {
    let Some(port) = port else {
        return false;
    };
    match UsbName::parse(name) {
        Some(UsbName::Port(named)) => named == port,
        Some(UsbName::Is(UsbPattern::Id {
            vendor,
            product: id,
        })) => product.is_none_or(|found| found == (vendor, id)),
        Some(UsbName::Is(UsbPattern::Class { .. })) => true,
        None => false,
    }
}

/// Reports each of `watched` that `event` may be about. Allocates nothing.
fn hints_for(event: &UeventMsg<'_>, watched: &[Watched], report: &mut dyn FnMut(Hint<'_>)) {
    let Some(subsystem) = event.subsystem() else {
        return;
    };
    let devpath = event.devpath();
    let last = devpath.rsplit('/').next().unwrap_or("");
    let usb = subsystem == "usb";
    let port = if usb { port_of(devpath) } else { None };
    let product = if usb { product_of(event) } else { None };
    for watched in watched {
        let fits = match &watched.fits {
            Fits::Usb => usb && usb_fits(&watched.name, port, product),
            Fits::Subsystem(wanted) => subsystem == *wanted,
            Fits::Node {
                subsystems,
                ends,
                within,
            } => {
                subsystems.contains(&subsystem)
                    && ends.iter().any(|end| {
                        !end.is_empty()
                            && (last == end
                                || (*within && devpath.split('/').any(|part| part == end)))
                    })
            }
        };
        if fits {
            report(Hint::Changed(&watched.name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_linux_hotplug::encode;

    const NAMES: [&str; 5] = [
        "usb:3-1",
        "usb:3-1.4",
        "usb:046D:0825",
        "usb:1a2b:3c4d",
        "i2c-1",
    ];

    fn watched(names: &[&str]) -> Vec<Watched> {
        names
            .iter()
            .filter_map(|name| {
                Some(Watched {
                    name: (*name).to_string(),
                    fits: fits_for(name)?,
                })
            })
            .collect()
    }

    fn hints_among(
        names: &[&str],
        action: &str,
        devpath: &str,
        vars: &[(&str, &str)],
    ) -> Vec<String> {
        let watched = watched(names);
        let mut bytes = Vec::new();
        encode(&mut bytes, action, devpath, vars.iter().copied());
        let event = UeventMsg::parse(&bytes).unwrap();
        let mut seen = Vec::new();
        hints_for(&event, &watched, &mut |hint| seen.push(format!("{hint:?}")));
        seen
    }

    fn hints(action: &str, devpath: &str, vars: &[(&str, &str)]) -> Vec<String> {
        hints_among(&NAMES, action, devpath, vars)
    }

    const USB_DEVICE: [(&str, &str); 2] = [("SUBSYSTEM", "usb"), ("DEVTYPE", "usb_device")];

    #[test]
    fn a_usb_device_controller_is_hinted_by_its_own_class_event_when_it_appears_or_changes() {
        // A gadget waiting for its controller (`udc:NAME`) is tried when the controller's class
        // device comes or goes, as a controller comes up or switches to the device role.
        let names = ["udc:fe980000.usb", "udc:other.usb"];
        let class = [("SUBSYSTEM", "udc")];
        let path = "/devices/platform/soc/fe980000.usb/udc/fe980000.usb";
        assert_eq!(
            hints_among(&names, "add", path, &class),
            ["Changed(\"udc:fe980000.usb\")"]
        );
        assert_eq!(
            hints_among(&names, "remove", path, &class),
            ["Changed(\"udc:fe980000.usb\")"]
        );
        // Another subsystem's event with the same name hints nothing.
        assert!(hints_among(&names, "add", path, &[("SUBSYSTEM", "usb")]).is_empty());
    }

    #[test]
    fn a_stable_gpio_or_pwm_name_is_hinted_by_its_subsystem_as_the_numbered_one_is() {
        let names = [
            "gpio:pinctrl-rp1:34",
            "pwm:/axi/pcie@1000120000/rp1/pwm@9c000:1",
            "gpiochip0:17",
            "pwmchip0:1",
        ];
        let gpio = [("SUBSYSTEM", "gpio")];
        assert_eq!(
            hints_among(
                &names,
                "add",
                "/devices/platform/soc/1f000d0000.gpio/gpiochip4",
                &gpio
            ),
            ["Changed(\"gpio:pinctrl-rp1:34\")"],
        );
        let pwm = [("SUBSYSTEM", "pwm")];
        assert_eq!(
            hints_among(
                &names,
                "change",
                "/devices/platform/soc/fe20c000.pwm/pwm/pwmchip2/pwm1",
                &pwm
            ),
            ["Changed(\"pwm:/axi/pcie@1000120000/rp1/pwm@9c000:1\")"],
        );
        // An event of another subsystem hints neither stable name.
        let other = [("SUBSYSTEM", "i2c")];
        assert!(hints_among(&names, "add", "/devices/platform/i2c-9", &other).is_empty());
        // Malformed stable names are not watched at all.
        assert_eq!(fits_for("gpio:pinctrl-rp1"), None);
        assert_eq!(fits_for("pwm:axi/pwm@9c000:1"), None);
    }

    #[test]
    fn an_event_for_a_port_hints_the_name_that_gives_the_port() {
        let path = "/devices/platform/usb3/3-1/3-1.4";
        let vars = [USB_DEVICE[0], USB_DEVICE[1], ("PRODUCT", "bda/5411/101")];
        for action in ["add", "remove", "bind", "unbind", "change"] {
            assert_eq!(hints(action, path, &vars), ["Changed(\"usb:3-1.4\")"]);
        }
    }

    #[test]
    fn an_event_with_a_vendor_and_product_hints_the_name_that_gives_them_as_the_board_spells_it() {
        let vars = [USB_DEVICE[0], USB_DEVICE[1], ("PRODUCT", "46d/825/12")];
        assert_eq!(
            hints("add", "/devices/platform/usb1/1-2", &vars),
            ["Changed(\"usb:046D:0825\")"]
        );
        assert_eq!(
            hints("remove", "/devices/platform/usb3/3-1", &vars),
            ["Changed(\"usb:3-1\")", "Changed(\"usb:046D:0825\")"]
        );
    }

    #[test]
    fn an_event_that_carries_no_numbers_hints_every_name_that_gives_them() {
        // What the inotify source reports: a bus entry and its subsystem.
        assert_eq!(
            hints("remove", "/bus/usb/devices/1-2", &[("SUBSYSTEM", "usb")]),
            ["Changed(\"usb:046D:0825\")", "Changed(\"usb:1a2b:3c4d\")"]
        );
    }

    #[test]
    fn a_class_is_hinted_by_every_whole_device_and_by_nothing_else() {
        let names = ["usb:class:0e", "usb:class:0e:01:00", "usb:3-1"];
        let vars = [USB_DEVICE[0], USB_DEVICE[1], ("PRODUCT", "46d/825/12")];
        assert_eq!(
            hints_among(&names, "add", "/devices/platform/usb1/1-2", &vars),
            [
                "Changed(\"usb:class:0e\")",
                "Changed(\"usb:class:0e:01:00\")"
            ]
        );
        let usb = [("SUBSYSTEM", "usb")];
        assert!(hints_among(&names, "add", "/devices/platform/usb1/1-2/1-2:1.0", &usb).is_empty());
        assert!(hints_among(&names, "add", "/devices/platform/usb1", &usb).is_empty());
        assert_eq!(
            BoardHints::filter_for(["usb:class:0e"]),
            Some(USB),
            "a class is watched as any USB name is"
        );
    }

    #[test]
    fn interfaces_root_hubs_and_other_subsystems_hint_nothing() {
        let usb = [("SUBSYSTEM", "usb")];
        assert!(hints("add", "/devices/platform/usb3/3-1/3-1:1.0", &usb).is_empty());
        assert!(hints("add", "/devices/platform/usb3", &usb).is_empty());
        assert!(hints("add", "/devices/platform/usb3/3-1/3-1.4/", &usb).is_empty());
        let video = [("SUBSYSTEM", "video4linux")];
        assert!(hints("add", "/devices/platform/usb3/3-1", &video).is_empty());
        assert!(hints("add", "/devices/platform/usb3/3-1", &[]).is_empty());
    }

    #[test]
    fn a_board_with_nothing_the_kernel_announces_has_no_watcher() {
        let paths = LinuxPaths::new().with_sys_class_root("/nonexistent/sys/class");
        let watcher =
            || DeviceWatcher::rescan(paths.clone().with_sys_bus_root("/nonexistent"), BOARD);
        let hints = BoardHints::new(watcher(), NAMES).unwrap();
        assert_eq!(
            hints.names(),
            [
                "i2c-1",
                "usb:046D:0825",
                "usb:1a2b:3c4d",
                "usb:3-1",
                "usb:3-1.4"
            ]
        );
        let none = [
            "usb:nonsense",
            "pio-i2c:sda=8,scl=7",
            "udc:",
            "udc:a/b",
            "dma:91400000.isp",
            "imu",
            "i2c-",
            "i2c-x",
            "spidev0",
            "gpiochip0",
            "gpiochip0:x",
            "pwmchip:1",
            "tty",
            "video",
            "../i2c-1",
            "tty/../x",
        ];
        assert!(BoardHints::new(watcher(), none).is_none());
        assert_eq!(BoardHints::filter_for(none), None);
        // Only the subsystems the names can be behind are watched.
        assert_eq!(BoardHints::filter_for(["usb:3-1", "imu"]), Some(USB));
        assert_eq!(BoardHints::filter_for(["usb:3-1", "i2c-1"]), Some(BOARD));
        assert_eq!(BoardHints::filter_for(["ttyAMA0"]), Some(BOARD));
    }

    /// Every kind of name a board file gives the Linux backend that the kernel announces.
    const BOARD_NAMES: [&str; 12] = [
        "i2c-1",
        "i2c-10",
        "spidev0.1",
        "gpiochip0:17",
        "gpiochip4:2",
        "pwmchip2:1",
        "ttyAMA0",
        "ttyUSB0",
        "video3",
        "v4l-subdev2",
        "media0",
        "usb:3-1",
    ];

    fn board(subsystem: &str, devpath: &str) -> Vec<String> {
        hints_among(&BOARD_NAMES, "add", devpath, &[("SUBSYSTEM", subsystem)])
    }

    #[test]
    fn an_i2c_adapter_or_its_node_hints_the_name_of_that_adapter_alone() {
        let adapter = "/devices/platform/soc/fe804000.i2c/i2c-1";
        assert_eq!(board("i2c", adapter), ["Changed(\"i2c-1\")"]);
        assert_eq!(
            board(
                "i2c-dev",
                "/devices/platform/soc/fe804000.i2c/i2c-1/i2c-dev/i2c-1"
            ),
            ["Changed(\"i2c-1\")"]
        );
        assert_eq!(
            board("i2c", "/devices/platform/soc/fe805000.i2c/i2c-10"),
            ["Changed(\"i2c-10\")"]
        );
        // A chip on the adapter, another adapter, and the same name in another subsystem.
        assert!(board("i2c", "/devices/platform/soc/fe804000.i2c/i2c-1/1-0068").is_empty());
        assert!(board("i2c", "/devices/platform/soc/fe804000.i2c/i2c-2").is_empty());
        assert!(board("hwmon", adapter).is_empty());
        // What the inotify and rescan sources report: the bus's or the class's own entry.
        assert_eq!(
            board("i2c", "/bus/i2c/devices/i2c-1"),
            ["Changed(\"i2c-1\")"]
        );
        assert_eq!(
            board("i2c-dev", "/class/i2c-dev/i2c-10"),
            ["Changed(\"i2c-10\")"]
        );
    }

    #[test]
    fn a_spidev_node_or_the_spi_device_behind_it_hints_its_name() {
        let device = "/devices/platform/soc/fe204000.spi/spi_master/spi0/spi0.1";
        assert_eq!(board("spi", device), ["Changed(\"spidev0.1\")"]);
        assert_eq!(
            board(
                "spidev",
                "/devices/platform/soc/fe204000.spi/spi_master/spi0/spi0.1/spidev/spidev0.1"
            ),
            ["Changed(\"spidev0.1\")"]
        );
        assert!(
            board(
                "spi",
                "/devices/platform/soc/fe204000.spi/spi_master/spi0/spi0.0"
            )
            .is_empty()
        );
        assert!(
            board(
                "spi_master",
                "/devices/platform/soc/fe204000.spi/spi_master/spi0"
            )
            .is_empty()
        );
    }

    #[test]
    fn a_gpio_chip_hints_every_line_named_on_it_and_no_other_chips() {
        assert_eq!(
            board("gpio", "/devices/platform/soc/fe200000.gpio/gpiochip0"),
            ["Changed(\"gpiochip0:17\")"]
        );
        assert_eq!(
            board("gpio", "/devices/platform/soc/1f000d0000.gpio/gpiochip4"),
            ["Changed(\"gpiochip4:2\")"]
        );
        assert!(board("gpio", "/devices/platform/soc/fe200000.gpio/gpiochip40").is_empty());
        // The legacy class numbers chips by their first line: not the same number.
        assert!(board("gpio", "/class/gpio/gpiochip512").is_empty());
    }

    #[test]
    fn a_pwm_chip_or_a_channel_exported_under_it_hints_its_channels() {
        let chip = "/devices/platform/soc/fe20c000.pwm/pwm/pwmchip2";
        assert_eq!(board("pwm", chip), ["Changed(\"pwmchip2:1\")"]);
        assert_eq!(
            board(
                "pwm",
                "/devices/platform/soc/fe20c000.pwm/pwm/pwmchip2/pwm1"
            ),
            ["Changed(\"pwmchip2:1\")"]
        );
        assert!(board("pwm", "/devices/platform/soc/fe20c000.pwm/pwm/pwmchip20").is_empty());
        assert!(board("gpio", chip).is_empty());
    }

    #[test]
    fn a_tty_a_video_node_and_a_media_node_hint_their_own_names() {
        assert_eq!(
            board("tty", "/devices/platform/soc/fe201000.serial/tty/ttyAMA0"),
            ["Changed(\"ttyAMA0\")"]
        );
        assert_eq!(
            board(
                "tty",
                "/devices/platform/usb1/1-1/1-1:1.0/ttyUSB0/tty/ttyUSB0"
            ),
            ["Changed(\"ttyUSB0\")"]
        );
        assert!(board("tty", "/devices/virtual/tty/ttyUSB1").is_empty());
        // The USB serial port's own device is not the tty.
        assert!(board("usb-serial", "/devices/platform/usb1/1-1/1-1:1.0/ttyUSB0").is_empty());
        assert_eq!(
            board(
                "video4linux",
                "/devices/platform/usb3/3-1/3-1:1.0/video4linux/video3"
            ),
            ["Changed(\"video3\")"]
        );
        assert_eq!(
            board(
                "video4linux",
                "/devices/platform/csi/video4linux/v4l-subdev2"
            ),
            ["Changed(\"v4l-subdev2\")"]
        );
        assert_eq!(
            board("media", "/devices/platform/usb3/3-1/media0"),
            ["Changed(\"media0\")"]
        );
        assert!(board("video4linux", "/devices/platform/csi/video4linux/video30").is_empty());
        assert!(board("media", "/devices/platform/csi/media1").is_empty());
        // And a USB device still hints its port.
        assert_eq!(
            board("usb", "/devices/platform/usb3/3-1"),
            ["Changed(\"usb:3-1\")"]
        );
    }

    #[test]
    fn working_out_which_names_an_event_fits_allocates_nothing() {
        let watched = watched(&BOARD_NAMES);
        let mut bytes = Vec::new();
        let vars = [("SUBSYSTEM", "pwm")];
        encode(
            &mut bytes,
            "add",
            "/devices/platform/soc/fe20c000.pwm/pwm/pwmchip2/pwm1",
            vars.iter().copied(),
        );
        let event = UeventMsg::parse(&bytes).unwrap();
        let mut seen = 0;
        let ((), count) = lemnos_measure::test_alloc::allocations(|| {
            for _ in 0..1000 {
                hints_for(&event, &watched, &mut |_| seen += 1);
            }
        });
        assert_eq!(seen, 1000);
        assert_eq!(count, 0);
    }
}
