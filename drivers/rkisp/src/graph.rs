//! The ISP path in the ISP's media graph: what feeds the ISP, the links to turn on, and the nodes
//! that take parameters and give statistics and images. Pure functions over a graph the caller
//! read, so every rule is tested without a board.
//!
//! ```text
//! <source> ─────────────────────► rkisp-isp-subdev pad 0      (raw in: a receiver's subdevice)
//! rkisp-input-params pad 0 ─────► rkisp-isp-subdev pad 1      (parameters in)
//! rkisp-isp-subdev pad 2 ───────► rkisp_mainpath pad 0        (output 0)
//!                        └──────► rkisp_selfpath pad 0        (output 1, optional)
//! rkisp-isp-subdev pad 3 ───────► rkisp-statistics pad 0      (statistics out)
//! ```
//!
//! Interface facts, from the vendor kernel's `drivers/media/platform/rockchip/isp` (`rkisp.h`
//! for the pads, `isp_params.c`, `isp_stats.c` and `capture.h` for the node names), the same for
//! ISP21, ISP30 and ISP35, as the dev branch's `device.rs` read them; no code is taken from them.
//! The source is the subdevice a receiver exposes in the ISP's graph, named after the receiver's
//! media device model (see the receiver driver's `route` module); the vendor driver enables the
//! first such link when it probes.

use lemnos_core::{Result, Status};
use lemnos_v4l2::media::{DataLink, LinkFlags, PadRef, Topology};

/// The ISP's own subdevice.
pub const ISP_ENTITY: &str = "rkisp-isp-subdev";
/// The parameter node (a meta output).
pub const PARAMS_ENTITY: &str = "rkisp-input-params";
/// The statistics node (a meta capture).
pub const STATISTICS_ENTITY: &str = "rkisp-statistics";
/// The main path's node: output 0.
pub const MAINPATH_ENTITY: &str = "rkisp_mainpath";
/// The self path's node: output 1, a second scaled image of the same frame.
pub const SELFPATH_ENTITY: &str = "rkisp_selfpath";

/// The ISP's raw input pad (`RKISP_ISP_PAD_SINK`).
pub const PAD_SINK: u32 = 0;
/// Its parameter pad (`RKISP_ISP_PAD_SINK_PARAMS`).
pub const PAD_PARAMS: u32 = 1;
/// Its image pad (`RKISP_ISP_PAD_SOURCE_PATH`).
pub const PAD_SOURCE: u32 = 2;
/// Its statistics pad (`RKISP_ISP_PAD_SOURCE_STATS`).
pub const PAD_STATISTICS: u32 = 3;

/// The entities of the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nodes {
    pub isp: u32,
    pub params: u32,
    pub statistics: u32,
    pub main: u32,
    /// Only when the self path was asked for.
    pub self_path: Option<u32>,
}

/// The link changes that set the path up, and what it ends with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The pad that feeds the ISP's raw input once the plan is applied: its format must match
    /// the ISP's sink.
    pub source: PadRef,
    /// Enabled links into the ISP's raw input from another source, to turn off first: the ISP
    /// takes one input.
    pub disable: Vec<(PadRef, PadRef)>,
    /// Links the path needs that are off, to turn on.
    pub enable: Vec<(PadRef, PadRef)>,
    pub nodes: Nodes,
}

fn entity(t: &Topology, name: &str) -> Result<u32> {
    t.entity_by_name(name)
        .map(|e| e.id)
        .ok_or(Status::NOT_FOUND)
}

fn on(l: &DataLink) -> bool {
    l.flags.contains(LinkFlags::ENABLED)
}

/// The path through the ISP graph `t`. `source` names the entity that should feed the ISP (a
/// receiver's subdevice, named after its media device model); `None` keeps whatever feeds it now,
/// or the first source wired to it. `self_path` adds output 1.
///
/// `NOT_FOUND` for an entity, a source or a link the graph doesn't have; `BUSY` when an immutable
/// link from another source holds the ISP's input.
pub fn plan(t: &Topology, source: Option<&str>, self_path: bool) -> Result<Plan> {
    let isp = entity(t, ISP_ENTITY)?;
    let nodes = Nodes {
        isp,
        params: entity(t, PARAMS_ENTITY)?,
        statistics: entity(t, STATISTICS_ENTITY)?,
        main: entity(t, MAINPATH_ENTITY)?,
        self_path: if self_path {
            Some(entity(t, SELFPATH_ENTITY)?)
        } else {
            None
        },
    };

    let inputs: Vec<DataLink> = t
        .links_to(isp)
        .into_iter()
        .filter(|l| l.sink.index == PAD_SINK)
        .collect();
    let wanted = match source {
        Some(name) => {
            let from = entity(t, name)?;
            let mut wired = inputs.iter().filter(|l| l.source.entity == from);
            wired
                .clone()
                .find(|l| on(l))
                .or_else(|| wired.next())
                .ok_or(Status::NOT_FOUND)?
        }
        None => inputs
            .iter()
            .find(|l| on(l))
            .or_else(|| inputs.first())
            .ok_or(Status::NOT_FOUND)?,
    };
    let mut disable = Vec::new();
    for link in inputs.iter().filter(|l| on(l) && l.id != wanted.id) {
        if link.flags.contains(LinkFlags::IMMUTABLE) {
            return Err(Status::BUSY);
        }
        disable.push((link.source, link.sink));
    }

    let pad = |entity, index| PadRef { entity, index };
    let mut needed = vec![
        (wanted.source, wanted.sink),
        (pad(nodes.params, 0), pad(isp, PAD_PARAMS)),
        (pad(isp, PAD_SOURCE), pad(nodes.main, 0)),
        (pad(isp, PAD_STATISTICS), pad(nodes.statistics, 0)),
    ];
    if let Some(sp) = nodes.self_path {
        needed.push((pad(isp, PAD_SOURCE), pad(sp, 0)));
    }
    let links = t.data_links();
    let mut enable = Vec::new();
    for (from, to) in needed {
        let link = links
            .iter()
            .find(|l| l.source == from && l.sink == to)
            .ok_or(Status::NOT_FOUND)?;
        if !on(link) {
            enable.push((from, to));
        }
    }
    Ok(Plan {
        source: wanted.source,
        disable,
        enable,
        nodes,
    })
}
