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

use lemnos_host::board::BoardFile;

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
