//! The driver as the image contracts see it, and the jobs it runs.

use lemnos_contract_image_blend::{self as blend, BlendJob, BlendOp};
use lemnos_contract_image_convert::{self as convert, ConvertJob};
use lemnos_contract_image_scaler::{self as scaler, Filter, ScaleJob};
use lemnos_core::{ConfigValue, Driver};

use super::*;

fn start(entries: &[(&str, ConfigValue<'_>)]) -> Result<Rga> {
    let needs = RgaNeeds {
        scale: None,
        convert: None,
        blend: None,
    };
    Rga::start(needs, &Config::new(entries))
}

fn on(soc: &str) -> Rga {
    start(&[("soc", ConfigValue::Str(soc))]).unwrap()
}

fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

fn scale_ref(
    buffer: u32,
    width: u32,
    height: u32,
    code: &[u8; 4],
    stride: u32,
) -> scaler::ImageRef {
    scaler::ImageRef {
        buffer,
        width,
        height,
        fourcc: fourcc(code),
        stride,
    }
}

fn convert_ref(
    buffer: u32,
    width: u32,
    height: u32,
    code: &[u8; 4],
    stride: u32,
) -> convert::ImageRef {
    convert::ImageRef {
        buffer,
        width,
        height,
        fourcc: fourcc(code),
        stride,
    }
}

fn blend_ref(buffer: u32, width: u32, height: u32) -> blend::ImageRef {
    blend::ImageRef {
        buffer,
        width,
        height,
        fourcc: fourcc(b"AR24"),
        stride: width * 4,
    }
}

#[test]
fn the_manifest_names_the_three_contracts() {
    let manifest = <Rga as Driver>::MANIFEST;
    assert_eq!(manifest.name, "rga");
    let tags: Vec<&str> = manifest.provides.iter().map(|c| c.tag.as_str()).collect();
    assert_eq!(tags, ["image.scaler/1", "image.convert/1", "image.blend/1"]);
}

#[test]
fn the_contracts_list_what_the_chips_hardware_does() {
    // RK3588: the mainline driver scales and converts every format but gray; `/dev/rga` adds
    // gray to AR24 and XR24, and blends.
    let mut rga = on("rk3588");
    let info = ImageScaler::info(&mut rga).unwrap();
    assert_eq!((info.max_width, info.max_height), (8192, 8192));
    assert_eq!(ImageScaler::format_count(&mut rga), Ok(7));
    let formats: Vec<u32> = (0..7)
        .map(|i| ImageScaler::format(&mut rga, i).unwrap())
        .collect();
    assert_eq!(formats[0], fourcc(b"RGB3"));
    assert!(!formats.contains(&fourcc(b"GREY")));
    assert_eq!(formats[6], fourcc(b"YU12"));
    assert_eq!(ImageScaler::format(&mut rga, 7), Err(Status::NOT_FOUND));

    assert_eq!(rga.conversion_count(), Ok(7 * 7 + 2));
    let pairs: Vec<(u32, u32)> = (0..51)
        .map(|i| rga.conversion(i).map(|p| (p.from, p.to)).unwrap())
        .collect();
    assert_eq!(pairs[0], (fourcc(b"RGB3"), fourcc(b"RGB3")));
    assert!(pairs.contains(&(fourcc(b"GREY"), fourcc(b"AR24"))));
    assert!(pairs.contains(&(fourcc(b"GREY"), fourcc(b"XR24"))));
    assert!(!pairs.contains(&(fourcc(b"GREY"), fourcc(b"NV12"))));
    assert!(!pairs.contains(&(fourcc(b"AR24"), fourcc(b"GREY"))));
    assert_eq!(rga.conversion(51), Err(Status::NOT_FOUND));

    assert_eq!(ImageBlend::format_count(&mut rga), Ok(1));
    assert_eq!(ImageBlend::format(&mut rga, 0), Ok(fourcc(b"AR24")));
    assert_eq!(ImageBlend::format(&mut rga, 1), Err(Status::NOT_FOUND));

    // RK3576: `/dev/rga` only, which scales nothing here and converts only from gray.
    let mut rga = on("rk3576");
    assert_eq!(ImageScaler::format_count(&mut rga), Ok(0));
    assert_eq!(ImageScaler::format(&mut rga, 0), Err(Status::NOT_FOUND));
    assert_eq!(rga.conversion_count(), Ok(2));
    let first = rga.conversion(0).unwrap();
    assert_eq!((first.from, first.to), (fourcc(b"GREY"), fourcc(b"AR24")));
    assert_eq!(ImageBlend::format_count(&mut rga), Ok(1));
}

#[test]
fn job_streams_the_board_gives_none_of_are_not_offered() {
    let mut rga = on("rk3588");
    let mut name = [0u8; 64];
    assert_eq!(
        ImageScaler::job_stream(&mut rga, &mut name),
        Err(Status::NOT_SUPPORTED)
    );
    assert_eq!(
        ImageConvert::job_stream(&mut rga, &mut name),
        Err(Status::NOT_SUPPORTED)
    );
    assert_eq!(
        ImageBlend::job_stream(&mut rga, &mut name),
        Err(Status::NOT_SUPPORTED)
    );
}

#[test]
fn config_sets_the_limits() {
    let mut rga = start(&[
        ("soc", ConfigValue::Str("rk3568")),
        ("max_width", ConfigValue::Int(640)),
        ("max_height", ConfigValue::Int(480)),
    ])
    .unwrap();
    let info = ImageConvert::info(&mut rga).unwrap();
    assert_eq!((info.max_width, info.max_height), (640, 480));
    assert_eq!(Lifecycle::health(&rga), Health::Ok);
}

#[test]
fn without_a_soc_or_with_a_chip_that_has_no_route_the_driver_does_not_bind() {
    // No `soc`: no route for anything, so no driver that would fail every job.
    assert_eq!(start(&[]).err(), Some(Status::INVALID_ARGUMENT));
    assert_eq!(
        start(&[("max_width", ConfigValue::Int(64))]).err(),
        Some(Status::INVALID_ARGUMENT)
    );
    // A chip this crate knows whose units have no route here yet.
    assert_eq!(
        start(&[("soc", ConfigValue::Str("k230d"))]).err(),
        Some(Status::NOT_SUPPORTED)
    );
    for soc in ["rk3566", "rk3568", "rk3576", "rk3588", "rk3562", "rv1126b"] {
        assert!(start(&[("soc", ConfigValue::Str(soc))]).is_ok(), "{soc}");
    }
}

#[test]
fn bad_config_is_refused() {
    let soc = ("soc", ConfigValue::Str("rk3588"));
    for entry in [
        ("max_width", ConfigValue::Int(0)),
        ("max_width", ConfigValue::Int(65536)),
        ("max_height", ConfigValue::Int(-1)),
        ("soc", ConfigValue::Str("rk9999")),
        // The CPU route's settings: the hardware has its own matrix and phase.
        ("matrix", ConfigValue::Str("bt601-limited")),
        ("sampling", ConfigValue::Str("centres")),
    ] {
        let entries = if entry.0 == "soc" {
            vec![entry]
        } else {
            vec![soc, entry]
        };
        assert_eq!(
            start(&entries).err(),
            Some(Status::INVALID_ARGUMENT),
            "{entries:?}"
        );
    }
}

#[test]
fn scale_jobs_that_overrun_their_buffers_are_refused() {
    let mut rga = start(&[
        ("soc", ConfigValue::Str("rk3588")),
        ("max_width", ConfigValue::Int(64)),
    ])
    .unwrap();
    let jobs = rga.jobs();
    // The buffers the jobs name, by length.
    let (input, output) = (8 * 4 * 4, 4 * 2 * 4);
    let good = ScaleJob {
        identity: 1,
        input: scale_ref(0, 8, 4, b"AR24", 32),
        output: scale_ref(1, 4, 2, b"AR24", 16),
        filter: Filter::Bilinear,
    };
    let fits = |plan: &jobs::Plan| {
        Jobs::fits(plan.input.layout, input) && Jobs::fits(plan.output.layout, output)
    };
    assert!(jobs.plan_scale(&good).is_some_and(|p| fits(&p)));
    // Planned, but past their buffers.
    for job in [
        // The input claims one row more than its buffer holds.
        ScaleJob {
            input: scale_ref(0, 8, 5, b"AR24", 32),
            ..good
        },
        // A stride so large its extent would wrap a 32-bit product.
        ScaleJob {
            input: scale_ref(0, 8, 4, b"AR24", u32::MAX),
            ..good
        },
    ] {
        let plan = jobs.plan_scale(&job).unwrap();
        assert!(!fits(&plan), "{job:?}");
    }
    // Refused by the job alone.
    for job in [
        // A stride shorter than a row.
        ScaleJob {
            output: scale_ref(1, 4, 2, b"AR24", 15),
            ..good
        },
        // Wider than configured.
        ScaleJob {
            input: scale_ref(0, 65, 1, b"AR24", 260),
            ..good
        },
        // Empty.
        ScaleJob {
            output: scale_ref(1, 0, 2, b"AR24", 16),
            ..good
        },
        // The same buffer on both sides.
        ScaleJob {
            output: scale_ref(0, 4, 2, b"AR24", 16),
            ..good
        },
        // A format change is a conversion, not a scale.
        ScaleJob {
            output: scale_ref(1, 4, 2, b"XR24", 16),
            ..good
        },
        // An unknown fourcc.
        ScaleJob {
            input: scale_ref(0, 8, 4, b"ZZZZ", 32),
            output: scale_ref(1, 4, 2, b"ZZZZ", 16),
            ..good
        },
    ] {
        assert_eq!(jobs.plan_scale(&job), None, "{job:?}");
    }
}

#[test]
fn a_convert_job_is_checked_on_both_planes_of_yuv() {
    let mut rga = on("rk3588");
    let jobs = rga.jobs();
    // NV12 of 4x4 with stride 4: 16 bytes of luma and 8 of chroma.
    let job = ConvertJob {
        identity: 2,
        input: convert_ref(0, 4, 4, b"RGB3", 12),
        output: convert_ref(1, 4, 4, b"NV12", 4),
    };
    let plan = jobs.plan_convert(&job).unwrap();
    assert!(Jobs::fits(plan.output.layout, 24));
    // One byte short of the chroma plane.
    assert!(!Jobs::fits(plan.output.layout, 23));
    // A conversion keeps the size.
    let resized = ConvertJob {
        output: convert_ref(1, 4, 2, b"NV12", 4),
        ..job
    };
    assert_eq!(jobs.plan_convert(&resized), None);
}

#[test]
fn a_blend_job_needs_three_buffers_of_one_shape_in_its_format() {
    let mut rga = on("rk3588");
    let jobs = rga.jobs();
    let job = BlendJob {
        identity: 3,
        op: BlendOp::SrcOver,
        foreground: blend_ref(0, 2, 2),
        background: blend_ref(1, 2, 2),
        output: blend_ref(2, 2, 2),
    };
    assert!(jobs.plan_blend(&job).is_some());
    let same = BlendJob {
        output: blend_ref(1, 2, 2),
        ..job
    };
    assert_eq!(jobs.plan_blend(&same), None);
    let smaller = BlendJob {
        output: blend_ref(2, 2, 1),
        ..job
    };
    assert_eq!(jobs.plan_blend(&smaller), None);
    let rgb = |buffer| blend::ImageRef {
        fourcc: fourcc(b"RGB3"),
        stride: 6,
        ..blend_ref(buffer, 2, 2)
    };
    let not_rgba = BlendJob {
        foreground: rgb(0),
        background: rgb(1),
        output: rgb(2),
        ..job
    };
    assert_eq!(jobs.plan_blend(&not_rgba), None);
}
