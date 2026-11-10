//! The driver as the image contracts see it, and the jobs it runs.

use lemnos_contract_image_blend::{self as blend, BlendJob, BlendOp};
use lemnos_contract_image_convert::{self as convert, ConvertJob};
use lemnos_contract_image_scaler::{self as scaler, Filter, ScaleJob};
use lemnos_core::{ConfigValue, Driver};
use lemnos_measure::test_alloc::allocations;

use super::*;

fn start(entries: &[(&str, ConfigValue<'_>)]) -> Result<Rga> {
    Rga::start(RgaNeeds {}, &Config::new(entries))
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
fn defaults_answer_the_contracts() {
    let mut rga = start(&[]).unwrap();
    let info = ImageScaler::info(&mut rga).unwrap();
    assert_eq!((info.max_width, info.max_height), (8192, 8192));
    assert_eq!(ImageScaler::format_count(&mut rga), Ok(8));
    let formats: Vec<u32> = (0..8)
        .map(|i| ImageScaler::format(&mut rga, i).unwrap())
        .collect();
    assert_eq!(formats[0], fourcc(b"RGB3"));
    assert_eq!(formats[7], fourcc(b"YU12"));
    assert_eq!(ImageScaler::format(&mut rga, 8), Err(Status::NOT_FOUND));

    assert_eq!(rga.conversion_count(), Ok(64));
    let first = rga.conversion(0).unwrap();
    assert_eq!((first.from, first.to), (fourcc(b"RGB3"), fourcc(b"RGB3")));
    let last = rga.conversion(63).unwrap();
    assert_eq!((last.from, last.to), (fourcc(b"YU12"), fourcc(b"YU12")));
    let some = rga.conversion(8 * 2 + 5).unwrap();
    assert_eq!((some.from, some.to), (fourcc(b"AR24"), fourcc(b"NV12")));
    assert_eq!(rga.conversion(64), Err(Status::NOT_FOUND));

    assert_eq!(ImageBlend::format_count(&mut rga), Ok(1));
    assert_eq!(ImageBlend::format(&mut rga, 0), Ok(fourcc(b"AR24")));
    assert_eq!(ImageBlend::format(&mut rga, 1), Err(Status::NOT_FOUND));
}

#[test]
fn job_streams_are_not_offered() {
    let mut rga = start(&[]).unwrap();
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
fn config_sets_the_limits_and_the_matrix() {
    let mut rga = start(&[
        ("max_width", ConfigValue::Int(640)),
        ("max_height", ConfigValue::Int(480)),
        ("matrix", ConfigValue::Str("bt709-limited")),
        ("sampling", ConfigValue::Str("corners")),
    ])
    .unwrap();
    let info = ImageConvert::info(&mut rga).unwrap();
    assert_eq!((info.max_width, info.max_height), (640, 480));
    let s = rga.jobs().settings();
    assert_eq!(s.matrix, Matrix::Bt709Limited);
    assert_eq!(s.sampling, Sampling::Corners);
}

#[test]
fn bad_config_is_refused() {
    for entries in [
        [("max_width", ConfigValue::Int(0))],
        [("max_width", ConfigValue::Int(65536))],
        [("max_height", ConfigValue::Int(-1))],
        [("matrix", ConfigValue::Str("bt2020"))],
        [("sampling", ConfigValue::Str("middle"))],
    ] {
        assert_eq!(
            start(&entries).err(),
            Some(Status::INVALID_ARGUMENT),
            "{entries:?}"
        );
    }
}

#[test]
fn a_scale_job_runs_and_counts() {
    let mut rga = start(&[]).unwrap();
    let input: Vec<u8> = (0..8 * 4 * 4).map(|n| n as u8).collect();
    let mut output = vec![0u8; 4 * 2 * 4];
    let job = ScaleJob {
        identity: 1,
        input: scale_ref(0, 8, 4, b"AR24", 32),
        output: scale_ref(1, 4, 2, b"AR24", 16),
        filter: Filter::Area,
    };
    assert_eq!(rga.jobs().scale(&job, &input, &mut output), 32);
    // A 2x2 box over the first two pixels of rows 0 and 1: bytes 0, 4, 32, 36 average to 18.
    assert_eq!(output[0], 18);
    let c = ImageScaler::counters(&mut rga).unwrap();
    assert_eq!((c.jobs, c.errors), (1, 0));
}

#[test]
fn scale_jobs_that_overrun_their_buffers_are_refused() {
    let mut rga = start(&[("max_width", ConfigValue::Int(64))]).unwrap();
    let input = vec![0u8; 8 * 4 * 4];
    let mut output = vec![0u8; 4 * 2 * 4];
    let good = ScaleJob {
        identity: 1,
        input: scale_ref(0, 8, 4, b"AR24", 32),
        output: scale_ref(1, 4, 2, b"AR24", 16),
        filter: Filter::Bilinear,
    };
    let bad = [
        // The input claims one row more than its buffer holds.
        ScaleJob {
            input: scale_ref(0, 8, 5, b"AR24", 32),
            ..good
        },
        // A stride shorter than a row.
        ScaleJob {
            output: scale_ref(1, 4, 2, b"AR24", 15),
            ..good
        },
        // A stride so large its extent would wrap a 32-bit product.
        ScaleJob {
            input: scale_ref(0, 8, 4, b"AR24", u32::MAX),
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
    ];
    for job in &bad {
        assert_eq!(rga.jobs().scale(job, &input, &mut output), 0, "{job:?}");
    }
    assert_eq!(rga.jobs().scale(&good, &input, &mut output), 32);
    let c = ImageScaler::counters(&mut rga).unwrap();
    assert_eq!((c.jobs, c.errors), (1, bad.len() as u64));
}

#[test]
fn a_convert_job_runs_and_checks_both_planes_of_yuv() {
    let mut rga = start(&[]).unwrap();
    let input = vec![128u8; 4 * 4 * 3];
    // NV12 of 4x4 with stride 4: 16 bytes of luma and 8 of chroma.
    let mut output = vec![0u8; 24];
    let job = ConvertJob {
        identity: 2,
        input: convert_ref(0, 4, 4, b"RGB3", 12),
        output: convert_ref(1, 4, 4, b"NV12", 4),
    };
    assert_eq!(rga.jobs().convert(&job, &input, &mut output), 24);
    // One byte short of the chroma plane.
    let mut short = vec![0u8; 23];
    assert_eq!(rga.jobs().convert(&job, &input, &mut short), 0);
    let c = ImageConvert::counters(&mut rga).unwrap();
    assert_eq!((c.jobs, c.errors), (1, 1));
}

#[test]
fn a_blend_job_runs_and_needs_three_buffers() {
    let mut rga = start(&[]).unwrap();
    let fg = [0u8, 0, 128, 128].repeat(4);
    let bg = [200u8, 0, 0, 255].repeat(4);
    let mut out = vec![0u8; 16];
    let job = BlendJob {
        identity: 3,
        op: BlendOp::SrcOver,
        foreground: blend_ref(0, 2, 2),
        background: blend_ref(1, 2, 2),
        output: blend_ref(2, 2, 2),
    };
    assert_eq!(rga.jobs().blend(&job, &fg, &bg, &mut out), 16);
    // Blue: 0 + 200 * (255 - 128) / 255, rounded, is 100; alpha: 128 + 255 * 127 / 255.
    assert_eq!(&out[..4], &[100, 0, 128, 255]);
    let same = BlendJob {
        output: blend_ref(1, 2, 2),
        ..job
    };
    assert_eq!(rga.jobs().blend(&same, &fg, &bg, &mut out), 0);
    let c = ImageBlend::counters(&mut rga).unwrap();
    assert_eq!((c.jobs, c.errors), (1, 1));
}

#[test]
fn jobs_allocate_nothing_once_made() {
    let mut rga = start(&[("max_width", ConfigValue::Int(64))]).unwrap();
    let input = vec![7u8; 64 * 32 * 4];
    let mut output = vec![0u8; 48 * 24 * 3 / 2];
    let mut nv = vec![0u8; 64 * 32 * 3 / 2];
    let scale = ScaleJob {
        identity: 1,
        input: scale_ref(0, 64, 32, b"AR24", 256),
        output: scale_ref(1, 30, 20, b"AR24", 120),
        filter: Filter::Bilinear,
    };
    let area = ScaleJob {
        filter: Filter::Area,
        output: scale_ref(1, 32, 16, b"AR24", 128),
        ..scale
    };
    let conv = ConvertJob {
        identity: 2,
        input: convert_ref(0, 64, 32, b"AR24", 256),
        output: convert_ref(1, 64, 32, b"NV12", 64),
    };
    let mut big = vec![0u8; 30 * 20 * 4];
    let ((), count) = allocations(|| {
        for _ in 0..100 {
            assert_ne!(rga.jobs().scale(&scale, &input, &mut big), 0);
            assert_ne!(rga.jobs().scale(&area, &input, &mut big[..]), 0);
            assert_ne!(rga.jobs().convert(&conv, &input, &mut nv), 0);
            assert_eq!(rga.jobs().convert(&conv, &input, &mut output), 0);
        }
    });
    assert_eq!(count, 0, "allocations in 400 jobs");
}
