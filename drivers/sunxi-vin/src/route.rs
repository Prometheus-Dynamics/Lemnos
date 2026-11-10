//! Which receiver and which ISP feed the capture node, read from the media graph.
//!
//! The kernel driver builds one media graph for the whole block. A capture node's frames come
//! along a chain of enabled links: sensor, CSI-2 receiver, parser, (time-division unit), ISP,
//! scaler, capture subdevice, video node. Selecting the node's input enables the links from the
//! sensor and from the parser into its ISP; closing the node disables them again. So the answer is
//! read from the graph as it stands, and it is complete while the driver holds the node open.
//!
//! The rule uses only what the graph says about itself, with no table of entity names:
//!
//! - walk upstream from the capture node's entity, one enabled data link at a time;
//! - the ISP is the first entity on the way whose function is an ISP's. The kernel driver gives
//!   its ISP subdevice `MEDIA_ENT_F_PROC_VIDEO_PIXEL_FORMATTER`; a kernel that says
//!   `MEDIA_ENT_F_PROC_VIDEO_ISP` is taken too;
//! - the sensor is the entity the walk ends on when its function is `MEDIA_ENT_F_CAM_SENSOR`;
//! - the receiver is the entity the sensor feeds: the last one before the sensor, or the last one
//!   of the walk when the graph has no sensor.
//!
//! Provenance: interface facts (the link chain, which links selecting the input enables, the ISP
//! entity's function) from the vendor kernel's `drivers/vin`: `vin.c` (`vin_create_media_links`,
//! `vin_setup_default_links`), `vin-video/vin_video.c` (`__vin_s_input`, `__csi_isp_setup_link`,
//! close) and `vin-isp/sunxi_isp.c`. GPL-2.0; no code copied.

use lemnos_v4l2::media::{EntityFunction, LinkFlags, LinkType, Topology};

/// What feeds a capture node, by media entity name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Feeds {
    /// The sensor at the head of the chain, when the graph has one there.
    pub sensor: Option<String>,
    /// The entity the sensor feeds: the CSI-2 receiver.
    pub receiver: Option<String>,
    /// The ISP the frames pass through on the way to the node.
    pub isp: Option<String>,
}

fn is_isp(function: EntityFunction) -> bool {
    function == EntityFunction::PROC_VIDEO_ISP
        || function == EntityFunction::PROC_VIDEO_PIXEL_FORMATTER
}

/// The entity feeding `entity` through an enabled data link, if any. The first one when there
/// are several.
fn upstream(topology: &Topology, entity: u32) -> Option<u32> {
    topology
        .links
        .iter()
        .filter(|l| l.flags.link_type() == LinkType::Data && l.flags.contains(LinkFlags::ENABLED))
        .find_map(|l| {
            let sink = topology.pad(l.sink_id)?;
            if sink.entity_id != entity {
                return None;
            }
            topology.pad(l.source_id).map(|p| p.entity_id)
        })
}

/// What feeds the entity `capture` in `topology`. Bounded by the number of entities, so a graph
/// with a loop in it ends.
#[must_use]
pub fn feeds(topology: &Topology, capture: u32) -> Feeds {
    let mut out = Feeds::default();
    let name = |id: u32| topology.entity(id).map(|e| e.name.clone());
    let mut at = capture;
    let mut last = None;
    for _ in 0..topology.entities.len() {
        let Some(next) = upstream(topology, at) else {
            break;
        };
        if next == capture {
            break;
        }
        let Some(entity) = topology.entity(next) else {
            break;
        };
        if entity.function == EntityFunction::CAM_SENSOR {
            out.sensor = Some(entity.name.clone());
            break;
        }
        if out.isp.is_none() && is_isp(entity.function) {
            out.isp = Some(entity.name.clone());
        }
        last = Some(next);
        at = next;
    }
    out.receiver = last.and_then(name).filter(|n| Some(n) != out.isp.as_ref());
    out
}
