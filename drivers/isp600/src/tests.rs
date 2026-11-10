//! The layouts and the job checks: every expected value here is written out by hand.

use lemnos_contract_camera_isp::{IspJob, NO_BUFFER};
use lemnos_contract_isp_isp600::{
    LAYOUT_LOAD, LAYOUT_STATISTICS, LOAD_BAYER_TABLE, LOAD_REGIONS, LOAD_REGISTERS, LOAD_YUV_TABLE,
    STATISTICS_AWB, STATISTICS_DENOISE_K, STATISTICS_HISTOGRAM_0, STATISTICS_REGIONS,
    STATISTICS_TONE_LUMINANCE, STATISTICS_TONE_PKX,
};
use lemnos_measure::test_alloc::allocations;

use crate::job::{self, Checked, Limits};
use crate::layout::{self, ADDRESS_WORDS, LOAD_BYTES, Refusal, Region, STATISTICS_BYTES};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

const fn r(offset: usize, bytes: usize) -> Region {
    Region { offset, bytes }
}

#[test]
fn the_load_image_is_the_kernels() {
    assert_eq!(LOAD_BYTES, 0x7700);
    assert_eq!(
        layout::LOAD_REGIONS,
        [
            r(0x0000, 0x1000),
            r(0x1000, 0x1800),
            r(0x2800, 0x2600),
            r(0x4e00, 0x1200),
            r(0x6000, 0x1700),
        ]
    );
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
    // Where the kernel finds the shading tables in the Bayer table.
    assert_eq!(layout::LENS_SHADING_TABLE, r(0x2800, 0x800));
    assert_eq!(layout::MSC_TABLE, r(0x3000, 0x800));
    assert_eq!(layout::NR_MSC_TABLE, r(0x3800, 0x3c8));
    // The statistics windows in the register image.
    assert_eq!(layout::AE_WINDOW, r(0x650, 8));
    assert_eq!(layout::AF_WINDOW, r(0x670, 12));
    assert_eq!(layout::AWB_WINDOW, r(0x6e0, 8));
    assert_eq!(layout::HISTOGRAM_WINDOW, r(0x710, 32));
}

#[test]
fn the_statistics_buffer_is_the_kernels() {
    assert_eq!(STATISTICS_BYTES, 0x9d00);
    assert_eq!(
        layout::STATISTICS_REGIONS,
        [
            r(0x0000, 0x100),
            r(0x0100, 0x360),
            r(0x0460, 0x20),
            r(0x0480, 0xc00),
            r(0x1080, 0xc00),
            r(0x1c80, 0xc00),
            r(0x2880, 0xc00),
            r(0x3480, 0xc00),
            r(0x4080, 0x1800),
            r(0x5880, 0x200),
            r(0x5a80, 0x400),
            r(0x5e80, 0x400),
            r(0x6280, 0x780),
            r(0x6a00, 0x3000),
            r(0x9a00, 0x300),
        ]
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

#[test]
fn which_parts_lie_before_the_first_histogram_is_fixed_by_the_sizes() {
    // The sizes before `ISP_STAT_HIST0_MEM_OFS` must add up to it; the only parts that can fill
    // the rest of the saved buffer after the first histogram are the second and the luminance.
    let sizes = [
        0x360, 0x20, 0xc00, 0xc00, 0xc00, 0xc00, 0xc00, 0x1800, 0x200, 0x400, 0x780,
    ];
    let after = 0x6a00 - 0x5a80 - 0x400;
    let mut fits = Vec::new();
    for mask in 0u32..(1 << sizes.len()) {
        let sum: usize = (0..sizes.len())
            .filter(|i| mask & (1 << i) != 0)
            .map(|i| sizes[i])
            .sum();
        if sum == after {
            fits.push(mask);
        }
    }
    // Index 9 (the second histogram, 0x400) and 10 (the luminance, 0x780); nothing else adds to
    // 0xb80.
    assert_eq!(fits, [(1 << 9) | (1 << 10)]);
}

#[test]
fn the_address_words_are_each_control_blocks_and_temporal_denoises() {
    assert_eq!(
        ADDRESS_WORDS,
        [
            0x24, 0x3c, 0x40, // ISP 0: load, save, save-load
            0x54, 0x6c, 0x70, // ISP 1
            0x84, 0x9c, 0xa0, // ISP 2
            0xb4, 0xcc, 0xd0, // ISP 3
            0x140, 0x144, 0x148, 0x14c, // temporal denoise
        ]
    );
}

#[test]
fn a_load_image_is_refused_for_its_length_or_any_address() {
    let image = vec![0x5au8; LOAD_BYTES];
    // Anything else may be anything; the address words must be zero.
    let mut clean = image.clone();
    for &w in &ADDRESS_WORDS {
        clean[w..w + 4].fill(0);
    }
    let checked = layout::check_load(&clean).unwrap();
    assert_eq!(checked.bytes().as_slice(), clean.as_slice());

    assert_eq!(
        layout::check_load(&clean[..LOAD_BYTES - 1]).unwrap_err(),
        Refusal::Length
    );
    let mut long = clean.clone();
    long.push(0);
    assert_eq!(layout::check_load(&long).unwrap_err(), Refusal::Length);
    assert_eq!(layout::check_load(&[]).unwrap_err(), Refusal::Length);

    for &w in &ADDRESS_WORDS {
        for byte in 0..4 {
            let mut bad = clean.clone();
            bad[w + byte] = 1;
            assert_eq!(
                layout::check_load(&bad).unwrap_err(),
                Refusal::Address(w),
                "a byte of the word at {w:#x}"
            );
        }
    }
    // The byte just past each word is the caller's.
    let mut beside = clean.clone();
    beside[0x150] = 0xff;
    beside[0x23] = 0xff;
    assert!(layout::check_load(&beside).is_ok());
    assert!(layout::check_load(&image).is_err(), "nothing zeroed");
}

#[test]
fn a_region_is_read_only_inside_its_buffer() {
    let buffer = vec![1u8; STATISTICS_BYTES];
    let last = layout::STATISTICS_REGIONS[14];
    assert_eq!(last.of(&buffer).unwrap().len(), 0x300);
    assert_eq!(last.of(&buffer[..STATISTICS_BYTES - 1]), None);
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
fn the_checks_allocate_nothing() {
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
