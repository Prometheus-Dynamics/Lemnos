//! The stable names of GPIO lines and PWM channels: `gpio:<label>:LINE` and `pwm:<node>:CHANNEL`.
//!
//! The kernel numbers a GPIO or PWM chip as it probes, so `gpiochipN` and `pwmchipN` can change
//! from one boot to the next. A stable name identifies the chip instead, and it is resolved when
//! the device binds:
//!
//! - a GPIO chip by its label, which the kernel reports in the chip's own information
//!   (`GPIO_GET_CHIPINFO_IOCTL`). A Raspberry Pi RP1's chip is `pinctrl-rp1`, a constant in
//!   the driver's source (`drivers/pinctrl/pinctrl-rp1.c`, `MODULE_NAME`);
//! - a PWM chip by its device-tree node: the chip's platform device's `of_node` link, which the
//!   kernel gives as the node's path in the live tree (`/sys/firmware/devicetree/base`). A PWM
//!   chip has no label of its own.
//!
//! The numbered forms still work, for a board that has no generated tree; the tree generator
//! refuses them.

use std::path::{Path, PathBuf};

/// A name split into what it identifies and the line or channel within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stable<'a> {
    /// A GPIO line: the chip's label and the line's offset.
    Gpio { label: &'a str, line: u32 },
    /// A PWM channel: the chip's device-tree node path and the channel.
    Pwm { node: &'a str, channel: u32 },
}

/// Splits a `gpio:` or `pwm:` name. Anything else, and a name with no channel number, is `None`.
pub(crate) fn parse(need: &str) -> Option<Stable<'_>> {
    if let Some(rest) = need.strip_prefix("gpio:") {
        let (label, line) = rest.rsplit_once(':')?;
        if label.is_empty() || label.contains('/') {
            return None;
        }
        return Some(Stable::Gpio {
            label,
            line: line.parse().ok()?,
        });
    }
    let rest = need.strip_prefix("pwm:")?;
    let (node, channel) = rest.rsplit_once(':')?;
    if !node.starts_with('/') || node.contains("..") {
        return None;
    }
    Some(Stable::Pwm {
        node,
        channel: channel.parse().ok()?,
    })
}

/// The number of the GPIO chip whose label is `label`, among `(number, label)` pairs.
pub(crate) fn chip_with_label<'a>(
    chips: impl IntoIterator<Item = (u32, &'a str)>,
    label: &str,
) -> Option<u32> {
    chips
        .into_iter()
        .find_map(|(number, found)| (found == label).then_some(number))
}

/// A GPIO chip found on the machine: its number, its label and its line count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpioChip {
    pub number: u32,
    pub label: String,
    pub lines: u32,
}

/// Every GPIO chip under `dev` (`gpiochipN` character devices), read from the kernel's chip
/// information. A chip that cannot be opened or read is left out.
pub fn gpio_chips(dev: &Path) -> Vec<GpioChip> {
    let Ok(entries) = std::fs::read_dir(dev) else {
        return Vec::new();
    };
    let mut chips = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(number) = name
            .to_str()
            .and_then(|n| n.strip_prefix("gpiochip"))
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        use std::os::fd::AsFd;
        let Ok(file) = std::fs::File::open(entry.path()) else {
            continue;
        };
        let Ok(info) = lemnos_linux_sys::gpio::chip_info(file.as_fd()) else {
            continue;
        };
        let end = info
            .label
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(info.label.len());
        if let Ok(label) = String::from_utf8(info.label[..end].to_vec()) {
            chips.push(GpioChip {
                number,
                label,
                lines: info.lines,
            });
        }
    }
    chips.sort_by_key(|c| c.number);
    chips
}

/// The number of the GPIO chip labelled `label`, found by reading each `/dev/gpiochipN` under
/// `dev`. A chip that cannot be opened or read is skipped.
pub(crate) fn gpio_chip_number(dev: &Path, label: &str) -> Option<u32> {
    let mut chips = Vec::new();
    for entry in std::fs::read_dir(dev).ok()?.flatten() {
        let name = entry.file_name();
        let Some(number) = name
            .to_str()
            .and_then(|n| n.strip_prefix("gpiochip"))
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if let Some(found) = chip_label(&entry.path()) {
            chips.push((number, found));
        }
    }
    chip_with_label(chips.iter().map(|(n, l)| (*n, l.as_str())), label)
}

/// The label a GPIO character device reports, if it can be read.
fn chip_label(path: &Path) -> Option<String> {
    use std::os::fd::AsFd;
    let file = std::fs::File::open(path).ok()?;
    let info = lemnos_linux_sys::gpio::chip_info(file.as_fd()).ok()?;
    let end = info
        .label
        .iter()
        .position(|b| *b == 0)
        .unwrap_or(info.label.len());
    String::from_utf8(info.label[..end].to_vec()).ok()
}

/// The PWM chip directory (`class/pwm/pwmchipN`) whose device-tree node is `node`, found under
/// `sys`. Each chip's `device/of_node` link is resolved and its path taken below the live tree.
pub(crate) fn pwm_chip_dir(sys: &Path, node: &str) -> Option<PathBuf> {
    let live = sys.join("firmware/devicetree/base");
    let chips = sys.join("class/pwm");
    for entry in std::fs::read_dir(&chips).ok()?.flatten() {
        let dir = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("pwmchip") {
            continue;
        }
        let Ok(of_node) = std::fs::canonicalize(dir.join("device/of_node")) else {
            continue;
        };
        let Ok(below) = of_node.strip_prefix(&live) else {
            continue;
        };
        if below.to_str() == Some(node.trim_start_matches('/')) {
            return Some(dir);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_names_split_from_the_right() {
        assert_eq!(
            parse("gpio:pinctrl-rp1:34"),
            Some(Stable::Gpio {
                label: "pinctrl-rp1",
                line: 34
            })
        );
        assert_eq!(
            parse("pwm:/axi/pcie@1000120000/rp1/pwm@9c000:1"),
            Some(Stable::Pwm {
                node: "/axi/pcie@1000120000/rp1/pwm@9c000",
                channel: 1
            })
        );
        assert_eq!(parse("gpio:pinctrl-rp1"), None);
        assert_eq!(parse("gpio::3"), None);
        assert_eq!(parse("gpio:a/b:3"), None);
        assert_eq!(parse("pwm:axi/pwm@9c000:1"), None);
        assert_eq!(parse("pwm:/axi/../x:1"), None);
        assert_eq!(parse("gpiochip0:17"), None);
    }

    #[test]
    fn a_chip_is_found_by_its_label() {
        let chips = [(0, "pinctrl-bcm2712"), (4, "pinctrl-rp1"), (5, "other")];
        assert_eq!(
            chip_with_label(chips.iter().copied(), "pinctrl-rp1"),
            Some(4)
        );
        assert_eq!(chip_with_label(chips.iter().copied(), "pinctrl"), None);
    }

    #[test]
    fn a_pwm_chip_is_found_by_its_device_tree_node() {
        let sys = std::env::temp_dir().join(format!("lemnos-stable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&sys);
        let node = sys.join("firmware/devicetree/base/axi/pcie@1000120000/rp1/pwm@9c000");
        std::fs::create_dir_all(&node).unwrap();
        let device = sys.join("devices/platform/pwm");
        std::fs::create_dir_all(&device).unwrap();
        std::os::unix::fs::symlink(&node, device.join("of_node")).unwrap();
        std::fs::create_dir_all(sys.join("class/pwm/pwmchip7")).unwrap();
        std::os::unix::fs::symlink(&device, sys.join("class/pwm/pwmchip7/device")).unwrap();
        assert_eq!(
            pwm_chip_dir(&sys, "/axi/pcie@1000120000/rp1/pwm@9c000"),
            Some(sys.join("class/pwm/pwmchip7"))
        );
        assert_eq!(
            pwm_chip_dir(&sys, "/axi/pcie@1000120000/rp1/pwm@98000"),
            None
        );
        std::fs::remove_dir_all(&sys).unwrap();
    }
}
