//! The descriptor's fields, the registers and the codes, as gcc on x86_64 and clang for riscv64
//! lay out the driver's declarations: each field set alone, read back as the words in memory.

use super::*;

fn one(set: impl FnOnce(&mut Descriptor)) -> [u32; 6] {
    let mut d = Descriptor::default();
    set(&mut d);
    d.words()
}

#[test]
fn every_descriptor_field_sits_where_the_c_compiler_puts_it() {
    assert_eq!(one(|d| d.rotation_mode = 3), [0x3, 0, 0, 0, 0, 0]);
    assert_eq!(one(|d| d.x_mirror = true), [0x4, 0, 0, 0, 0, 0]);
    assert_eq!(one(|d| d.y_mirror = true), [0x8, 0, 0, 0, 0, 0]);
    assert_eq!(one(|d| d.pixel_width = 3), [0x300, 0, 0, 0, 0, 0]);
    assert_eq!(one(|d| d.channel = 7), [0x7_0000, 0, 0, 0, 0, 0]);
    assert_eq!(one(|d| d.pause = true), [0x2000_0000, 0, 0, 0, 0, 0]);
    assert_eq!(one(|d| d.node_intr = true), [0x4000_0000, 0, 0, 0, 0, 0]);
    assert_eq!(
        one(|d| d.src_addr = 0x1122_3344),
        [0, 0x1122_3344, 0, 0, 0, 0]
    );
    assert_eq!(one(|d| d.width = 0xabcd), [0, 0, 0xabcd, 0, 0, 0]);
    assert_eq!(one(|d| d.height = 0x1234), [0, 0, 0x1234_0000, 0, 0, 0]);
    assert_eq!(one(|d| d.src_stride = 0x5678), [0, 0, 0, 0x5678, 0, 0]);
    assert_eq!(one(|d| d.dst_stride = 0x9abc), [0, 0, 0, 0x9abc_0000, 0, 0]);
    assert_eq!(
        one(|d| d.dst_addr = 0x5566_7788),
        [0, 0, 0, 0, 0x5566_7788, 0]
    );
    assert_eq!(one(|d| d.next = 0x99aa_bbcc), [0, 0, 0, 0, 0, 0x99aa_bbcc]);
    // Out-of-range codes stay in their fields.
    assert_eq!(
        one(|d| {
            d.rotation_mode = u32::MAX;
            d.pixel_width = u32::MAX;
            d.channel = u32::MAX;
        }),
        [0x7_0303, 0, 0, 0, 0, 0]
    );
    let bytes = Descriptor {
        src_addr: 0x1122_3344,
        width: 0xabcd,
        ..Descriptor::default()
    }
    .to_bytes();
    assert_eq!(DESCRIPTOR_BYTES, 24);
    assert_eq!(&bytes[4..10], [0x44, 0x33, 0x22, 0x11, 0xcd, 0xab]);
}

#[test]
fn the_registers_match_the_c_compiler() {
    assert_eq!(reg::CTRL, 16);
    assert_eq!(reg::LLI_BASE, 20);
    assert_eq!(reg::CH_CNT, 24);
    assert_eq!(reg::CURRENT_LLT, 24 + 8 * 4);
    assert_eq!(reg::CURRENT_DADDR, 68);
    assert_eq!(reg::WEIGHT, 72);
    assert_eq!(reg::END, 80);
    assert_eq!(
        (CH_EN, DONE_INT, INT_MASK, DMA_CFG, DMA_WEIGHT),
        (0x10, 0x4_0000, 0x7_0000, 0x7ff, 0x0044_4210)
    );
    assert_eq!((REGISTERS, REGISTERS_LEN, IRQ), (0x8080_0000, 0x1000, 140));
    assert_eq!(DDR_END, 0x8000_0000);
}

#[test]
fn the_codes_are_the_drivers() {
    assert_eq!(
        (
            pixel::BITS_8,
            pixel::BITS_16,
            pixel::BITS_24,
            pixel::BITS_32
        ),
        (0, 1, 2, 3)
    );
    assert_eq!((turn::R0, turn::R90, turn::R180, turn::R270), (0, 1, 2, 3));
    assert_eq!(
        (
            drm::ROTATE_0,
            drm::ROTATE_90,
            drm::ROTATE_180,
            drm::ROTATE_270
        ),
        (1, 2, 4, 8)
    );
    assert_eq!((drm::REFLECT_X, drm::REFLECT_Y), (0x10, 0x20));
    assert_eq!(fourcc::ARGB1555, 0x3531_5241);
    assert_eq!(fourcc::ARGB4444, 0x3231_5241);
    assert_eq!(fourcc::RGB565, 0x3631_4752);
    assert_eq!(fourcc::RGB888, 0x3432_4752);
    assert_eq!(fourcc::BGR888, 0x3432_4742);
    assert_eq!(fourcc::ARGB8888, 0x3432_5241);
    assert_eq!(fourcc::NV12, 0x3231_564e);
    assert_eq!(fourcc::NV21, 0x3132_564e);
    assert_eq!(fourcc::NV16, 0x3631_564e);
    assert_eq!(fourcc::NV61, 0x3136_564e);
}
