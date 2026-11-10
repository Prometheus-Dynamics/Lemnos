//! Layouts, request numbers and the access table: every expected value is written out by hand.

use core::mem::{offset_of, size_of};

use crate::access::{STATISTICS, STATISTICS_REGISTERS, WRITABLE, allowed, readable, writable};
use crate::regs::{
    ADDRESS_REGISTERS, AE_MEAN_0, AE_V_SIZE, AWB_V_SIZE, CTRL_GAMMA_OUT_ENABLE, CTRL_GEN_CFG_UPD,
    HIST_BIN_0, HIST_V_SIZE, HIST_WEIGHT_44, ISP_AWB_MEAN, ISP_AWB_WHITE_CNT, ISP_CTRL, ISP_ICR,
    ISP_IMSC, ISP_MIS, LSC_TABLE_SEL, MI_BASE, WINDOW_BYTES,
};
use crate::uapi::{
    DQEVENT, Event, READ_REG, RESET, RegPair, SUBSCRIBE_EVENT, Subscription, UNSUBSCRIBE_EVENT,
    WRITE_REG,
};

#[test]
fn layouts_and_numbers_match_the_header() {
    assert_eq!(size_of::<RegPair>(), 8);
    assert_eq!(offset_of!(RegPair, value), 4);
    assert_eq!(size_of::<Subscription>(), 8);
    assert_eq!(offset_of!(Subscription, id), 4);
    #[cfg(target_pointer_width = "64")]
    {
        assert_eq!(size_of::<Event>(), 24);
        assert_eq!(offset_of!(Event, id), 4);
        assert_eq!(offset_of!(Event, irq_value), 8);
        assert_eq!(offset_of!(Event, timestamp), 16);
        assert_eq!(DQEVENT, 0x8018_7606);
    }
    assert_eq!(RESET, 0x4004_7601);
    assert_eq!(READ_REG, 0xC008_7602);
    assert_eq!(WRITE_REG, 0x4008_7603);
    assert_eq!(SUBSCRIBE_EVENT, 0x4008_7604);
    assert_eq!(UNSUBSCRIBE_EVENT, 0x4008_7605);
}

#[test]
fn every_allowed_register_is_inside_the_window() {
    assert_eq!(WINDOW_BYTES, 0x9000);
    for span in WRITABLE.iter().chain(&STATISTICS) {
        assert!(span.count > 0);
        assert_eq!(span.first % 4, 0);
        assert!(span.end() <= u64::from(WINDOW_BYTES), "{span:?}");
    }
    // No two spans overlap.
    let all: Vec<_> = WRITABLE.iter().chain(&STATISTICS).collect();
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert!(
                a.end() <= u64::from(b.first) || b.end() <= u64::from(a.first),
                "{a:?} {b:?}"
            );
        }
    }
}

#[test]
fn nothing_past_the_window_or_off_a_word_is_reached() {
    for addr in [
        WINDOW_BYTES,
        WINDOW_BYTES + 4,
        0x0001_0000,
        0x8000_0000,
        u32::MAX,
        u32::MAX - 3,
        ISP_CTRL + 1,
        ISP_CTRL + 2,
        AE_MEAN_0 + 3,
    ] {
        assert_eq!(writable(addr), None, "{addr:#x}");
        assert!(!readable(addr), "{addr:#x}");
    }
    // Every word of the window and past it, by brute force over the first 64 KiB.
    for addr in 0..0x1_0000u32 {
        if writable(addr).is_some() || readable(addr) {
            assert!(addr % 4 == 0 && addr + 4 <= WINDOW_BYTES, "{addr:#x}");
        }
    }
}

#[test]
fn no_register_that_takes_an_address_or_starts_dma_is_reachable() {
    for addr in ADDRESS_REGISTERS {
        assert_eq!(writable(addr), None, "{addr:#x}");
        assert!(!readable(addr), "{addr:#x}");
    }
    // The whole memory interface (from the second-generation interface's control to the end
    // of the first-generation block) and the front end are out.
    for addr in (0x1300..0x2000)
        .step_by(4)
        .chain((0x3D00..0x3E00).step_by(4))
    {
        assert_eq!(writable(addr), None, "{addr:#x}");
    }
    assert_eq!(writable(MI_BASE), None);
    // The interrupt registers are the driver's: mask, status and clear.
    for addr in [ISP_IMSC, ISP_MIS, ISP_ICR] {
        assert_eq!(writable(addr), None, "{addr:#x}");
        assert!(!readable(addr), "{addr:#x}");
    }
}

#[test]
fn isp_ctrl_takes_only_its_gamma_and_update_bits() {
    assert!(allowed(ISP_CTRL, CTRL_GEN_CFG_UPD));
    assert!(allowed(ISP_CTRL, CTRL_GAMMA_OUT_ENABLE | CTRL_GEN_CFG_UPD));
    assert!(allowed(ISP_CTRL, 0));
    // Bit 0 is the ISP's enable; bit 4 its output.
    assert!(!allowed(ISP_CTRL, 1));
    assert!(!allowed(ISP_CTRL, 1 << 4));
    assert!(!allowed(ISP_CTRL, u32::MAX));
    assert_eq!(writable(ISP_CTRL), Some(0xE00));
}

#[test]
fn the_tuning_registers_are_writable_and_the_measurements_readable() {
    for addr in [
        LSC_TABLE_SEL,
        HIST_V_SIZE,
        HIST_WEIGHT_44,
        AE_V_SIZE,
        AWB_V_SIZE,
    ] {
        assert_eq!(writable(addr), Some(u32::MAX), "{addr:#x}");
    }
    for addr in [
        AE_MEAN_0,
        AE_MEAN_0 + 24 * 4,
        HIST_BIN_0,
        HIST_BIN_0 + 15 * 4,
    ] {
        assert!(readable(addr), "{addr:#x}");
        assert_eq!(writable(addr), None, "{addr:#x}");
    }
    assert!(!readable(AE_MEAN_0 + 25 * 4));
    assert!(readable(ISP_AWB_WHITE_CNT) && readable(ISP_AWB_MEAN));
    assert_eq!(
        STATISTICS.iter().map(|s| s.count as usize).sum::<usize>(),
        STATISTICS_REGISTERS
    );
}

#[cfg(target_os = "linux")]
#[test]
fn the_node_refuses_before_reaching_the_device() {
    use std::io::ErrorKind;

    // A plain file: any request that reaches it fails with `ENOTTY`, so `InvalidInput` means the
    // check refused it first.
    let dir = std::env::temp_dir().join(format!("k230isp-sys-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("regs");
    std::fs::write(&path, [0u8; 8]).unwrap();
    let node = crate::IspNode::open(&path).unwrap();
    for (addr, value) in [
        (WINDOW_BYTES, 0),
        (u32::MAX, 0),
        (MI_BASE, 0),
        (ISP_IMSC, 0),
        (ISP_CTRL, 1),
    ] {
        let err = node.write(addr, value).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput, "{addr:#x}");
    }
    assert_eq!(
        node.read(ISP_MIS).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    // An allowed register reaches the file, which is no device.
    let err = node.write(AE_V_SIZE, 64).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::ENOTTY));
    let err = node.read(AE_MEAN_0).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::ENOTTY));
    assert_eq!(
        node.next_event().unwrap_err().raw_os_error(),
        Some(libc::ENOTTY)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
