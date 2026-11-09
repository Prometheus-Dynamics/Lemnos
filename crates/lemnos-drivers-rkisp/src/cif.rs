//! The vendor `rkcif` capture block's media graph: which media devices each chip has, and the one
//! call that routes a MIPI channel to its capture node (feature `device`).
//!
//! ```text
//! sensor ─► rockchip-mipi-csi2 ─ pad 1 ─► stream_cif_mipi_id0  (a capture node)
//!                              ─ pad 2 ─► stream_cif_mipi_id1
//!                              ...
//! ```
//!
//! From `drivers/media/platform/rockchip/cif` of rockchip-linux/kernel `develop-6.1` (`dev.c`,
//! `dev.h`) and the chips' `.dtsi` files. The stream names and the pad rule are the same on every
//! chip; only the media devices differ. The driver enables each channel's link for its first
//! sensor when it probes, so on a one-camera board the call usually has nothing to change, and
//! [`crate::device::IspDevice`] needs no call: it takes whatever feeds the ISP. Written from the
//! driver's source, untested on board.

use std::format;
use std::path::{Path, PathBuf};
use std::vec::Vec;

use lemnos_v4l2::media::{LinkFlags, MediaDevice, PadRef, Topology, find_media_devices};

use crate::Isp;
use crate::device::{DeviceError, Result};

/// The capture entities, one per MIPI channel (`CIF_MIPI_ID*_VDEV_NAME`).
pub const STREAM_ENTITIES: [&str; 4] = [
    "stream_cif_mipi_id0",
    "stream_cif_mipi_id1",
    "stream_cif_mipi_id2",
    "stream_cif_mipi_id3",
];

/// The model names of the MIPI media devices a chip's description has (each is its device tree
/// node's name), or none for an ISP whose chips this table does not cover.
#[must_use]
pub fn models(isp: Isp) -> &'static [&'static str] {
    match isp {
        Isp::Isp21 => &["rkcif_mipi_lvds"],
        Isp::Isp30 => &[
            "rkcif-mipi-lvds",
            "rkcif-mipi-lvds1",
            "rkcif-mipi-lvds2",
            "rkcif-mipi-lvds3",
            "rkcif-mipi-lvds4",
            "rkcif-mipi-lvds5",
        ],
        Isp::Isp35 => &[
            "rkcif-mipi-lvds",
            "rkcif-mipi-lvds1",
            "rkcif-mipi-lvds2",
            "rkcif-mipi-lvds3",
        ],
        Isp::Isp32 | Isp::Isp32Lite => &[],
    }
}

/// The MIPI capture media devices of a chip with this ISP, in path order.
#[must_use]
pub fn find_cif(isp: Isp) -> Vec<PathBuf> {
    let models = models(isp);
    find_media_devices(|m| {
        m.device_info()
            .is_ok_and(|i| models.contains(&i.model.as_str()))
    })
}

/// The link changes a route needs.
#[derive(Debug, PartialEq, Eq)]
struct Plan {
    disable: Vec<(PadRef, PadRef)>,
    enable: Option<(PadRef, PadRef)>,
    node: u32,
}

fn plan(t: &Topology, channel: u32) -> Result<Plan> {
    let name = usize::try_from(channel)
        .ok()
        .and_then(|c| STREAM_ENTITIES.get(c))
        .ok_or_else(|| DeviceError::Setup(format!("no MIPI channel {channel}")))?;
    let node = t
        .entity_by_name(name)
        .ok_or_else(|| DeviceError::Setup(format!("no '{name}' entity")))?
        .id;
    let links = t.links_to(node);
    let enabled = |l: &&lemnos_v4l2::media::DataLink| l.flags.contains(LinkFlags::ENABLED);
    let candidates = || links.iter().filter(|l| l.source.index == channel + 1);
    // A channel can be linked from several sensors: keep the one in use.
    let wanted = candidates()
        .find(enabled)
        .or_else(|| candidates().next())
        .ok_or_else(|| {
            DeviceError::Setup(format!("nothing feeds '{name}' from pad {}", channel + 1))
        })?;
    let mut disable = Vec::new();
    for link in links.iter().filter(enabled).filter(|l| l.id != wanted.id) {
        if link.flags.contains(LinkFlags::IMMUTABLE) {
            return Err(DeviceError::Setup(format!(
                "an immutable link already feeds '{name}'"
            )));
        }
        disable.push((link.source, link.sink));
    }
    let enable =
        (!wanted.flags.contains(LinkFlags::ENABLED)).then_some((wanted.source, wanted.sink));
    Ok(Plan {
        disable,
        enable,
        node,
    })
}

/// Routes MIPI `channel` (0 to 3) of the media device `media` to its capture node, and returns
/// the node's path. Other links into that node are turned off; other channels are left alone.
pub fn enable_stream(media: impl AsRef<Path>, channel: u32) -> Result<PathBuf> {
    let media = MediaDevice::open(media)?;
    let t = media.topology()?;
    let plan = plan(&t, channel)?;
    for (source, sink) in plan.disable {
        media.setup_link(source, sink, LinkFlags::empty())?;
    }
    if let Some((source, sink)) = plan.enable {
        media.setup_link(source, sink, LinkFlags::ENABLED)?;
    }
    t.devnode_path(plan.node)
        .ok_or_else(|| DeviceError::Setup(format!("channel {channel} has no device node")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;
    use std::vec;

    use lemnos_v4l2::media::{Entity, EntityFlags, EntityFunction, Link, Pad, PadFlags};

    fn entity(id: u32, name: &str) -> Entity {
        Entity {
            id,
            name: name.to_string(),
            function: EntityFunction(0),
            flags: EntityFlags::empty(),
        }
    }

    fn pad(id: u32, entity_id: u32, index: u32) -> Pad {
        Pad {
            id,
            entity_id,
            flags: PadFlags::empty(),
            index,
        }
    }

    const STREAM0: u32 = 10;
    const CSI_A: u32 = 20;
    const CSI_B: u32 = 21;
    const SINK: u32 = 100;
    const A1: u32 = 101;
    const A2: u32 = 102;
    const B1: u32 = 103;

    fn graph(a1: LinkFlags, a2: LinkFlags, b1: LinkFlags) -> Topology {
        let link = |id, source_id, flags| Link {
            id,
            source_id,
            sink_id: SINK,
            flags,
        };
        Topology {
            version: 1,
            entities: vec![
                entity(STREAM0, STREAM_ENTITIES[0]),
                entity(CSI_A, "rockchip-mipi-csi2"),
                entity(CSI_B, "other"),
            ],
            interfaces: vec![],
            pads: vec![
                pad(SINK, STREAM0, 0),
                pad(A1, CSI_A, 1),
                pad(A2, CSI_A, 2),
                pad(B1, CSI_B, 1),
            ],
            links: vec![link(1, A1, a1), link(2, A2, a2), link(3, B1, b1)],
        }
    }

    const ON: LinkFlags = LinkFlags::ENABLED;
    const OFF: LinkFlags = LinkFlags::empty();

    #[test]
    fn a_route_already_enabled_changes_nothing() {
        let plan = plan(&graph(ON, OFF, OFF), 0).expect("plan");
        assert_eq!(plan.enable, None);
        assert!(plan.disable.is_empty());
        assert_eq!(plan.node, STREAM0);
    }

    #[test]
    fn a_disabled_route_is_enabled_from_the_channels_pad() {
        let plan = plan(&graph(OFF, OFF, OFF), 0).expect("plan");
        let (source, _) = plan.enable.expect("a link to enable");
        assert_eq!((source.entity, source.index), (CSI_A, 1));
    }

    #[test]
    fn another_link_into_the_node_is_turned_off() {
        let plan = plan(&graph(OFF, ON, OFF), 0).expect("plan");
        assert_eq!(plan.disable.len(), 1);
        assert_eq!(plan.disable[0].0.index, 2);
        assert!(plan.enable.is_some());
    }

    #[test]
    fn the_sensor_in_use_is_kept() {
        let plan = plan(&graph(OFF, OFF, ON), 0).expect("plan");
        assert_eq!(plan.enable, None);
        assert!(plan.disable.is_empty());
    }

    #[test]
    fn an_immutable_link_in_the_way_is_an_error() {
        let flags = LinkFlags::ENABLED | LinkFlags::IMMUTABLE;
        assert!(plan(&graph(OFF, flags, OFF), 0).is_err());
    }

    #[test]
    fn a_missing_channel_or_entity_is_an_error() {
        assert!(plan(&graph(ON, OFF, OFF), 4).is_err());
        assert!(plan(&graph(ON, OFF, OFF), u32::MAX).is_err());
        assert!(plan(&graph(ON, OFF, OFF), 1).is_err());
    }

    #[test]
    fn each_chip_lists_its_media_devices() {
        assert_eq!(models(Isp::Isp21).len(), 1);
        assert_eq!(models(Isp::Isp30).len(), 6);
        assert_eq!(models(Isp::Isp35).len(), 4);
        assert!(models(Isp::Isp32).is_empty());
    }
}
