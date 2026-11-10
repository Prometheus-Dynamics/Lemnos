//! The configuration check and the statistics layout: every expected value is written out by
//! hand.

use lemnos_driver_k230isp_sys::regs::{
    AE_MEAN_0, AE_V_SIZE, CTRL_GAMMA_OUT_ENABLE, FE_DMA_ADDR, HIST_BIN_0, ISP_AWB_GAIN_RB,
    ISP_AWB_MEAN, ISP_AWB_WHITE_CNT, ISP_CTRL, ISP_IMSC, LSC_R_TABLE_DATA, MI_BASE, WINDOW_BYTES,
};
use lemnos_measure::test_alloc::allocations;

use crate::config::{self, CONFIG_BYTES, MAX_WRITES};
use crate::stats::{self, Raw, STATISTICS_BYTES};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

/// A configuration holding `writes`, `CONFIG_BYTES` long.
fn list(writes: &[(u32, u32)]) -> Vec<u8> {
    let mut out = vec![0u8; CONFIG_BYTES];
    out[..4].copy_from_slice(&(writes.len() as u32).to_le_bytes());
    for (i, (addr, value)) in writes.iter().enumerate() {
        out[4 + 8 * i..8 + 8 * i].copy_from_slice(&addr.to_le_bytes());
        out[8 + 8 * i..12 + 8 * i].copy_from_slice(&value.to_le_bytes());
    }
    out
}

#[test]
fn a_list_of_allowed_writes_comes_back_in_order() {
    let writes = [
        (AE_V_SIZE, 0x40),
        (ISP_AWB_GAIN_RB, 0x0100_0100),
        (LSC_R_TABLE_DATA, 0x0040_0400),
        (LSC_R_TABLE_DATA, 0x0040_0401),
        (ISP_CTRL, CTRL_GAMMA_OUT_ENABLE),
    ];
    let config = list(&writes);
    let got: Vec<_> = config::check(&config).unwrap().collect();
    assert_eq!(got, writes);
    // No writes is a list too.
    assert_eq!(config::check(&list(&[])).unwrap().count(), 0);
    // A full list.
    let full = vec![(AE_V_SIZE, 2); MAX_WRITES];
    assert_eq!(config::check(&list(&full)).unwrap().count(), MAX_WRITES);
}

#[test]
fn one_write_past_the_table_refuses_the_whole_list() {
    let good = (AE_V_SIZE, 0x40);
    for bad in [
        (WINDOW_BYTES, 0),
        (WINDOW_BYTES - 4, 0),
        (u32::MAX, 0),
        (u32::MAX - 3, 0),
        (MI_BASE, 1),
        (MI_BASE + 0x08, 0x1000_0000),
        (FE_DMA_ADDR, 0x1000_0000),
        (ISP_IMSC, 2),
        (AE_MEAN_0, 0),
        (AE_V_SIZE + 1, 0),
        // The ISP's enable bit is not the caller's.
        (ISP_CTRL, 1),
    ] {
        let config = list(&[good, good, bad]);
        assert!(config::check(&config).is_none(), "{bad:x?}");
        let config = list(&[bad, good]);
        assert!(config::check(&config).is_none(), "{bad:x?}");
    }
}

#[test]
fn a_count_past_the_limit_or_the_bytes_is_refused() {
    let mut config = list(&[(AE_V_SIZE, 2)]);
    config[..4].copy_from_slice(&((MAX_WRITES + 1) as u32).to_le_bytes());
    assert!(config::check(&config).is_none());
    config[..4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(config::check(&config).is_none());
    // Two writes said, fewer bytes given.
    let two = list(&[(AE_V_SIZE, 2), (AE_V_SIZE, 4)]);
    assert!(config::check(&two[..12]).is_none());
    assert!(config::check(&two[..19]).is_none());
    assert_eq!(config::check(&two[..20]).unwrap().count(), 2);
    assert!(config::check(&[1, 0, 0]).is_none());
    assert!(config::check(&[]).is_none());
}

#[test]
fn the_check_allocates_nothing() {
    let config = list(&vec![(AE_V_SIZE, 2); MAX_WRITES]);
    let (n, allocated) = allocations(|| config::check(&config).map(Iterator::count));
    assert_eq!(n, Some(MAX_WRITES));
    assert_eq!(allocated, 0);
}

#[test]
fn the_statistics_registers_are_in_the_layouts_order() {
    let regs: Vec<u32> = stats::registers().collect();
    assert_eq!(regs.len() * 4, STATISTICS_BYTES);
    assert_eq!(STATISTICS_BYTES, 172);
    assert_eq!(regs[0], AE_MEAN_0);
    assert_eq!(regs[24], AE_MEAN_0 + 96);
    assert_eq!(regs[25], HIST_BIN_0);
    assert_eq!(regs[40], HIST_BIN_0 + 60);
    assert_eq!(regs[41], ISP_AWB_WHITE_CNT);
    assert_eq!(regs[42], ISP_AWB_MEAN);
}

#[test]
fn the_awb_mean_decodes_in_red_green_blue_order() {
    let mut bytes = vec![0u8; STATISTICS_BYTES];
    bytes[41 * 4..42 * 4].copy_from_slice(&(0xFC00_0000u32 | 1234).to_le_bytes());
    bytes[42 * 4..43 * 4].copy_from_slice(&0x00AA_BBCCu32.to_le_bytes());
    bytes[0..4].copy_from_slice(&0x1FFu32.to_le_bytes());
    bytes[25 * 4..26 * 4].copy_from_slice(&0xFFF0_0010u32.to_le_bytes());
    let raw = Raw::decode(&bytes).unwrap();
    assert_eq!(raw.awb_mean, [0xCC, 0xAA, 0xBB]);
    assert_eq!(raw.awb_white, 1234);
    assert_eq!(raw.ae[0], 0xFF);
    assert_eq!(raw.ae[1..], [0; 24]);
    assert_eq!(raw.hist[0], 0x10);
    assert!(Raw::decode(&bytes[..STATISTICS_BYTES - 1]).is_none());
}
