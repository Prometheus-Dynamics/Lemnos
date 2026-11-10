//! What the media graphs say: the link changes that route a MIPI channel to its capture node, and
//! which ISP the receiver feeds. Pure functions over graphs read by the caller, so every rule here
//! is tested without a board.
//!
//! ```text
//! receiver's graph:  csi2 host ─ pad n+1 ─► stream_cif_mipi_id<n>   (one per MIPI channel n)
//! an ISP's graph:    <receiver's model>  ─► rkisp-isp-subdev pad 0  (the receiver's ISP-facing subdev)
//! ```
//!
//! Interface facts, from the vendor kernel's `drivers/media/platform/rockchip/cif` (`dev.h`,
//! `dev.c`, `subdev-itf.c`) and `drivers/media/platform/rockchip/isp/dev.c`, branch `develop-6.1`,
//! read for names and numbers only; no code is taken from them:
//!
//! - The capture entities are named `stream_cif_mipi_id0` to `..._id3` (`CIF_MIPI_ID*_VDEV_NAME`),
//!   and channel `n` is fed from pad `n + 1` of the CSI-2 host (`rkcif_create_links`, which enables
//!   that link for the first sensor when it probes).
//! - The receiver's media device model is its device name (`dev.c`, `media_dev.model`).
//! - The subdevice a receiver hands frames to an ISP through is named after the same device
//!   (`subdev-itf.c`, `rkcif_subdev_media_init` copies `dev_name` of the receiver), and its entity
//!   function is `MEDIA_ENT_F_PROC_VIDEO_COMPOSER`. It shows up in the ISP's graph, not the
//!   receiver's.
//! - The ISP links each such subdevice to its sink, pad 0 of `rkisp-isp-subdev` (`rkisp_create_links`,
//!   `RKISP_ISP_PAD_SINK`), enabling the first and leaving the others off. The ISP's media device
//!   driver name is the ISP's device name.
//!
//! So which ISP a receiver feeds is read off the graphs: no table of chips is involved.
//!
//! Provenance: the routing plan is ported from the dev branch's
//! `crates/lemnos-drivers-rkisp/src/cif.rs`; the ISP rule is new.

use lemnos_core::{Result, Status};
use lemnos_v4l2::media::{DataLink, EntityFunction, LinkFlags, PadRef, Topology};

/// The capture entities, one per MIPI channel (`CIF_MIPI_ID*_VDEV_NAME`).
pub const STREAM_ENTITIES: [&str; 4] = [
    "stream_cif_mipi_id0",
    "stream_cif_mipi_id1",
    "stream_cif_mipi_id2",
    "stream_cif_mipi_id3",
];

/// The ISP's own subdevice in its graph.
pub const ISP_ENTITY: &str = "rkisp-isp-subdev";
/// Its raw input pad (`RKISP_ISP_PAD_SINK`).
pub const ISP_SINK_PAD: u32 = 0;

/// The link changes a route needs, and the link it ends with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Enabled links into the capture node to turn off.
    pub disable: Vec<(PadRef, PadRef)>,
    /// The channel's link, to turn on when it's off.
    pub enable: Option<(PadRef, PadRef)>,
    /// The link that carries the channel once the plan is applied: watch it.
    pub link: u32,
    /// The capture node's entity.
    pub node: u32,
}

/// What routes MIPI `channel` (0 to 3) to its capture node in the receiver's graph `t`. Other
/// links into that node are turned off; other channels are left alone. `NOT_FOUND` for a channel
/// the graph doesn't have or nothing feeds; `BUSY` when an immutable link already feeds the node.
pub fn plan(t: &Topology, channel: u32) -> Result<Plan> {
    let name = usize::try_from(channel)
        .ok()
        .and_then(|c| STREAM_ENTITIES.get(c))
        .ok_or(Status::INVALID_ARGUMENT)?;
    let node = t.entity_by_name(name).ok_or(Status::NOT_FOUND)?.id;
    let links = t.links_to(node);
    let enabled = |l: &&DataLink| l.flags.contains(LinkFlags::ENABLED);
    let pad = channel.checked_add(1).ok_or(Status::INVALID_ARGUMENT)?;
    let candidates = || links.iter().filter(|l| l.source.index == pad);
    // A channel can be linked from several sources: keep the one in use.
    let wanted = candidates()
        .find(enabled)
        .or_else(|| candidates().next())
        .ok_or(Status::NOT_FOUND)?;
    let mut disable = Vec::new();
    for link in links.iter().filter(enabled).filter(|l| l.id != wanted.id) {
        if link.flags.contains(LinkFlags::IMMUTABLE) {
            return Err(Status::BUSY);
        }
        disable.push((link.source, link.sink));
    }
    let enable =
        (!wanted.flags.contains(LinkFlags::ENABLED)).then_some((wanted.source, wanted.sink));
    Ok(Plan {
        disable,
        enable,
        link: wanted.id,
        node,
    })
}

/// Whether the receiver's graph `t` has MIPI capture entities: what tells a receiver's media
/// device from others.
pub fn is_receiver(t: &Topology) -> bool {
    t.entity_by_name(STREAM_ENTITIES[0]).is_some()
}

/// One ISP's graph, as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IspGraph {
    /// The ISP's media device driver name: its device name.
    pub name: String,
    pub topology: Topology,
}

/// An ISP a receiver is wired to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IspLink {
    /// The ISP's name, from [`IspGraph::name`].
    pub isp: String,
    /// The link is on: this ISP takes the receiver's frames. An ISP takes one input at a time.
    pub enabled: bool,
}

/// Every ISP among `isps` that the receiver whose media device model is `receiver` is wired to,
/// in the order given, and whether each link is on. Empty when none is: the receiver feeds no ISP,
/// and its frames go only to its capture nodes.
pub fn feeds(receiver: &str, isps: &[IspGraph]) -> Vec<IspLink> {
    let mut out = Vec::new();
    for graph in isps {
        let t = &graph.topology;
        let Some(isp) = t.entity_by_name(ISP_ENTITY) else {
            continue;
        };
        let wired = t.links_to(isp.id).into_iter().filter(|l| {
            l.sink.index == ISP_SINK_PAD
                && t.entity(l.source.entity).is_some_and(|e| {
                    e.name == receiver && e.function == EntityFunction::PROC_VIDEO_COMPOSER
                })
        });
        // One link per ISP: the receiver's subdevice has one source pad.
        let mut enabled = None;
        for link in wired {
            enabled = Some(enabled.unwrap_or(false) || link.flags.contains(LinkFlags::ENABLED));
        }
        if let Some(enabled) = enabled {
            out.push(IspLink {
                isp: graph.name.clone(),
                enabled,
            });
        }
    }
    out
}

/// The ISP the receiver's frames go to now, from [`feeds`]: the one whose link is on.
pub fn feeding(links: &[IspLink]) -> Option<&str> {
    links.iter().find(|l| l.enabled).map(|l| l.isp.as_str())
}
