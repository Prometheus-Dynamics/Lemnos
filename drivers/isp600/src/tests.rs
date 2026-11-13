//! The layouts: every expected value here is written out by hand. The library half: none of it
//! needs the `device` feature.

use lemnos_measure::test_alloc::allocations;

use crate::layout::{
    self, ADDRESS_WORDS, EXTENT_WORDS, LOAD_BYTES, LOAD_REGISTERS_OFFSET, Refusal, Region,
    STATISTICS_BYTES, module, reg,
};
use crate::tuning::{self, ChannelExpand, Error, Image, OutOfRange};

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
    // Anything else may be anything; the address and extent words must be zero.
    let mut clean = image.clone();
    for &w in ADDRESS_WORDS.iter().chain(&EXTENT_WORDS) {
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

#[test]
fn the_load_check_allocates_nothing() {
    let image = vec![0u8; LOAD_BYTES];
    let ((), allocated) = allocations(|| {
        for _ in 0..1000 {
            assert!(layout::check_load(&image).is_ok());
        }
    });
    assert_eq!(allocated, 0);
}

#[test]
fn the_extent_words_are_the_input_valid_and_lossless_words() {
    assert_eq!(
        EXTENT_WORDS,
        [
            0x120, 0x124, 0x128, 0x380, 0x384, 0x388, 0x38c, 0x390, 0x394
        ]
    );
}

#[test]
fn a_load_image_is_refused_for_any_extent_word() {
    for &word in &EXTENT_WORDS {
        let mut image = vec![0u8; LOAD_BYTES];
        image[word + 3] = 0x80;
        assert_eq!(
            layout::check_load(&image).unwrap_err(),
            Refusal::Extent(word),
            "word {word:#x}"
        );
    }
    assert!(layout::check_load(&vec![0u8; LOAD_BYTES]).is_ok());
}

#[test]
fn the_module_enable_bits_are_the_headers_bits() {
    assert_eq!(tuning::MODULE_ENABLE_BITS, 0x3f07_ffff);
}

#[test]
fn channel_expand_encodes_the_headers_fields() {
    let full = ChannelExpand {
        black_level: true,
        inverse_black_level: true,
        input_bits: 31,
        output_bits: 31,
    };
    assert_eq!(full.encode(), Ok(0x001f_01f3));
    let mid = ChannelExpand {
        black_level: false,
        inverse_black_level: true,
        input_bits: 12,
        output_bits: 10,
    };
    assert_eq!(mid.encode(), Ok(0x000a_00c2));
    assert_eq!(ChannelExpand::default().encode(), Ok(0));
}

#[test]
fn a_bit_depth_past_its_five_bits_is_refused() {
    let input = ChannelExpand {
        input_bits: 32,
        ..ChannelExpand::default()
    };
    assert_eq!(
        input.encode(),
        Err(OutOfRange {
            field: "input_bit",
            value: 32,
            max: 31
        })
    );
    let output = ChannelExpand {
        output_bits: 255,
        ..ChannelExpand::default()
    };
    assert!(matches!(
        output.encode(),
        Err(OutOfRange {
            field: "output_bit",
            value: 255,
            ..
        })
    ));
}

#[test]
fn a_channel_expand_setter_writes_its_register_and_nothing_else() {
    let mut bytes = [0u8; LOAD_BYTES];
    let mut image = Image::new(&mut bytes);
    image
        .set_channel_expand(
            1,
            ChannelExpand {
                black_level: true,
                inverse_black_level: false,
                input_bits: 12,
                output_bits: 10,
            },
        )
        .unwrap();
    let at = LOAD_REGISTERS_OFFSET + reg::CH1_EXPAND_CFG0;
    let mut expected = [0u8; LOAD_BYTES];
    expected[at..at + 4].copy_from_slice(&0x000a_00c1u32.to_le_bytes());
    assert_eq!(bytes, expected);
}

#[test]
fn the_setters_refuse_a_channel_or_a_range_they_cannot_write() {
    let mut bytes = [0u8; LOAD_BYTES];
    let mut image = Image::new(&mut bytes);
    assert_eq!(
        image.set_channel_expand(3, ChannelExpand::default()),
        Err(Error::Channel(3))
    );
    assert!(matches!(
        image.set_channel_expand(
            0,
            ChannelExpand {
                input_bits: 32,
                ..ChannelExpand::default()
            }
        ),
        Err(Error::Range(OutOfRange {
            field: "input_bit",
            ..
        }))
    ));
    assert_eq!(bytes, [0u8; LOAD_BYTES]);
}

#[test]
fn module_enables_write_the_bypass_word_and_refuse_reserved_bits() {
    let mut bytes = [0u8; LOAD_BYTES];
    let mut image = Image::new(&mut bytes);
    image.set_modules(module::BLC | module::AE).unwrap();
    let at = LOAD_REGISTERS_OFFSET + reg::MODULE_BYPASS1;
    assert_eq!(&bytes[at..at + 4], &0x0100_0040u32.to_le_bytes());
    let mut image = Image::new(&mut bytes);
    assert_eq!(image.set_modules(1 << 19), Err(Error::Reserved(1 << 19)));
    assert_eq!(image.set_modules(1 << 30), Err(Error::Reserved(1 << 30)));
    assert_eq!(&bytes[at..at + 4], &0x0100_0040u32.to_le_bytes());
}

#[test]
fn the_temporal_record_enable_writes_its_word() {
    let mut bytes = [0u8; LOAD_BYTES];
    let mut image = Image::new(&mut bytes);
    image.set_temporal_record(true);
    let at = LOAD_REGISTERS_OFFSET + reg::D3D_CFG0;
    assert_eq!(&bytes[at..at + 4], &1u32.to_le_bytes());
    Image::new(&mut bytes).set_temporal_record(false);
    assert_eq!(&bytes[at..at + 4], &0u32.to_le_bytes());
}

#[test]
fn no_setter_writes_an_address_or_extent_word() {
    let mut bytes = [0u8; LOAD_BYTES];
    let mut image = Image::new(&mut bytes);
    for channel in 0..tuning::expand::CHANNELS {
        image
            .set_channel_expand(
                channel,
                ChannelExpand {
                    black_level: true,
                    inverse_black_level: true,
                    input_bits: 31,
                    output_bits: 31,
                },
            )
            .unwrap();
    }
    image.set_modules(0x3f07_ffff).unwrap();
    image.set_temporal_record(true);
    assert!(image.check().is_ok());
    for &word in ADDRESS_WORDS.iter().chain(EXTENT_WORDS.iter()) {
        assert_eq!(&bytes[word..word + 4], &[0; 4], "word {word:#x}");
    }
}
