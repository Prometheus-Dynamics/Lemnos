//! The driver and its routing rules against graphs written out by hand, shaped as the vendor
//! driver builds them (see `route`): every expected value here is written out by hand.

use std::sync::{Arc, Mutex};

use lemnos_contract_camera_receiver::{CameraReceiver, FrameLayout, LinkConfig, Packing};
use lemnos_core::{Health, Lifecycle, Result, Status};
use lemnos_linux_sys::media::media_v2_link;
use lemnos_linux_sys::v4l2::device::zeroed;
use lemnos_measure::test_alloc::allocations;
use lemnos_v4l2::media::{
    Entity, EntityFlags, EntityFunction, Link, LinkFlags, Pad, PadFlags, PadRef, Topology,
};

use crate::hw::Hardware;
use crate::route::{self, ISP_ENTITY, IspGraph, IspLink, STREAM_ENTITIES};
use crate::{Rkcif, backoff_ns, link_on};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

const ON: LinkFlags = LinkFlags::ENABLED;
const OFF: LinkFlags = LinkFlags::empty();

fn entity(id: u32, name: &str, function: EntityFunction) -> Entity {
    Entity {
        id,
        name: name.to_string(),
        function,
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

fn link(id: u32, source_id: u32, sink_id: u32, flags: LinkFlags) -> Link {
    Link {
        id,
        source_id,
        sink_id,
        flags,
    }
}

// The receiver's graph: a CSI-2 host whose pads 1 and 2 each feed both capture nodes, as the
// vendor driver links them, and a second source into channel 0.
const STREAM0: u32 = 10;
const STREAM1: u32 = 11;
const CSI_A: u32 = 20;
const CSI_B: u32 = 21;
const SINK0: u32 = 100;
const SINK1: u32 = 101;
const A1: u32 = 110;
const A2: u32 = 111;
const B1: u32 = 112;
/// Link ids: host pad 1 to channel 0, host pad 2 to channel 0, the other source's pad 1 to
/// channel 0, host pad 1 to channel 1, host pad 2 to channel 1.
const A1_TO_0: u32 = 1;
const A2_TO_0: u32 = 2;
const B1_TO_0: u32 = 3;
const A1_TO_1: u32 = 4;
const A2_TO_1: u32 = 5;

fn receiver(a1: LinkFlags, a2: LinkFlags, b1: LinkFlags) -> Topology {
    Topology {
        version: 1,
        entities: vec![
            entity(STREAM0, STREAM_ENTITIES[0], EntityFunction::IO_V4L),
            entity(STREAM1, STREAM_ENTITIES[1], EntityFunction::IO_V4L),
            entity(CSI_A, "rockchip-mipi-csi2", EntityFunction::VID_IF_BRIDGE),
            entity(CSI_B, "other", EntityFunction::VID_IF_BRIDGE),
        ],
        interfaces: vec![],
        pads: vec![
            pad(SINK0, STREAM0, 0),
            pad(SINK1, STREAM1, 0),
            pad(A1, CSI_A, 1),
            pad(A2, CSI_A, 2),
            pad(B1, CSI_B, 1),
        ],
        links: vec![
            link(A1_TO_0, A1, SINK0, a1),
            link(A2_TO_0, A2, SINK0, a2),
            link(B1_TO_0, B1, SINK0, b1),
            link(A1_TO_1, A1, SINK1, OFF),
            link(A2_TO_1, A2, SINK1, ON),
        ],
    }
}

/// The model the receiver's media device reports, and its subdevice's name in an ISP's graph.
const MODEL: &str = "rkcif-mipi-lvds2";

const ISP: u32 = 1;
const ISP_SINK: u32 = 50;
const ISP_PARAMS: u32 = 51;

/// An ISP's graph with the given sources, each `(name, function, pad index on the ISP, flags)`.
fn isp(name: &str, sources: &[(&str, EntityFunction, u32, LinkFlags)]) -> IspGraph {
    let mut t = Topology {
        version: 1,
        entities: vec![entity(ISP, ISP_ENTITY, EntityFunction(0x4009))],
        interfaces: vec![],
        pads: vec![pad(ISP_SINK, ISP, 0), pad(ISP_PARAMS, ISP, 1)],
        links: vec![],
    };
    for (n, (source, function, isp_pad, flags)) in (0u32..).zip(sources) {
        let id = 10 + n;
        let source_pad = 200 + n;
        t.entities.push(entity(id, source, *function));
        t.pads.push(pad(source_pad, id, 0));
        let sink = if *isp_pad == 0 { ISP_SINK } else { ISP_PARAMS };
        t.links.push(link(300 + n, source_pad, sink, *flags));
    }
    IspGraph {
        name: name.to_string(),
        topology: t,
    }
}

const COMPOSER: EntityFunction = EntityFunction::PROC_VIDEO_COMPOSER;

#[derive(Default)]
struct Mock {
    topology: Topology,
    isps: Vec<IspGraph>,
    /// Every link change, as `(source, sink, on)`.
    changes: Vec<(PadRef, PadRef, bool)>,
    watched: Option<u32>,
    closed: u32,
    fail_topology: Option<Status>,
}

#[derive(Clone, Default)]
struct Hw(Arc<Mutex<Mock>>);

impl Hw {
    fn with<R>(&self, f: impl FnOnce(&mut Mock) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }

    /// Turns link `id` on or off behind the driver's back.
    fn set(&self, id: u32, flags: LinkFlags) {
        self.with(|m| {
            m.topology
                .links
                .iter_mut()
                .find(|l| l.id == id)
                .unwrap()
                .flags = flags;
        });
    }

    fn is_on(&self, id: u32) -> bool {
        self.with(|m| {
            m.topology
                .links
                .iter()
                .find(|l| l.id == id)
                .unwrap()
                .flags
                .contains(ON)
        })
    }
}

/// The pad with this entity and index, by its graph id.
fn pad_id(t: &Topology, at: PadRef) -> u32 {
    t.pads
        .iter()
        .find(|p| p.entity_id == at.entity && p.index == at.index)
        .unwrap()
        .id
}

impl Hardware for Hw {
    fn topology(&mut self) -> Result<Topology> {
        self.with(|m| match m.fail_topology {
            Some(status) => Err(status),
            None => Ok(m.topology.clone()),
        })
    }

    fn model(&mut self) -> Result<String> {
        Ok(MODEL.to_string())
    }

    fn setup_link(&mut self, source: PadRef, sink: PadRef, enable: bool) -> Result<()> {
        self.with(|m| {
            m.changes.push((source, sink, enable));
            let (s, k) = (pad_id(&m.topology, source), pad_id(&m.topology, sink));
            let l = m
                .topology
                .links
                .iter_mut()
                .find(|l| l.source_id == s && l.sink_id == k)
                .unwrap();
            l.flags = if enable { ON } else { OFF };
        });
        Ok(())
    }

    fn watch(&mut self, link: u32) -> Result<()> {
        self.with(|m| m.watched = Some(link));
        self.check()
    }

    fn check(&mut self) -> Result<()> {
        self.with(|m| {
            let id = m.watched.ok_or(Status::IO)?;
            match m.topology.links.iter().find(|l| l.id == id) {
                Some(l) if l.flags.contains(ON) && m.fail_topology.is_none() => Ok(()),
                _ => Err(Status::IO),
            }
        })
    }

    fn close(&mut self) {
        self.with(|m| {
            m.closed += 1;
            m.watched = None;
        });
    }

    fn isps(&mut self) -> Result<Vec<IspGraph>> {
        Ok(self.with(|m| m.isps.clone()))
    }
}

fn driver_on(topology: Topology) -> (Rkcif, Hw) {
    let hw = Hw::default();
    hw.with(|m| m.topology = topology);
    (Rkcif::with_hardware(Box::new(hw.clone())), hw)
}

fn driver() -> (Rkcif, Hw) {
    driver_on(receiver(ON, OFF, OFF))
}

/// A sensor's mode: 1280x800 RAW10 over 2 lanes at 800 Mbit/s each, virtual channel 0.
fn raw10() -> LinkConfig {
    LinkConfig {
        lanes: 2,
        virtual_channel: 0,
        data_type: 0x2b,
        bit_depth: 10,
        continuous_clock: false,
        width: 1280,
        height: 800,
        pixel_rate_hz: 80_000_000,
        link_rate_mbps: 800,
    }
}

// Routing in the receiver's graph.

#[test]
fn a_route_already_on_changes_nothing() {
    let plan = route::plan(&receiver(ON, OFF, OFF), 0).unwrap();
    assert_eq!(plan.enable, None);
    assert!(plan.disable.is_empty());
    assert_eq!((plan.link, plan.node), (A1_TO_0, STREAM0));
}

#[test]
fn a_route_that_is_off_is_turned_on_from_the_channels_pad() {
    let plan = route::plan(&receiver(OFF, OFF, OFF), 0).unwrap();
    let (source, sink) = plan.enable.unwrap();
    assert_eq!((source.entity, source.index), (CSI_A, 1));
    assert_eq!((sink.entity, sink.index), (STREAM0, 0));
    assert_eq!(plan.link, A1_TO_0);
}

#[test]
fn another_link_into_the_node_is_turned_off() {
    let plan = route::plan(&receiver(OFF, ON, OFF), 0).unwrap();
    assert_eq!(plan.disable.len(), 1);
    assert_eq!(
        (plan.disable[0].0.entity, plan.disable[0].0.index),
        (CSI_A, 2)
    );
    assert_eq!(plan.link, A1_TO_0);
}

#[test]
fn the_source_in_use_is_kept() {
    let plan = route::plan(&receiver(OFF, OFF, ON), 0).unwrap();
    assert_eq!(plan.enable, None);
    assert!(plan.disable.is_empty());
    assert_eq!(plan.link, B1_TO_0);
}

#[test]
fn channel_one_is_fed_from_pad_two() {
    let plan = route::plan(&receiver(ON, OFF, OFF), 1).unwrap();
    assert_eq!((plan.link, plan.node), (A2_TO_1, STREAM1));
    assert_eq!(plan.enable, None);
}

#[test]
fn an_immutable_link_in_the_way_is_busy() {
    assert_eq!(
        route::plan(&receiver(OFF, ON | LinkFlags::IMMUTABLE, OFF), 0).unwrap_err(),
        Status::BUSY
    );
}

#[test]
fn a_missing_channel_or_source_is_refused() {
    let t = receiver(ON, OFF, OFF);
    assert_eq!(route::plan(&t, 4).unwrap_err(), Status::INVALID_ARGUMENT);
    assert_eq!(
        route::plan(&t, u32::MAX).unwrap_err(),
        Status::INVALID_ARGUMENT
    );
    // The graph has no capture node for channel 2.
    assert_eq!(route::plan(&t, 2).unwrap_err(), Status::NOT_FOUND);
    // Channel 0's node with nothing on pad 1 feeding it.
    let mut t = receiver(ON, OFF, OFF);
    t.links.retain(|l| l.sink_id != SINK0 || l.id == A2_TO_0);
    assert_eq!(route::plan(&t, 0).unwrap_err(), Status::NOT_FOUND);
}

#[test]
fn a_receiver_graph_is_told_by_its_capture_nodes() {
    assert!(route::is_receiver(&receiver(ON, OFF, OFF)));
    assert!(!route::is_receiver(&isp("rkisp-vir0", &[]).topology));
}

// Which ISP the receiver feeds.

#[test]
fn feeds_names_the_isp_whose_link_is_on() {
    let isps = [
        isp("rkisp-vir0", &[(MODEL, COMPOSER, 0, OFF)]),
        isp("rkisp-vir1", &[(MODEL, COMPOSER, 0, ON)]),
    ];
    let links = route::feeds(MODEL, &isps);
    assert_eq!(
        links,
        vec![
            IspLink {
                isp: "rkisp-vir0".into(),
                enabled: false
            },
            IspLink {
                isp: "rkisp-vir1".into(),
                enabled: true
            },
        ]
    );
    assert_eq!(route::feeding(&links), Some("rkisp-vir1"));
}

#[test]
fn feeds_ignores_other_receivers_and_other_inputs() {
    let isps = [
        // Another receiver's subdevice, on.
        isp("rkisp-vir0", &[("rkcif-mipi-lvds", COMPOSER, 0, ON)]),
        // Named like this receiver but not its subdevice: a sensor wired straight in.
        isp("rkisp-vir1", &[(MODEL, EntityFunction::CAM_SENSOR, 0, ON)]),
        // This receiver's subdevice on a pad other than the raw input.
        isp("rkisp-vir2", &[(MODEL, COMPOSER, 1, ON)]),
    ];
    assert!(route::feeds(MODEL, &isps).is_empty());
}

#[test]
fn feeds_with_no_isp_or_no_isp_entity_is_empty() {
    assert!(route::feeds(MODEL, &[]).is_empty());
    let mut graph = isp("rkisp-vir0", &[(MODEL, COMPOSER, 0, ON)]);
    graph.topology.entities[0].name = "something-else".into();
    assert!(route::feeds(MODEL, &[graph]).is_empty());
    assert_eq!(route::feeding(&[]), None);
}

#[test]
fn feeds_a_second_receiver_that_shares_an_isp_is_wired_but_off() {
    // Two receivers wired to one ISP; the ISP takes the first.
    let isps = [isp(
        "rkisp-vir0",
        &[
            ("rkcif-mipi-lvds1", COMPOSER, 0, ON),
            (MODEL, COMPOSER, 0, OFF),
        ],
    )];
    let links = route::feeds(MODEL, &isps);
    assert_eq!(
        links,
        vec![IspLink {
            isp: "rkisp-vir0".into(),
            enabled: false
        }]
    );
    assert_eq!(route::feeding(&links), None);
    assert_eq!(
        route::feeding(&route::feeds("rkcif-mipi-lvds1", &isps)),
        Some("rkisp-vir0")
    );
}

#[test]
fn the_driver_reads_feeds_from_its_own_model() {
    let (mut d, hw) = driver();
    hw.with(|m| {
        m.isps = vec![
            isp("rkisp-vir0", &[("rkcif-mipi-lvds", COMPOSER, 0, ON)]),
            isp("rkisp-vir1", &[(MODEL, COMPOSER, 0, ON)]),
        ];
    });
    let links = d.feeds().unwrap();
    assert_eq!(
        links,
        vec![IspLink {
            isp: "rkisp-vir1".into(),
            enabled: true
        }]
    );
}

// The contract.

#[test]
fn limits_are_the_receivers() {
    let (mut d, _hw) = driver();
    let limits = d.limits().unwrap();
    assert_eq!(limits.max_lanes, 4);
    assert_eq!(limits.max_link_rate_mbps, 2500);
    assert_eq!(limits.max_width, 8192);
    assert_eq!(limits.max_height, 8192);
}

#[test]
fn configure_works_out_the_compact_layout() {
    let (mut d, _hw) = driver();
    // 1280 * 10 / 8 = 1600, up to 256: 1792.
    assert_eq!(
        d.configure(raw10(), Packing::Packed).unwrap(),
        FrameLayout {
            width: 1280,
            height: 800,
            bit_depth: 10,
            packing: Packing::Packed,
            stride: 1792,
            size: 1_433_600,
        }
    );
    assert_eq!(d.link().unwrap(), Some(raw10()));
    // 1001 * 12 / 8 = 1501 (rounded down, as the vendor driver does), up to 256: 1536.
    let link = LinkConfig {
        width: 1001,
        height: 3,
        bit_depth: 12,
        data_type: 0x2c,
        ..raw10()
    };
    let got = d.configure(link, Packing::Packed).unwrap();
    assert_eq!((got.stride, got.size), (1536, 4608));
    // A line already aligned stays as it is: 2048 * 8 / 8.
    let link = LinkConfig {
        width: 2048,
        height: 2,
        bit_depth: 8,
        data_type: 0x2a,
        ..raw10()
    };
    let got = d.configure(link, Packing::Packed).unwrap();
    assert_eq!((got.stride, got.size), (2048, 4096));
}

#[test]
fn configure_refuses_what_the_receiver_cannot_take() {
    let (mut d, _hw) = driver();
    let mut bad = |f: &dyn Fn(&mut LinkConfig), packing: Packing| {
        let mut link = raw10();
        f(&mut link);
        assert_eq!(
            d.configure(link, packing).unwrap_err(),
            Status::INVALID_ARGUMENT
        );
    };
    bad(&|l| l.lanes = 0, Packing::Packed);
    bad(&|l| l.lanes = 5, Packing::Packed);
    bad(&|l| l.virtual_channel = 4, Packing::Packed);
    bad(&|l| l.data_type = 0, Packing::Packed);
    bad(&|l| l.data_type = 64, Packing::Packed);
    bad(&|l| l.link_rate_mbps = 79, Packing::Packed);
    bad(&|l| l.link_rate_mbps = 2501, Packing::Packed);
    bad(&|l| l.width = 0, Packing::Packed);
    bad(&|l| l.width = 8193, Packing::Packed);
    bad(&|l| l.height = 0, Packing::Packed);
    bad(&|l| l.height = 8193, Packing::Packed);
    bad(&|l| l.bit_depth = 14, Packing::Packed);
    bad(&|l| l.bit_depth = 9, Packing::Packed);
    // Unpacked needs the vendor driver's private call.
    bad(&|_| {}, Packing::Unpacked16);
    assert_eq!(d.link().unwrap(), None, "nothing refused is kept");
}

#[test]
fn start_requires_configure_and_routes_the_links_channel() {
    let (mut d, hw) = driver_on(receiver(OFF, ON, OFF));
    assert_eq!(d.start().unwrap_err(), Status::INVALID_ARGUMENT);
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    let a = |index| PadRef {
        entity: CSI_A,
        index,
    };
    let sink = PadRef {
        entity: STREAM0,
        index: 0,
    };
    assert_eq!(
        hw.with(|m| m.changes.clone()),
        vec![(a(2), sink, false), (a(1), sink, true)],
        "the other link off first, then the channel's on"
    );
    assert!(hw.is_on(A1_TO_0));
    assert!(!hw.is_on(A2_TO_0));
    assert!(hw.is_on(A2_TO_1), "other channels are left alone");
    assert_eq!(hw.with(|m| m.watched), Some(A1_TO_0));
}

#[test]
fn the_virtual_channel_picks_the_capture_node() {
    let (mut d, hw) = driver_on(receiver(ON, OFF, OFF));
    let link = LinkConfig {
        virtual_channel: 1,
        ..raw10()
    };
    d.configure(link, Packing::Packed).unwrap();
    d.start().unwrap();
    assert!(hw.with(|m| m.changes.is_empty()), "already routed");
    assert_eq!(hw.with(|m| m.watched), Some(A2_TO_1));
    // Channel 3 has no node in this graph: refused, and nothing is running.
    d.stop().unwrap();
    let link = LinkConfig {
        virtual_channel: 3,
        ..raw10()
    };
    d.configure(link, Packing::Packed).unwrap();
    assert_eq!(d.start().unwrap_err(), Status::NOT_FOUND);
    assert_eq!(d.tick(0), None);
}

#[test]
fn a_route_cannot_be_configured_or_started_twice() {
    let (mut d, _hw) = driver();
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    assert_eq!(
        d.configure(raw10(), Packing::Packed).unwrap_err(),
        Status::BUSY
    );
    assert_eq!(d.start().unwrap_err(), Status::BUSY);
    d.stop().unwrap();
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
}

#[test]
fn frame_stream_and_counters_are_not_served() {
    let (mut d, _hw) = driver();
    let mut out = [0u8; 32];
    assert_eq!(d.frame_stream(&mut out).unwrap_err(), Status::NOT_SUPPORTED);
    assert_eq!(d.counters().unwrap_err(), Status::NOT_SUPPORTED);
}

// Failures.

#[test]
fn a_route_turned_off_is_routed_again_with_capped_backoff() {
    let (mut d, hw) = driver();
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    assert_eq!(d.health(), Health::Ok);

    // A tick takes the clock, then the next checks the route.
    let mut now = 0u64;
    let at = d.tick(now).unwrap();
    assert_eq!(at, 1_000_000_000);
    now = at;
    let at = d.tick(now).unwrap();
    assert_eq!(at, now + 1_000_000_000, "still on");
    now = at;

    // Someone turns the channel's link off, and the graph can't be read for a while.
    hw.set(A1_TO_0, OFF);
    hw.with(|m| m.fail_topology = Some(Status::NO_DEVICE));
    let at = d.tick(now).unwrap();
    assert_eq!(at, now + 250_000_000);
    assert!(matches!(d.health(), Health::Degraded(_)));
    assert_eq!(d.start().unwrap_err(), Status::BUSY, "while recovering");

    let expected = [
        500_000_000u64,
        1_000_000_000,
        2_000_000_000,
        4_000_000_000,
        8_000_000_000,
        8_000_000_000,
        8_000_000_000,
    ];
    now = at;
    for delay in expected {
        let at = d.tick(now).unwrap();
        assert_eq!(at - now, delay, "backoff capped at 8 s");
        now = at;
    }
    // Forever: it never gives up and never turns itself off.
    for _ in 0..100 {
        let at = d.tick(now).unwrap();
        assert_eq!(at - now, 8_000_000_000);
        now = at;
        assert!(matches!(d.health(), Health::Degraded(_)));
    }
    assert!(hw.with(|m| m.closed) >= 100, "each attempt opens afresh");

    // And it clears when an attempt works: the link is on again and watched.
    hw.with(|m| m.fail_topology = None);
    let at = d.tick(now).unwrap();
    assert_eq!(at, now + 1_000_000_000);
    assert_eq!(d.health(), Health::Ok);
    assert!(hw.is_on(A1_TO_0));
    assert_eq!(hw.with(|m| m.watched), Some(A1_TO_0));
}

#[test]
fn stop_ends_recovery_and_leaves_the_route() {
    let (mut d, hw) = driver();
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    let t = d.tick(0).unwrap();
    hw.with(|m| m.fail_topology = Some(Status::IO));
    let _ = d.tick(t);
    assert!(matches!(d.health(), Health::Degraded(_)));
    d.stop().unwrap();
    assert_eq!(d.tick(u64::MAX), None, "stopped: nothing is due");
    assert_eq!(d.health(), Health::Ok);
    assert!(hw.is_on(A1_TO_0), "the ISP may be streaming through it");
}

#[test]
fn a_failed_start_is_reported_and_retried_by_its_caller() {
    let (mut d, hw) = driver();
    d.configure(raw10(), Packing::Packed).unwrap();
    hw.with(|m| m.fail_topology = Some(Status::NO_DEVICE));
    assert_eq!(d.start().unwrap_err(), Status::NO_DEVICE);
    assert_eq!(
        d.tick(0),
        None,
        "no background retries of a start not begun"
    );
    assert_eq!(d.health(), Health::Ok);
    hw.with(|m| m.fail_topology = None);
    d.start().unwrap();
}

#[test]
fn backoff_doubles_and_caps() {
    assert_eq!(backoff_ns(0), 250_000_000);
    assert_eq!(backoff_ns(1), 500_000_000);
    assert_eq!(backoff_ns(5), 8_000_000_000);
    assert_eq!(backoff_ns(100), 8_000_000_000);
}

#[test]
fn the_check_reads_the_watched_links_flag() {
    let mut links = [zeroed::<media_v2_link>(); 3];
    for (n, l) in (0u32..).zip(links.iter_mut()) {
        l.id = 7 + n;
    }
    links[1].flags = 1; // MEDIA_LNK_FL_ENABLED
    links[2].flags = 2; // MEDIA_LNK_FL_IMMUTABLE only
    assert!(link_on(&links, 8));
    assert!(!link_on(&links, 7));
    assert!(!link_on(&links, 9));
    assert!(!link_on(&links, 99), "a link gone from the graph");
}

#[test]
fn the_steady_state_allocates_nothing() {
    let (mut d, _hw) = driver();
    d.configure(raw10(), Packing::Packed).unwrap();
    d.start().unwrap();
    let mut now = d.tick(0).unwrap();
    let mut out = [0u8; 32];
    let ((), allocated) = allocations(|| {
        for _ in 0..1000 {
            now = d.tick(now).unwrap();
            assert_eq!(d.link().unwrap(), Some(raw10()));
            let _ = d.limits().unwrap();
            let _ = d.frame_stream(&mut out);
        }
    });
    assert_eq!(allocated, 0, "the route check");
    assert_eq!(d.health(), Health::Ok);
}
