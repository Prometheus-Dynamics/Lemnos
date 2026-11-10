//! The generation table, the job checks and the ISP path against graphs written out by hand,
//! shaped as the vendor driver builds them (see `graph`): every expected value here is written
//! out by hand.

use lemnos_contract_camera_isp::{IspJob, NO_BUFFER};
use lemnos_core::Status;
use lemnos_measure::test_alloc::allocations;

use crate::job::{self, Checked, Limits};
use crate::{
    Generation, LAYOUT_ISP21_PARAMS, LAYOUT_ISP21_STATISTICS, LAYOUT_ISP30_PARAMS,
    LAYOUT_ISP35_PARAMS, LAYOUT_ISP35_STATISTICS,
};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

#[test]
fn the_soc_names_its_isp() {
    let root = |s: &[&'static str]| Generation::from_compatibles(s.iter().copied());
    assert_eq!(
        root(&["radxa,zero3", "rockchip,rk3566"]),
        Some(Generation::Isp21)
    );
    assert_eq!(
        root(&["rockchip,rk3568-evb", "rockchip,rk3568"]),
        Some(Generation::Isp21)
    );
    assert_eq!(
        root(&["radxa,rock-5a", "rockchip,rk3588s"]),
        Some(Generation::Isp30)
    );
    assert_eq!(
        root(&["rockchip,rk3588-evb", "rockchip,rk3588"]),
        Some(Generation::Isp30)
    );
    assert_eq!(
        root(&["luckfox,aura", "rockchip,rv1126b"]),
        Some(Generation::Isp35)
    );
    // Other generations, and chips this crate doesn't know.
    assert_eq!(root(&["luckfox,pico", "rockchip,rv1106"]), None);
    assert_eq!(root(&["rockchip,rk3562"]), None);
    assert_eq!(root(&["rockchip,rv1103b"]), None);
    assert_eq!(root(&["rockchip,rk3576"]), None);
    assert_eq!(
        root(&["raspberrypi,5-compute-module", "brcm,bcm2712"]),
        None
    );
}

#[test]
fn a_board_names_its_generation() {
    assert_eq!(Generation::from_name("isp21"), Some(Generation::Isp21));
    assert_eq!(Generation::from_name("isp30"), Some(Generation::Isp30));
    assert_eq!(Generation::from_name("isp35"), Some(Generation::Isp35));
    assert_eq!(Generation::from_name("isp32"), None);
    assert_eq!(Generation::from_name(""), None);
}

#[test]
fn each_generation_names_its_layouts_and_their_sizes() {
    let c = Generation::Isp21.config_layout();
    assert_eq!((c.layout, c.bytes), (LAYOUT_ISP21_PARAMS, 10475));
    let s = Generation::Isp21.statistics_layout();
    assert_eq!((s.layout, s.bytes), (LAYOUT_ISP21_STATISTICS, 13700));
    let c = Generation::Isp30.config_layout();
    assert_eq!((c.layout, c.bytes), (LAYOUT_ISP30_PARAMS, 10921));
    assert_eq!(Generation::Isp30.statistics_layout().bytes, 16172);
    let c = Generation::Isp35.config_layout();
    assert_eq!((c.layout, c.bytes), (LAYOUT_ISP35_PARAMS, 13065));
    let s = Generation::Isp35.statistics_layout();
    assert_eq!((s.layout, s.bytes), (LAYOUT_ISP35_STATISTICS, 14542));
}

// ---- Jobs.

/// An ISP30 with 4 config buffers of 16 KiB, 3 main path buffers and the self path off.
fn limits() -> Limits {
    Limits::new(Generation::Isp30, 4, 16384, [Some(3), None]).expect("limits")
}

fn job(config_buffer: u32, outputs: [u32; 4]) -> IspJob {
    IspJob {
        identity: 77,
        layout: LAYOUT_ISP30_PARAMS,
        config_buffer,
        input_buffer: NO_BUFFER,
        outputs,
    }
}

const NONE4: [u32; 4] = [NO_BUFFER; 4];

#[test]
fn a_job_inside_what_the_driver_holds_passes() {
    assert_eq!(
        job::check(&job(3, [2, NO_BUFFER, NO_BUFFER, NO_BUFFER]), &limits()),
        Some(Checked {
            identity: 77,
            config_buffer: 3,
            outputs: [Some(2), None],
        })
    );
    // Statistics only: no image this frame.
    assert!(job::check(&job(0, NONE4), &limits()).is_some());
}

#[test]
fn a_config_buffer_past_the_config_stream_is_refused() {
    assert_eq!(job::check(&job(4, NONE4), &limits()), None);
    assert_eq!(job::check(&job(u32::MAX - 1, NONE4), &limits()), None);
}

#[test]
fn an_output_buffer_past_its_stream_is_refused() {
    assert_eq!(
        job::check(&job(0, [3, NO_BUFFER, NO_BUFFER, NO_BUFFER]), &limits()),
        None
    );
    assert_eq!(
        job::check(
            &job(0, [u32::MAX - 1, NO_BUFFER, NO_BUFFER, NO_BUFFER]),
            &limits()
        ),
        None
    );
}

#[test]
fn a_buffer_for_an_output_that_is_off_or_missing_is_refused() {
    // The self path is off.
    assert_eq!(
        job::check(&job(0, [NO_BUFFER, 0, NO_BUFFER, NO_BUFFER]), &limits()),
        None
    );
    // The ISP has two outputs.
    assert_eq!(
        job::check(&job(0, [NO_BUFFER, NO_BUFFER, 0, NO_BUFFER]), &limits()),
        None
    );
    assert_eq!(
        job::check(&job(0, [NO_BUFFER, NO_BUFFER, NO_BUFFER, 0]), &limits()),
        None
    );
}

#[test]
fn another_layout_or_an_input_buffer_is_refused() {
    let mut j = job(0, NONE4);
    j.layout = LAYOUT_ISP21_PARAMS;
    assert_eq!(job::check(&j, &limits()), None);
    let mut j = job(0, NONE4);
    j.input_buffer = 0;
    assert_eq!(job::check(&j, &limits()), None);
}

#[test]
fn a_config_stream_too_small_for_the_parameters_is_refused() {
    assert_eq!(
        Limits::new(Generation::Isp35, 4, 13064, [Some(2), None]).unwrap_err(),
        Status::INVALID_ARGUMENT
    );
    assert!(Limits::new(Generation::Isp35, 4, 13065, [Some(2), None]).is_ok());
}

#[test]
fn parameters_are_copied_whole_or_not_at_all() {
    let n = Generation::Isp21.config_layout().bytes as usize;
    let from: Vec<u8> = (0..n + 10).map(|i| i as u8).collect();
    let mut into = vec![0xee; n + 4];
    assert_eq!(job::copy_params(Generation::Isp21, &from, &mut into), Ok(n));
    assert_eq!(&into[..n], &from[..n]);
    assert_eq!(&into[n..], &[0xee; 4]);

    // The caller's buffer is shorter than the parameters.
    let mut into = vec![0; n];
    assert_eq!(
        job::copy_params(Generation::Isp21, &from[..n - 1], &mut into),
        Err(Status::INVALID_ARGUMENT)
    );
    assert!(into.iter().all(|&b| b == 0));
    // The kernel's buffer is.
    let mut into = vec![0; n - 1];
    assert_eq!(
        job::copy_params(Generation::Isp21, &from, &mut into),
        Err(Status::BUFFER_TOO_SMALL)
    );
}

#[test]
fn a_dequeued_buffer_is_read_only_as_far_as_it_was_mapped_and_fits() {
    let mapped = [7u8; 64];
    assert_eq!(job::filled(&mapped, 40, 48).map(<[u8]>::len), Some(40));
    assert_eq!(job::filled(&mapped, 64, 64).map(<[u8]>::len), Some(64));
    // The kernel reports more than it mapped.
    assert_eq!(job::filled(&mapped, 65, 1 << 20), None);
    assert_eq!(job::filled(&mapped, usize::MAX, u32::MAX), None);
    // The stream buffer is smaller.
    assert_eq!(job::filled(&mapped, 49, 48), None);
}

#[test]
fn the_per_frame_checks_allocate_nothing() {
    let limits = limits();
    let j = job(1, [0, NO_BUFFER, NO_BUFFER, NO_BUFFER]);
    let n = Generation::Isp30.config_layout().bytes as usize;
    let from = vec![1u8; n];
    let mut into = vec![0u8; n];
    let mapped = vec![2u8; 128];
    let (ok, allocated) = allocations(|| {
        let checked = job::check(&j, &limits).is_some();
        let copied = job::copy_params(Generation::Isp30, &from, &mut into).is_ok();
        let out = job::filled(&mapped, 100, 128).is_some();
        checked && copied && out
    });
    assert!(ok);
    assert_eq!(allocated, 0);
}

// ---- The ISP path.

#[cfg(target_os = "linux")]
mod path {
    use lemnos_core::Status;
    use lemnos_v4l2::media::{
        Entity, EntityFlags, EntityFunction, Link, LinkFlags, Pad, PadFlags, PadRef, Topology,
    };

    use crate::graph::{
        ISP_ENTITY, MAINPATH_ENTITY, PARAMS_ENTITY, SELFPATH_ENTITY, STATISTICS_ENTITY, plan,
    };

    const ON: LinkFlags = LinkFlags::ENABLED;
    const OFF: LinkFlags = LinkFlags::empty();

    // Entities.
    const ISP: u32 = 1;
    const PARAMS: u32 = 2;
    const STATS: u32 = 3;
    const MAIN: u32 = 4;
    const SELF: u32 = 5;
    const CIF_A: u32 = 6;
    const CIF_B: u32 = 7;
    // Pads: the ISP's 0 to 3, then one per node.
    const P_SINK: u32 = 10;
    const P_PARAMS: u32 = 11;
    const P_SOURCE: u32 = 12;
    const P_STATS: u32 = 13;
    const N_PARAMS: u32 = 20;
    const N_STATS: u32 = 21;
    const N_MAIN: u32 = 22;
    const N_SELF: u32 = 23;
    const N_CIF_A: u32 = 24;
    const N_CIF_B: u32 = 25;

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

    /// The ISP's graph with two receivers wired to its input, as the vendor driver links it.
    /// `a` and `b` are the receivers' links, `nodes` the four node links, `self_path` the self
    /// path's.
    fn graph(a: LinkFlags, b: LinkFlags, nodes: LinkFlags, self_path: LinkFlags) -> Topology {
        Topology {
            version: 1,
            entities: vec![
                entity(ISP, ISP_ENTITY, EntityFunction::PROC_VIDEO_ISP),
                entity(PARAMS, PARAMS_ENTITY, EntityFunction::IO_V4L),
                entity(STATS, STATISTICS_ENTITY, EntityFunction::IO_V4L),
                entity(MAIN, MAINPATH_ENTITY, EntityFunction::IO_V4L),
                entity(SELF, SELFPATH_ENTITY, EntityFunction::IO_V4L),
                entity(
                    CIF_A,
                    "rkcif-mipi-lvds",
                    EntityFunction::PROC_VIDEO_COMPOSER,
                ),
                entity(
                    CIF_B,
                    "rkcif-mipi-lvds1",
                    EntityFunction::PROC_VIDEO_COMPOSER,
                ),
            ],
            interfaces: vec![],
            pads: vec![
                pad(P_SINK, ISP, 0),
                pad(P_PARAMS, ISP, 1),
                pad(P_SOURCE, ISP, 2),
                pad(P_STATS, ISP, 3),
                pad(N_PARAMS, PARAMS, 0),
                pad(N_STATS, STATS, 0),
                pad(N_MAIN, MAIN, 0),
                pad(N_SELF, SELF, 0),
                pad(N_CIF_A, CIF_A, 0),
                pad(N_CIF_B, CIF_B, 0),
            ],
            links: vec![
                link(1, N_CIF_A, P_SINK, a),
                link(2, N_CIF_B, P_SINK, b),
                link(3, N_PARAMS, P_PARAMS, nodes),
                link(4, P_SOURCE, N_MAIN, nodes),
                link(5, P_STATS, N_STATS, nodes),
                link(6, P_SOURCE, N_SELF, self_path),
            ],
        }
    }

    fn at(entity: u32, index: u32) -> PadRef {
        PadRef { entity, index }
    }

    #[test]
    fn a_path_already_on_changes_nothing() {
        let p = plan(&graph(ON, OFF, ON, OFF), None, false).expect("plan");
        assert_eq!(p.source, at(CIF_A, 0));
        assert!(p.enable.is_empty());
        assert!(p.disable.is_empty());
        assert_eq!(
            (
                p.nodes.isp,
                p.nodes.params,
                p.nodes.statistics,
                p.nodes.main
            ),
            (ISP, PARAMS, STATS, MAIN)
        );
        assert_eq!(p.nodes.self_path, None);
    }

    #[test]
    fn the_links_the_path_needs_are_turned_on() {
        let p = plan(&graph(OFF, OFF, OFF, OFF), None, true).expect("plan");
        assert_eq!(p.source, at(CIF_A, 0));
        assert_eq!(
            p.enable,
            vec![
                (at(CIF_A, 0), at(ISP, 0)),
                (at(PARAMS, 0), at(ISP, 1)),
                (at(ISP, 2), at(MAIN, 0)),
                (at(ISP, 3), at(STATS, 0)),
                (at(ISP, 2), at(SELF, 0)),
            ]
        );
        assert_eq!(p.nodes.self_path, Some(SELF));
    }

    #[test]
    fn the_source_in_use_is_kept() {
        let p = plan(&graph(OFF, ON, ON, OFF), None, false).expect("plan");
        assert_eq!(p.source, at(CIF_B, 0));
        assert!(p.enable.is_empty());
    }

    #[test]
    fn a_named_source_takes_the_input_from_the_other() {
        let p = plan(&graph(ON, OFF, ON, OFF), Some("rkcif-mipi-lvds1"), false).expect("plan");
        assert_eq!(p.source, at(CIF_B, 0));
        assert_eq!(p.disable, vec![(at(CIF_A, 0), at(ISP, 0))]);
        assert_eq!(p.enable, vec![(at(CIF_B, 0), at(ISP, 0))]);
    }

    #[test]
    fn an_immutable_link_from_another_source_is_busy() {
        let held = ON | LinkFlags::IMMUTABLE;
        assert_eq!(
            plan(&graph(held, OFF, ON, OFF), Some("rkcif-mipi-lvds1"), false).unwrap_err(),
            Status::BUSY
        );
    }

    #[test]
    fn a_missing_entity_source_or_link_is_not_found() {
        assert_eq!(
            plan(&graph(ON, OFF, ON, OFF), Some("nobody"), false).unwrap_err(),
            Status::NOT_FOUND
        );
        // Asked for by name, but not wired to the ISP.
        let mut t = graph(ON, OFF, ON, OFF);
        t.links.retain(|l| l.id != 2);
        assert_eq!(
            plan(&t, Some("rkcif-mipi-lvds1"), false).unwrap_err(),
            Status::NOT_FOUND
        );
        // Nothing feeds the ISP.
        let mut t = graph(ON, OFF, ON, OFF);
        t.links.retain(|l| l.id > 2);
        assert_eq!(plan(&t, None, false).unwrap_err(), Status::NOT_FOUND);
        // The statistics node's link is gone.
        let mut t = graph(ON, OFF, ON, OFF);
        t.links.retain(|l| l.id != 5);
        assert_eq!(plan(&t, None, false).unwrap_err(), Status::NOT_FOUND);
        // The self path's node is gone, and it was asked for.
        let mut t = graph(ON, OFF, ON, OFF);
        t.entities.retain(|e| e.id != SELF);
        assert_eq!(plan(&t, None, true).unwrap_err(), Status::NOT_FOUND);
        assert!(plan(&t, None, false).is_ok());
    }
}
