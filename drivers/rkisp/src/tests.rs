//! The generation table and the ISP path against graphs written out by hand, shaped as the
//! vendor driver builds them (see `graph`): every expected value here is written out by hand. The
//! library half: none of it needs the `device` feature.

use crate::{
    Generation, LAYOUT_ISP21_PARAMS, LAYOUT_ISP21_STATISTICS, LAYOUT_ISP30_PARAMS,
    LAYOUT_ISP35_PARAMS, LAYOUT_ISP35_STATISTICS,
};

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

// ---- The ISP path.

#[cfg(target_os = "linux")]
mod path {
    use lemnos_abi::Status;
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

#[test]
fn each_generation_names_its_module_bits() {
    use crate::uapi::{module, module35};
    // The ISP35 table is the ISP21/30 one plus its own modules: LSC is on both, the RGB-IR bit only
    // on ISP35, and the force bit is the top bit on both.
    assert_eq!(Generation::Isp21.module_bits(), module::ALL);
    assert_eq!(Generation::Isp30.module_bits(), module::ALL);
    assert_eq!(Generation::Isp35.module_bits(), module35::ALL);
    assert!(Generation::Isp35.accepts_modules(module35::RGBIR | module35::LSC));
    assert!(!Generation::Isp30.accepts_modules(module35::RGBIR));
    assert!(!Generation::Isp21.accepts_modules(module35::HSV));
    assert!(Generation::Isp21.accepts_modules(0));
    assert!(Generation::Isp30.accepts_modules(module::LSC | module::CGC));
    assert!(!Generation::Isp30.accepts_modules(module::FORCE | (1 << 62)));
    assert!(Generation::Isp35.accepts_modules(module35::FORCE));
}
