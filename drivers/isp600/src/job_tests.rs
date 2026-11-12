//! The job checks, and the contract's names for the layouts' regions: every expected value here
//! is written out by hand.

use lemnos_contract_camera_isp::{IspJob, NO_BUFFER};
use lemnos_contract_isp_isp600::{
    LAYOUT_LOAD, LAYOUT_STATISTICS, LOAD_BAYER_TABLE, LOAD_REGIONS, LOAD_REGISTERS, LOAD_YUV_TABLE,
    STATISTICS_AWB, STATISTICS_DENOISE_K, STATISTICS_HISTOGRAM_0, STATISTICS_REGIONS,
    STATISTICS_TONE_LUMINANCE, STATISTICS_TONE_PKX,
};
use lemnos_measure::test_alloc::allocations;

use crate::job::{self, Checked, Limits};
use crate::layout::{self, LOAD_BYTES, STATISTICS_BYTES};

#[test]
fn the_contract_names_the_regions_of_both_layouts() {
    assert_eq!(usize::from(LOAD_REGIONS), layout::LOAD_REGIONS.len());
    assert_eq!(
        layout::LOAD_REGIONS[usize::from(LOAD_REGISTERS)],
        layout::REGISTERS
    );
    assert_eq!(
        layout::LOAD_REGIONS[usize::from(LOAD_BAYER_TABLE)],
        layout::BAYER_TABLE
    );
    assert_eq!(
        layout::LOAD_REGIONS[usize::from(LOAD_YUV_TABLE)],
        layout::YUV_TABLE
    );
    assert_eq!(
        usize::from(STATISTICS_REGIONS),
        layout::STATISTICS_REGIONS.len()
    );
    let at = |n: u8| layout::STATISTICS_REGIONS[usize::from(n)];
    // The one offset the source states, and the two edges it fixes.
    assert_eq!(at(STATISTICS_HISTOGRAM_0).offset, 0x5a80);
    assert_eq!(at(STATISTICS_TONE_LUMINANCE).end(), layout::SAVE_BYTES);
    assert_eq!(at(STATISTICS_TONE_PKX).offset, layout::SAVE_BYTES);
    assert_eq!(at(STATISTICS_DENOISE_K).end(), STATISTICS_BYTES);
    assert_eq!(at(STATISTICS_AWB).bytes, 0x1800);
    assert_ne!(LAYOUT_LOAD, LAYOUT_STATISTICS);
}

fn limits() -> Limits {
    Limits {
        config_buffers: 4,
        output_buffers: 3,
    }
}

fn job(config_buffer: u32, output: u32) -> IspJob {
    IspJob {
        identity: 77,
        layout: LAYOUT_LOAD,
        config_buffer,
        input_buffer: NO_BUFFER,
        outputs: [output, NO_BUFFER, NO_BUFFER, NO_BUFFER],
    }
}

#[test]
fn a_job_is_checked_against_the_buffers_made() {
    assert_eq!(
        job::check(&job(3, 2), &limits()),
        Some(Checked {
            identity: 77,
            config_buffer: 3,
            output: Some(2),
        })
    );
    assert_eq!(
        job::check(&job(0, NO_BUFFER), &limits()).unwrap().output,
        None
    );
    for bad in [
        job(4, 0),
        job(u32::MAX - 1, 0),
        job(0, 3),
        job(0, u32::MAX - 1),
        IspJob {
            layout: LAYOUT_STATISTICS,
            ..job(0, 0)
        },
        IspJob {
            input_buffer: 0,
            ..job(0, 0)
        },
        IspJob {
            outputs: [0, 0, NO_BUFFER, NO_BUFFER],
            ..job(0, 0)
        },
        IspJob {
            outputs: [NO_BUFFER, NO_BUFFER, NO_BUFFER, 1],
            ..job(0, 0)
        },
    ] {
        assert_eq!(job::check(&bad, &limits()), None, "{bad:?}");
    }
}

#[test]
fn filled_is_bounded_by_the_mapping_and_the_stream_buffer() {
    let mapped = [9u8; 16];
    assert_eq!(job::filled(&mapped, 8, 8).unwrap().len(), 8);
    assert_eq!(job::filled(&mapped, 9, 8), None, "past the stream buffer");
    assert_eq!(job::filled(&mapped, 17, 64), None, "past the mapping");
    assert_eq!(job::filled(&mapped, usize::MAX, usize::MAX), None);
}

#[test]
fn the_job_checks_allocate_nothing() {
    let image = vec![0u8; LOAD_BYTES];
    let j = job(1, 1);
    let ((), allocated) = allocations(|| {
        for _ in 0..1000 {
            assert!(layout::check_load(&image).is_ok());
            assert!(job::check(&j, &limits()).is_some());
        }
    });
    assert_eq!(allocated, 0);
}
