//! The device permissions the daemon needs, as one list of facts. Two renderers read it: `udev`
//! writes the rules of a systemd image (`lemnos-ctl tree BOARD --udev FILE`), and `mdev` writes the
//! `/etc/mdev.conf` lines of a BusyBox image (`lemnos-ctl tree BOARD --mdev FILE`). Nothing else
//! states a board's permissions: a change to a fact changes both outputs, and the test that parses
//! both back (`tests/permissions_agree.rs`) fails when they differ.
//!
//! The daemon drops to user `lemnos` after its root phase and opens its nodes as that user, so each
//! node it needs must be readable and writable by [`DAEMON_GROUP`]. The packaged kinds (serial ports,
//! buses, cameras, usbfs, the watchdog) are not here: they are the packaged rules' own, in
//! `packaging/udev/70-lemnos.rules` and `packaging/init/mdev.conf.fragment`. These facts are the
//! board's own list: the kernel module's nodes, and the dma-heaps the board's drivers allocate from.
//!
//! Nothing here knows a kind of hardware beyond the daemon's own nodes.

use lemnos_host::board::{BoardFile, Owner};

/// The group the daemon runs as, and the group its device nodes are given to. It is the group of
/// the `lemnos` user in `packaging/systemd/lemnos.conf` (`u lemnos`), so the two are one name.
pub const DAEMON_GROUP: &str = "lemnos";
/// The mode the daemon's nodes are given: owner and group read and write, nothing for others.
pub const NODE_MODE: &str = "0660";
/// The owner the daemon's nodes are left with: the kernel's default, root.
pub const NODE_OWNER: &str = "root";

/// How a fact names the kernel devices it covers, within its subsystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Name {
    /// Exactly this kernel name.
    Exact(String),
    /// Kernel names matching a glob, where `*` stands for any run of characters (udev's `KERNEL==`
    /// reading). No other character is special.
    Glob(String),
}

/// One permission the daemon needs: the nodes of one kernel subsystem with a name, given an owner,
/// a group and a mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    /// The kernel subsystem the devices belong to (`SUBSYSTEM` in udev).
    pub subsystem: &'static str,
    /// The directory under `/dev` the kernel creates the node in: `""` for the root of `/dev`,
    /// `"dma_heap/"` for a dma-heap. Both renderers write the node's path from this and `name`.
    pub dir: &'static str,
    /// The kernel name, or the pattern of kernel names, within the subsystem.
    pub name: Name,
    pub owner: &'static str,
    pub group: &'static str,
    pub mode: &'static str,
}

impl Fact {
    /// A fact that gives the daemon's group the nodes, with the owner and mode every daemon node has.
    fn daemon(subsystem: &'static str, dir: &'static str, name: Name) -> Self {
        Self {
            subsystem,
            dir,
            name,
            owner: NODE_OWNER,
            group: DAEMON_GROUP,
            mode: NODE_MODE,
        }
    }
}

/// The facts a renderer writes under one heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The heading the renderers print above the facts, saying what they cover and where it came from.
    pub title: String,
    pub facts: Vec<Fact>,
}

/// The sections for a board, in the order the renderers print them.
#[must_use]
pub fn sections(board: &BoardFile) -> Vec<Section> {
    let mut out = vec![module_nodes()];
    out.extend(firmware_mailbox(board));
    out.extend(dma_heaps(board));
    out
}

/// The firmware mailbox the daemon clears the boot count through, on a board whose slot record is
/// `tryboot` (the default, as the slot driver reads it): the daemon's group gets `/dev/vcio`, the
/// misc device the firmware's property mailbox is on. A board with another record has no such node.
fn firmware_mailbox(board: &BoardFile) -> Option<Section> {
    let slots = board.devices.iter().find(|d| d.driver == "system-slots")?;
    let record = slots
        .config
        .get("record")
        .and_then(|value| value.as_str())
        .unwrap_or("tryboot");
    if record != "tryboot" {
        return None;
    }
    Some(Section {
        title: "The firmware mailbox the boot count is cleared through (/dev/vcio, a misc device): the slot driver's tryboot record."
            .to_owned(),
        facts: vec![Fact::daemon("misc", "", Name::Exact("vcio".to_owned()))],
    })
}

/// The kernel module's control node and the receiver nodes it makes: `/dev/lemnos` and
/// `/dev/lemnos-<receiver>`, both misc devices named by the module.
fn module_nodes() -> Section {
    Section {
        title: "The kernel module's nodes: /dev/lemnos, and /dev/lemnos-<receiver> for each receiver the module owns (misc devices named by the module)."
            .to_owned(),
        facts: vec![
            Fact::daemon("misc", "", Name::Exact("lemnos".to_owned())),
            Fact::daemon("misc", "", Name::Glob("lemnos-*".to_owned())),
        ],
    }
}

/// The dma-heaps the board's drivers allocate from, one fact each (`BoardFile::dma_heaps`).
fn dma_heaps(board: &BoardFile) -> Option<Section> {
    let heaps = board.dma_heaps();
    if heaps.is_empty() {
        return None;
    }
    let facts = heaps
        .into_iter()
        .map(|heap| Fact::daemon("dma_heap", "dma_heap/", Name::Exact(heap)))
        .collect();
    Some(Section {
        title:
            "The dma-heaps the board's drivers allocate from: the board file's [host] dma_heaps \
                (default linux,cma), and every dma-buf stream's heap. Each is /dev/dma_heap/<name>."
                .to_owned(),
        facts,
    })
}

/// A late grant (HOTPLUG-GRANT): the attributes the daemon writes after its drop, on one fan or one PWM channel the
/// board names, handed to the daemon's group when the device appears (a hwmon fan) or is exported (a PWM channel).
/// Only the board's own devices, and only enabled ones that the daemon drives (`owner` is not `host`). The packaged
/// files hold none: a late grant is the board's, so the board's fragment is the only place it is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Late {
    /// A hwmon fan found by its `name` attribute (the board's `hwmon-fan` config `name`).
    HwmonName(String),
    /// A hwmon fan by its directory (the board's `hwmon-fan` config `hwmon`), as the board names it.
    HwmonDir(String),
    /// A PWM channel on the controller at a device-tree node (the board's `pwm:NODE:CH`, `node` as the board
    /// writes it, from `/`). Its chip is found by the node its parent device names (`device/of_node`), since the
    /// kernel's chip number is probe order.
    PwmChannel { node: String, channel: u32 },
}

/// The attributes a fan's late grant gives the daemon: its duty and the mode that makes the duty take effect.
pub const HWMON_ATTRIBUTES: [&str; 2] = ["pwm1", "pwm1_enable"];
/// The attributes a channel's late grant gives the daemon: the four it writes after its drop. `export` and
/// `unexport` stay root's.
pub const CHANNEL_ATTRIBUTES: [&str; 4] = ["period", "duty_cycle", "polarity", "enable"];

impl Late {
    /// The attributes this grant covers, in the order the renderers write them.
    #[must_use]
    pub fn attributes(&self) -> &'static [&'static str] {
        match self {
            Self::HwmonName(_) | Self::HwmonDir(_) => &HWMON_ATTRIBUTES,
            Self::PwmChannel { .. } => &CHANNEL_ATTRIBUTES,
        }
    }
}

/// A name a renderer may write into a shell word or a udev value unescaped: letters, digits, `_`, `-` and `.`.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// A device-tree node name as a board writes it, for a shell word and a udev value: `/`, `@`, `,`, `.`, `_`, `-`,
/// letters and digits.
fn node_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '/' | '@' | ',' | '.' | '_' | '-')
}

/// The late grants of a board: one per enabled, Lemnos-driven hwmon fan the board names by `name` or `hwmon`, and
/// one per PWM channel the board names by its controller's node (`pwm:NODE:CH`) on an enabled, Lemnos-driven
/// `pwm-channel-fan`. A name the renderers could not write into a shell word or a udev value gets none. The
/// board's own generator refuses a `pwmchipN:CH` name, so no grant is made for it.
#[must_use]
pub fn late_grants(board: &BoardFile) -> Vec<Late> {
    let mut out = Vec::new();
    for device in &board.devices {
        if device.disabled || device.owner == Owner::Host {
            continue;
        }
        match device.driver.as_str() {
            "hwmon-fan" => {
                let name = device.config.get("name").and_then(|v| v.as_str());
                let dir = device.config.get("hwmon").and_then(|v| v.as_str());
                match (name, dir) {
                    (Some(name), _) if plain(name) => out.push(Late::HwmonName(name.to_owned())),
                    (None, Some(dir)) if plain(dir) => out.push(Late::HwmonDir(dir.to_owned())),
                    _ => {}
                }
            }
            "pwm-channel-fan" => {
                let Some(name) = device.uses.get("pwm") else {
                    continue;
                };
                let Some((node, channel)) =
                    name.strip_prefix("pwm:").and_then(|r| r.rsplit_once(':'))
                else {
                    continue;
                };
                let Ok(channel) = channel.parse::<u32>() else {
                    continue;
                };
                if node.starts_with('/') && node.chars().all(node_char) {
                    out.push(Late::PwmChannel {
                        node: node.to_owned(),
                        channel,
                    });
                }
            }
            _ => {}
        }
    }
    out
}
