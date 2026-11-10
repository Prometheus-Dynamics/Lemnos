//! Kernel hotplug events as hints for the host: which of a board file's USB names an event may
//! be about. The host is told the name and nothing else.
//!
//! Only whole USB devices count (`1-1.4`, not its interfaces or a root hub). Every action is
//! the same hint, since arriving, leaving and changing all mean "look again". A name that gives
//! a port fits events for that port. A name that gives a vendor and product fits events that
//! carry those numbers, and every event that carries none, since such an event could be about
//! it. Events the kernel dropped become [`Hint::Any`].

use std::time::{Duration, Instant};

use lemnos_core::{Hint, HintSource};

use super::{DeviceWatchSource, DeviceWatcher, Filter, LinuxPaths, UeventMsg, UeventRef};
use crate::usb::UsbName;

/// The devices [`UsbHints`] watches.
pub const USB: Filter = Filter::subsystems(&["usb"]);

/// How often a watcher with no descriptor walks sysfs.
const RESCAN_EVERY: Duration = Duration::from_secs(1);

/// Hints for the USB names of a board file, from a [`DeviceWatcher`].
#[derive(Debug)]
pub struct UsbHints {
    watcher: DeviceWatcher,
    names: Vec<String>,
    scanned: Instant,
}

impl UsbHints {
    /// Watches the machine's USB devices for `names`, by kernel events where they can be had.
    /// `None` when none of the names is a USB name: there is nothing to watch for.
    pub fn open<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<Self> {
        Self::new(DeviceWatcher::new(LinuxPaths::new(), USB), names)
    }

    /// Hints from `watcher`, which should report the [`USB`] filter, for those of `names` that
    /// are USB names. `None` when none is.
    pub fn new<'a>(
        watcher: DeviceWatcher,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Option<Self> {
        let mut usb: Vec<String> = names
            .into_iter()
            .filter(|name| UsbName::parse(name).is_some())
            .map(str::to_string)
            .collect();
        usb.sort();
        usb.dedup();
        (!usb.is_empty()).then(|| Self {
            watcher,
            names: usb,
            scanned: Instant::now(),
        })
    }

    /// The names hints are given for, as the board file spells them.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Reports what has happened since the last call, without waiting.
    pub fn read(&mut self, report: &mut dyn FnMut(Hint<'_>)) {
        let names = &self.names;
        let read = self.watcher.read(|item| match item {
            UeventRef::Event(event) => hints_for(&event, names, report),
            UeventRef::Overflow => report(Hint::Any),
        });
        if read.is_err() {
            // Whatever was pending can't be told apart any more.
            report(Hint::Any);
        }
    }
}

impl HintSource for UsbHints {
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
    let number = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    (number(bus) && ports.split('.').all(number)).then_some(last)
}

/// The vendor and product of an event's `PRODUCT` (`46d/825/12`: hex, no leading zeros).
fn product_of(event: &UeventMsg<'_>) -> Option<(u16, u16)> {
    let mut parts = event.get("PRODUCT")?.split('/');
    let vendor = u16::from_str_radix(parts.next()?, 16).ok()?;
    let product = u16::from_str_radix(parts.next()?, 16).ok()?;
    Some((vendor, product))
}

/// Reports each of `names` that `event` may be about.
fn hints_for(event: &UeventMsg<'_>, names: &[String], report: &mut dyn FnMut(Hint<'_>)) {
    if event.subsystem() != Some("usb") {
        return;
    }
    let Some(port) = port_of(event.devpath()) else {
        return;
    };
    let product = product_of(event);
    for name in names {
        let fits = match UsbName::parse(name) {
            Some(UsbName::Port(named)) => named == port,
            Some(UsbName::Id {
                vendor,
                product: id,
            }) => product.is_none_or(|found| found == (vendor, id)),
            None => false,
        };
        if fits {
            report(Hint::Changed(name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotplug::uevent::encode;

    const NAMES: [&str; 5] = [
        "usb:3-1",
        "usb:3-1.4",
        "usb:046D:0825",
        "usb:1a2b:3c4d",
        "i2c-1",
    ];

    fn hints(action: &str, devpath: &str, vars: &[(&str, &str)]) -> Vec<String> {
        let names: Vec<String> = NAMES.iter().map(|name| (*name).to_string()).collect();
        let mut bytes = Vec::new();
        encode(&mut bytes, action, devpath, vars.iter().copied());
        let event = UeventMsg::parse(&bytes).unwrap();
        let mut seen = Vec::new();
        hints_for(&event, &names, &mut |hint| seen.push(format!("{hint:?}")));
        seen
    }

    const USB_DEVICE: [(&str, &str); 2] = [("SUBSYSTEM", "usb"), ("DEVTYPE", "usb_device")];

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
    fn only_usb_names_are_watched_for_and_a_board_with_none_has_no_watcher() {
        let paths = LinuxPaths::new().with_sys_class_root("/nonexistent/sys/class");
        let watcher =
            || DeviceWatcher::rescan(paths.clone().with_sys_bus_root("/nonexistent"), USB);
        let hints = UsbHints::new(watcher(), NAMES).unwrap();
        assert_eq!(
            hints.names(),
            ["usb:046D:0825", "usb:1a2b:3c4d", "usb:3-1", "usb:3-1.4"]
        );
        assert!(UsbHints::new(watcher(), ["i2c-1", "usb:nonsense"]).is_none());
    }
}
