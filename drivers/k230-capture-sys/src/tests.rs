//! Layouts, the access table, the memory checks and the register sequences against a fake device
//! and fake DMA memory: every expected value is written out by hand.

use core::mem::{offset_of, size_of};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::{Arc, Mutex};

use lemnos_driver_k230isp_sys::uapi::{EVENT_IRQ_TYPE, Event};
use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::{DmaBuffer, DmaBuffers};
use lemnos_hal::mock::MockDma;

use crate::access::{READ_ONLY, WRITABLE, allowed, readable, writable};
use crate::memory::{FrameMemory, MemoryError, Plane, Planes};
use crate::mipi::{
    DATA_RAW8, DATA_RAW10, DATA_RAW12, DevAttr, Interface, LinkError, MipiAttr, RESET,
    SET_DEV_ATTR, SET_STREAM,
};
use crate::regs::{
    ADDRESS_REGISTERS, ISP_CTRL, ISP_CTRL_ENABLE, ISP_CTRL_GEN_CFG_UPD, MIV2_CTRL, MIV2_IMSC,
    MP_CB_BASE_AD_INIT, MP_CB_BASE_AD_INIT2, MP_CB_OFFS_CNT_INIT, MP_CB_SIZE_INIT,
    MP_CR_BASE_AD_INIT, MP_CR_BASE_AD_INIT2, MP_CR_OFFS_CNT_INIT, MP_CR_SIZE_INIT, MP_CTRL, MP_FMT,
    MP_JDP_BASE_AD_INIT, MP_JDP_OFFS_CNT_INIT, MP_JDP_SIZE_INIT, MP_RAW_BASE_AD_INIT,
    MP_RAW_BASE_AD_SHD, MP_RAW_OFFS_CNT_INIT, MP_RAW_SIZE_INIT, MP_Y_BASE_AD_INIT,
    MP_Y_BASE_AD_INIT2, MP_Y_BASE_AD_SHD, MP_Y_LLENGTH, MP_Y_OFFS_CNT_INIT, MP_Y_SIZE_INIT,
    WINDOW_BYTES,
};
use crate::{Capture, CaptureError, EID_MIV2_MIS, MiEvent, Output, RELEASE_AFTER};

// The fake ISP node.

#[derive(Default)]
struct PortState {
    regs: HashMap<u32, u32>,
    /// Every write that reached the device, in order.
    writes: Vec<(u32, u32)>,
    reads: Vec<u32>,
    events: VecDeque<Event>,
    subscribed: Vec<u32>,
    fail_write: Option<u32>,
}

/// Stands in for the `vvcam-isp` node: registers in a map, every request logged.
#[derive(Clone, Default)]
pub(crate) struct FakePort(Arc<Mutex<PortState>>);

impl FakePort {
    fn with<R>(&self, f: impl FnOnce(&mut PortState) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }

    pub(crate) fn read_raw(&self, addr: u32) -> io::Result<u32> {
        self.with(|s| {
            s.reads.push(addr);
            Ok(s.regs.get(&addr).copied().unwrap_or(0))
        })
    }

    pub(crate) fn write_raw(&self, addr: u32, value: u32) -> io::Result<()> {
        self.with(|s| {
            if s.fail_write == Some(addr) {
                return Err(io::Error::from_raw_os_error(5));
            }
            s.writes.push((addr, value));
            s.regs.insert(addr, value);
            Ok(())
        })
    }

    pub(crate) fn subscribe(&self, id: u32) -> io::Result<()> {
        self.with(|s| s.subscribed.push(id));
        Ok(())
    }

    pub(crate) fn dequeue(&self) -> io::Result<Option<Event>> {
        Ok(self.with(|s| s.events.pop_front()))
    }

    fn writes(&self) -> Vec<(u32, u32)> {
        self.with(|s| std::mem::take(&mut s.writes))
    }

    fn set(&self, addr: u32, value: u32) {
        self.with(|s| s.regs.insert(addr, value));
    }
}

// Fake DMA memory: addresses from 0x4000_0000, or the one a test asks for.

#[derive(Default)]
struct DmaState {
    next: u32,
    address: u64,
    /// The address the next allocation gets instead.
    force: Option<u64>,
    /// Bytes to give less than asked, once.
    short: u64,
    live: HashMap<u32, (u64, Vec<u8>)>,
    released: Vec<u32>,
    to_device: Vec<u32>,
    to_cpu: Vec<u32>,
}

#[derive(Clone)]
struct FakeDma(Arc<Mutex<DmaState>>);

impl FakeDma {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(DmaState {
            address: 0x4000_0000,
            ..DmaState::default()
        })))
    }

    fn with<R>(&self, f: impl FnOnce(&mut DmaState) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }

    /// The block writes `data` at `offset` of the memory at bus address `address`.
    fn device_write(&self, address: u32, offset: usize, data: &[u8]) {
        self.with(|s| {
            let (_, bytes) = s
                .live
                .values_mut()
                .find(|(a, _)| *a == u64::from(address))
                .unwrap();
            bytes[offset..offset + data.len()].copy_from_slice(data);
        });
    }
}

impl DmaBuffers for FakeDma {
    type Error = ErrorKind;

    fn allocate(&mut self, len: u64) -> Result<DmaBuffer, ErrorKind> {
        self.with(|s| {
            s.next += 1;
            let address = s.force.take().unwrap_or(s.address);
            s.address += len.next_multiple_of(0x1000);
            let len = len - std::mem::take(&mut s.short);
            s.live
                .insert(s.next, (address, vec![0; usize::try_from(len).unwrap()]));
            Ok(DmaBuffer {
                id: s.next,
                len,
                device_address: address,
            })
        })
    }

    fn release(&mut self, id: u32) -> Result<(), ErrorKind> {
        self.with(|s| {
            s.released.push(id);
            s.live.remove(&id).map(|_| ()).ok_or(ErrorKind::NotFound)
        })
    }

    fn write(&mut self, _id: u32, _offset: u64, _data: &[u8]) -> Result<(), ErrorKind> {
        Err(ErrorKind::Unsupported)
    }

    fn read(&mut self, id: u32, offset: u64, out: &mut [u8]) -> Result<(), ErrorKind> {
        self.with(|s| {
            let (_, bytes) = s.live.get(&id).ok_or(ErrorKind::NotFound)?;
            let at = usize::try_from(offset).unwrap();
            out.copy_from_slice(
                bytes
                    .get(at..at + out.len())
                    .ok_or(ErrorKind::InvalidInput)?,
            );
            Ok(())
        })
    }

    fn sync_for_device(&mut self, id: u32) -> Result<(), ErrorKind> {
        self.with(|s| s.to_device.push(id));
        Ok(())
    }

    fn sync_for_cpu(&mut self, id: u32) -> Result<(), ErrorKind> {
        self.with(|s| s.to_cpu.push(id));
        Ok(())
    }
}

fn capture() -> (Capture, FakePort, FakeDma) {
    let port = FakePort::default();
    let dma = FakeDma::new();
    let capture = Capture::with_port(port.clone(), Box::new(dma.clone()));
    (capture, port, dma)
}

/// NV12 at 64x4: a 256-byte Y plane and a 128-byte CbCr plane after it.
fn nv12() -> Planes {
    Planes::new(
        Plane {
            offset: 0,
            size: 256,
        },
        Some(Plane {
            offset: 256,
            size: 128,
        }),
        None,
    )
    .unwrap()
}

// Layouts and numbers.

#[test]
fn mipi_layout_and_numbers_match_the_header() {
    assert_eq!(size_of::<Interface>(), 48);
    assert_eq!(offset_of!(Interface, vcid), 4);
    assert_eq!(offset_of!(Interface, bit_mode), 8);
    assert_eq!(offset_of!(Interface, data_type), 12);
    assert_eq!(offset_of!(Interface, h_time), 16);
    assert_eq!(offset_of!(Interface, v_time), 32);
    assert_eq!(size_of::<MipiAttr>(), 404);
    assert_eq!(offset_of!(MipiAttr, lane_id), 384);
    assert_eq!(offset_of!(MipiAttr, phy_freq), 400);
    assert_eq!(size_of::<DevAttr>(), 424);
    assert_eq!(offset_of!(DevAttr, image_rect), 4);
    assert_eq!(offset_of!(DevAttr, mipi), 20);
    assert_eq!(RESET, 0x4004_7601);
    assert_eq!(SET_DEV_ATTR, 0x41A8_7602);
    assert_eq!(SET_STREAM, 0x4004_7603);
}

#[test]
fn a_link_becomes_the_request_the_kernel_reads() {
    let attr = DevAttr::csi(2, 1, 10, 900, 1920, 1080).unwrap();
    assert_eq!(attr.mode, 0);
    assert_eq!(attr.mipi.lane_id, [0, 1, -1, -1, -1, -1, -1, -1]);
    // 900 Mbit/s is past 800: the next rate up.
    assert_eq!(attr.mipi.phy_freq, 1200);
    let itf = attr.mipi.interfaces[0];
    assert_eq!((itf.enable, itf.vcid, itf.data_type), (1, 1, DATA_RAW10));
    assert!(
        attr.mipi.interfaces[1..]
            .iter()
            .all(|i| *i == Interface::default())
    );
    assert_eq!(
        (attr.image_rect.width, attr.image_rect.height),
        (1920, 1080)
    );

    let four = DevAttr::csi(4, 0, 8, 800, 640, 480).unwrap();
    assert_eq!(four.mipi.lane_id, [0, 1, 2, 3, -1, -1, -1, -1]);
    assert_eq!(four.mipi.phy_freq, 800);
    assert_eq!(four.mipi.interfaces[0].data_type, DATA_RAW8);
    let twelve = DevAttr::csi(1, 0, 12, 1600, 640, 480).unwrap();
    assert_eq!(twelve.mipi.interfaces[0].data_type, DATA_RAW12);
    assert_eq!(twelve.mipi.lane_id[..2], [0, -1]);

    // What the kernel would turn into something else is refused instead.
    assert_eq!(DevAttr::csi(3, 0, 10, 800, 64, 64), Err(LinkError::Lanes));
    assert_eq!(DevAttr::csi(0, 0, 10, 800, 64, 64), Err(LinkError::Lanes));
    assert_eq!(DevAttr::csi(8, 0, 10, 800, 64, 64), Err(LinkError::Lanes));
    assert_eq!(
        DevAttr::csi(2, 4, 10, 800, 64, 64),
        Err(LinkError::VirtualChannel)
    );
    assert_eq!(DevAttr::csi(2, 0, 14, 800, 64, 64), Err(LinkError::Depth));
    assert_eq!(DevAttr::csi(2, 0, 10, 1601, 64, 64), Err(LinkError::Rate));
    assert_eq!(DevAttr::csi(2, 0, 10, 0, 64, 64), Err(LinkError::Rate));
}

// The access table.

#[test]
fn every_allowed_register_is_inside_the_window_and_none_overlap() {
    for span in &WRITABLE {
        assert!(span.count > 0);
        assert_eq!(span.first % 4, 0);
        assert!(span.end() <= u64::from(WINDOW_BYTES), "{span:?}");
    }
    for (i, a) in WRITABLE.iter().enumerate() {
        for b in &WRITABLE[i + 1..] {
            assert!(
                a.end() <= u64::from(b.first) || b.end() <= u64::from(a.first),
                "{a:?} {b:?}"
            );
        }
    }
}

#[test]
fn no_address_extent_or_path_register_is_writable() {
    for addr in ADDRESS_REGISTERS.iter().chain(&[MIV2_CTRL, MP_CTRL]) {
        assert_eq!(writable(*addr), None, "{addr:#x}");
        assert!(readable(*addr), "{addr:#x}");
    }
    // Nor anything the k230isp sys crate names as taking an address or starting DMA.
    for addr in lemnos_driver_k230isp_sys::regs::ADDRESS_REGISTERS {
        assert_eq!(writable(addr), None, "{addr:#x}");
    }
    // Every word of the first 64 KiB: what is writable is exactly the table, inside the window.
    let mut count = 0;
    for addr in 0..0x1_0000u32 {
        if writable(addr).is_some() {
            count += 1;
            assert!(addr % 4 == 0 && addr + 4 <= WINDOW_BYTES, "{addr:#x}");
        }
    }
    assert_eq!(count, WRITABLE.iter().map(|s| s.count).sum::<u32>());
    for addr in [
        WINDOW_BYTES,
        u32::MAX,
        u32::MAX - 3,
        ISP_CTRL + 2,
        MP_FMT + 1,
    ] {
        assert_eq!(writable(addr), None, "{addr:#x}");
        assert!(!readable(addr), "{addr:#x}");
    }
    assert!(READ_ONLY.iter().all(|a| writable(*a).is_none()));
}

#[test]
fn a_write_outside_the_table_or_a_mask_never_reaches_the_device() {
    let (capture, port, _) = capture();
    for (addr, value) in [
        (MP_Y_BASE_AD_INIT, 0x8000_0000),
        (MP_Y_SIZE_INIT, 0x1000_0000),
        (MIV2_CTRL, 1),
        (MP_CTRL, 0x8),
        (WINDOW_BYTES, 0),
        (u32::MAX - 3, 0),
        // isp_ctrl takes only its five bits: bit 11 is the gamma-out enable, the k230isp driver's.
        (ISP_CTRL, 1 << 11),
        (MIV2_IMSC, 1 << 3),
    ] {
        assert!(!allowed(addr, value), "{addr:#x}");
        assert!(matches!(
            capture.write(addr, value),
            Err(CaptureError::Refused)
        ));
    }
    assert!(matches!(
        capture.read(WINDOW_BYTES),
        Err(CaptureError::Refused)
    ));
    assert_eq!(port.writes(), []);
    assert!(port.with(|s| s.reads.is_empty()));
}

#[test]
fn a_masked_write_keeps_the_other_bits() {
    let (capture, port, _) = capture();
    port.set(ISP_CTRL, 1 << 11);
    capture
        .write(ISP_CTRL, ISP_CTRL_ENABLE | ISP_CTRL_GEN_CFG_UPD)
        .unwrap();
    assert_eq!(port.writes(), [(ISP_CTRL, (1 << 11) | 1 | 0x400)]);
    // A whole-word register is written as it stands, unread.
    capture.write(MP_Y_LLENGTH, 1920).unwrap();
    assert_eq!(port.writes(), [(MP_Y_LLENGTH, 1920)]);
}

// The memory checks.

#[test]
fn memory_the_block_cannot_name_is_given_back_and_refused() {
    let dma = FakeDma::new();
    let mut d = dma.clone();
    dma.with(|s| s.force = Some(0x4000_0008));
    assert_eq!(
        FrameMemory::allocate(&mut d, 4096),
        Err(Err(MemoryError::Unaligned))
    );
    // The last byte at 4 GiB is one too many.
    dma.with(|s| s.force = Some(0xFFFF_F000 + 16));
    assert_eq!(
        FrameMemory::allocate(&mut d, 4096),
        Err(Err(MemoryError::Unreachable))
    );
    dma.with(|s| s.short = 16);
    assert_eq!(
        FrameMemory::allocate(&mut d, 4096),
        Err(Err(MemoryError::Short))
    );
    assert_eq!(dma.with(|s| s.released.clone()), [1, 2, 3]);
    assert!(dma.with(|s| s.live.is_empty()));
    // Ending exactly at 4 GiB is fine.
    dma.with(|s| s.force = Some(0xFFFF_F000));
    let m = FrameMemory::allocate(&mut d, 4096).unwrap();
    assert_eq!((m.len(), m.address(0x10)), (4096, 0xFFFF_F010));

    // The shared mock hands out addresses at 4 GiB and up: every one is refused.
    let mut mock = MockDma::new();
    assert_eq!(
        FrameMemory::allocate(&mut mock, 4096),
        Err(Err(MemoryError::Unreachable))
    );
    assert_eq!(mock.live_buffers(), 0);
}

#[test]
fn planes_are_aligned_inside_the_size_field_and_apart() {
    let p = |offset, size| Plane { offset, size };
    assert_eq!(Planes::new(p(0, 0), None, None), Err(MemoryError::Plane));
    assert_eq!(Planes::new(p(8, 256), None, None), Err(MemoryError::Plane));
    assert_eq!(Planes::new(p(0, 250), None, None), Err(MemoryError::Plane));
    assert_eq!(
        Planes::new(p(0, 0x2000_0000), None, None),
        Err(MemoryError::Plane)
    );
    assert_eq!(
        Planes::new(p(0, 256), Some(p(240, 64)), None),
        Err(MemoryError::Overlap)
    );
    assert_eq!(
        Planes::new(p(0, 256), Some(p(256, 64)), Some(p(304, 16))),
        Err(MemoryError::Overlap)
    );
    // Offsets near the top of the field cannot wrap the end.
    let high = Planes::new(p(0xFFFF_FFF0, 0x1FFF_FFF0), None, None).unwrap();
    assert_eq!(high.end(), 0xFFFF_FFF0 + 0x1FFF_FFF0);
    let ok = Planes::new(p(0, 256), Some(p(256, 64)), Some(p(320, 64))).unwrap();
    assert_eq!(ok.end(), 384);
    assert_eq!(nv12().cr_or_y(), p(0, 256));
}

// The frame path.

#[test]
fn start_points_every_address_register_into_memory_then_turns_one_path_on() {
    let (mut capture, port, dma) = capture();
    // Planes that do not fit refuse the start before any request.
    capture.provide(384).unwrap();
    let big = Planes::new(
        Plane {
            offset: 0,
            size: 512,
        },
        None,
        None,
    )
    .unwrap();
    assert!(matches!(
        capture.start(Output::YCbCr, big),
        Err(CaptureError::Memory(MemoryError::Outside))
    ));
    assert_eq!(port.writes(), []);

    port.set(MP_CTRL, 0x8000_0001);
    capture.start(Output::YCbCr, nv12()).unwrap();
    // Memories at 0x4000_0000, 0x4000_1000, 0x4000_2000.
    let arm = |base: u32| {
        vec![
            (MP_Y_BASE_AD_INIT, base),
            (MP_Y_SIZE_INIT, 256),
            (MP_Y_OFFS_CNT_INIT, 0),
            (MP_CB_BASE_AD_INIT, base + 256),
            (MP_CB_SIZE_INIT, 128),
            (MP_CB_OFFS_CNT_INIT, 0),
            // No Cr plane in NV12: pointed at the Y plane.
            (MP_CR_BASE_AD_INIT, base),
            (MP_CR_SIZE_INIT, 256),
            (MP_CR_OFFS_CNT_INIT, 0),
            (MP_Y_BASE_AD_INIT2, base),
            (MP_CB_BASE_AD_INIT2, base + 256),
            (MP_CR_BASE_AD_INIT2, base),
            (MP_RAW_BASE_AD_INIT, base),
            (MP_RAW_SIZE_INIT, 256),
            (MP_RAW_OFFS_CNT_INIT, 0),
            (MP_JDP_BASE_AD_INIT, base),
            (MP_JDP_SIZE_INIT, 256),
            (MP_JDP_OFFS_CNT_INIT, 0),
        ]
    };
    let mut expected = vec![(MIV2_CTRL, 0)];
    expected.extend(arm(0x4000_0000));
    // Init enables and auto update on, loaded now; bit 31 kept.
    expected.push((MP_CTRL, 0x8000_0001 | 0x20 | 0x10 | 0x2 | 0x8));
    expected.extend(arm(0x4000_1000));
    expected.push((MIV2_CTRL, 1));
    let writes = port.writes();
    assert_eq!(writes, expected);
    // Every address written lies inside one memory of the ring.
    for (addr, value) in &writes {
        if ADDRESS_REGISTERS.contains(addr) && *value >= 0x4000_0000 {
            assert!((0x4000_0000..0x4000_0000 + 0x1000 * 2 + 384).contains(value));
        }
    }
    assert_eq!(dma.with(|s| s.to_device.clone()), [1, 2, 3]);
    assert!(capture.running());

    // Started twice is refused.
    assert!(matches!(
        capture.start(Output::Raw, nv12()),
        Err(CaptureError::Refused)
    ));

    // Re-arming memory 2.
    capture.rearm(2).unwrap();
    assert_eq!(port.writes(), arm(0x4000_2000));
    assert!(matches!(capture.rearm(3), Err(CaptureError::Refused)));
}

#[test]
fn the_raw_path_reads_its_own_shadow() {
    let (mut capture, port, _) = capture();
    capture.provide(256).unwrap();
    let raw = Planes::new(
        Plane {
            offset: 0,
            size: 256,
        },
        None,
        None,
    )
    .unwrap();
    capture.start(Output::Raw, raw).unwrap();
    assert_eq!(port.writes().last(), Some(&(MIV2_CTRL, 2)));
    port.set(MP_RAW_BASE_AD_SHD, 0x4000_1000);
    port.set(MP_Y_BASE_AD_SHD, 0x4000_0000);
    assert_eq!(capture.active().unwrap(), Some(1));
    port.set(MP_RAW_BASE_AD_SHD, 0x1234_0000);
    assert_eq!(capture.active().unwrap(), None);
}

#[test]
fn memory_is_kept_while_the_path_runs_and_until_its_last_frame_is_done() {
    let (mut capture, port, dma) = capture();
    capture.provide(384).unwrap();
    capture.start(Output::YCbCr, nv12()).unwrap();
    assert!(matches!(capture.release(), Err(CaptureError::Refused)));
    assert!(matches!(capture.provide(4096), Err(CaptureError::Refused)));

    // A stop whose request fails leaves the path counted as on.
    port.with(|s| s.fail_write = Some(MIV2_CTRL));
    assert!(capture.stop().is_err());
    assert!(capture.running());
    assert!(matches!(capture.release(), Err(CaptureError::Refused)));
    port.with(|s| s.fail_write = None);

    port.writes();
    capture.stop().unwrap();
    assert_eq!(port.writes(), [(MIV2_CTRL, 0), (MP_CTRL, 0x3A)]);
    assert!(!capture.running());
    // Too soon after the stop.
    assert!(matches!(capture.release(), Err(CaptureError::Refused)));
    // Memory that is big enough is kept without a release.
    capture.provide(384).unwrap();
    assert!(dma.with(|s| s.released.is_empty()));
    std::thread::sleep(RELEASE_AFTER);
    // Bigger memory replaces it.
    capture.provide(8192).unwrap();
    assert_eq!(dma.with(|s| s.released.clone()), [1, 2, 3]);
    assert_eq!(capture.memory(0).unwrap().len(), 8192);
    capture.release().unwrap();
    assert!(dma.with(|s| s.live.is_empty()));
    assert!(capture.memory(0).is_none());
}

#[test]
fn dropping_a_running_capture_turns_it_off_before_the_memory_goes() {
    let (mut capture, port, dma) = capture();
    capture.provide(384).unwrap();
    capture.start(Output::YCbCr, nv12()).unwrap();
    port.writes();
    let before = std::time::Instant::now();
    drop(capture);
    assert!(before.elapsed() >= RELEASE_AFTER);
    assert_eq!(port.writes()[0], (MIV2_CTRL, 0));
    assert!(dma.with(|s| s.live.is_empty()));
}

#[test]
fn a_frame_is_read_back_only_inside_its_planes() {
    let (mut capture, _, dma) = capture();
    capture.provide(4096).unwrap();
    let mut out = [0u8; 16];
    // Nothing armed: nothing to read.
    assert!(matches!(
        capture.copy_out(0, 0, &mut out),
        Err(CaptureError::Refused)
    ));
    capture.start(Output::YCbCr, nv12()).unwrap();
    dma.device_write(0x4000_1000, 368, &[7; 16]);
    capture.take(1).unwrap();
    capture.copy_out(1, 368, &mut out).unwrap();
    assert_eq!(out, [7; 16]);
    assert_eq!(dma.with(|s| s.to_cpu.clone()), [2]);
    // One byte past the planes, an offset that wraps, and a slot past the ring.
    assert!(matches!(
        capture.copy_out(1, 369, &mut out),
        Err(CaptureError::Refused)
    ));
    assert!(matches!(
        capture.copy_out(1, u64::MAX - 4, &mut out),
        Err(CaptureError::Refused)
    ));
    assert!(matches!(
        capture.copy_out(3, 0, &mut out),
        Err(CaptureError::Refused)
    ));
}

#[test]
fn frame_ends_arrive_as_events_and_the_rest_are_passed_over() {
    let (capture, port, _) = capture();
    capture.watch_frames().unwrap();
    assert_eq!(port.with(|s| s.subscribed.clone()), [EID_MIV2_MIS]);
    assert_eq!(port.writes(), [(MIV2_IMSC, 0x3)]);
    let event = |id, irq_value, timestamp| Event {
        kind: EVENT_IRQ_TYPE,
        id,
        irq_value,
        timestamp,
    };
    port.with(|s| {
        s.events.push_back(event(0x06, 1, 5));
        s.events.push_back(event(EID_MIV2_MIS, 1, 10));
    });
    assert_eq!(
        capture.next_event().unwrap(),
        Some(MiEvent {
            status: 1,
            timestamp_ns: 10
        })
    );
    assert_eq!(capture.next_event().unwrap(), None);
}
