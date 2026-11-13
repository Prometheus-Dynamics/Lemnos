//! The driver against a stand-in for the block and a mocked stream: every register value and
//! every expected sequence here is written out by hand.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use lemnos_contract_camera_receiver::{
    BayerOrder, CameraReceiver, Counters, FrameLayout, LinkConfig, MAX_STREAM_NAME, Packing,
};
use lemnos_core::{Health, Lifecycle, Result, Status, StreamHandle};
use lemnos_driver_k230_capture_sys::memory::{Plane, Planes};
use lemnos_driver_k230_capture_sys::mipi::DevAttr;
use lemnos_driver_k230_capture_sys::{MiEvent, Output};
use lemnos_hal::mock::MockStream;
use lemnos_measure::test_alloc::allocations;

use crate::plan::{self, Format};
use crate::{COPY_CHUNK, Hardware, K230Capture, Options, Settings, backoff_ns, parse_order};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

/// What the driver asked of the block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    Open,
    SetLink,
    Write(u32, u32),
    Provide(u64),
    Start(Output, Planes),
    Rearm(usize),
    Take(usize),
    Stop,
}

/// The block: what it was asked, and its ring as it would move.
#[derive(Default)]
struct Block {
    calls: Vec<Call>,
    /// The link handed over last.
    link: Option<DevAttr>,
    memory: [Vec<u8>; 3],
    /// The memory being written (the shadow base register), while on.
    writing: Option<usize>,
    /// The memory the next frame goes to.
    armed: usize,
    events: VecDeque<MiEvent>,
    fail_open: Option<Status>,
    fail_event: Option<Status>,
    fail_copy: bool,
}

#[derive(Clone, Default)]
struct Handle(Arc<Mutex<Block>>);

impl Handle {
    fn with<R>(&self, f: impl FnOnce(&mut Block) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }

    /// The block finishes writing `bytes` into the memory it is on, moves to the armed one, and
    /// raises the main path's frame end at `timestamp_ns`.
    fn frame(&self, bytes: &[u8], timestamp_ns: u64) {
        self.with(|b| {
            let slot = b.writing.expect("the path is on");
            b.memory[slot][..bytes.len()].copy_from_slice(bytes);
            b.writing = Some(b.armed);
            b.events.push_back(MiEvent {
                status: 0x1,
                timestamp_ns,
            });
        });
    }

    fn calls(&self) -> Vec<Call> {
        self.with(|b| b.calls.clone())
    }

    fn rearms(&self) -> Vec<usize> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Rearm(slot) => Some(slot),
                _ => None,
            })
            .collect()
    }
}

struct Hw(Handle);

impl Hw {
    fn log(&self, call: Call) {
        self.0.with(|b| b.calls.push(call));
    }
}

impl Hardware for Hw {
    fn open(&mut self) -> Result<()> {
        if let Some(status) = self.0.with(|b| b.fail_open) {
            return Err(status);
        }
        self.log(Call::Open);
        Ok(())
    }

    fn set_link(&mut self, attr: &DevAttr) -> Result<()> {
        self.0.with(|b| {
            b.link = Some(*attr);
            b.calls.push(Call::SetLink);
        });
        Ok(())
    }

    fn write(&mut self, addr: u32, value: u32) -> Result<()> {
        self.log(Call::Write(addr, value));
        Ok(())
    }

    fn provide(&mut self, bytes: u64) -> Result<()> {
        self.0.with(|b| {
            for memory in &mut b.memory {
                if (memory.len() as u64) < bytes {
                    *memory = vec![0; bytes as usize];
                }
            }
            b.calls.push(Call::Provide(bytes));
        });
        Ok(())
    }

    fn start(&mut self, output: Output, planes: Planes) -> Result<()> {
        self.0.with(|b| {
            b.writing = Some(0);
            b.armed = 1;
            b.calls.push(Call::Start(output, planes));
        });
        Ok(())
    }

    fn rearm(&mut self, slot: usize) -> Result<()> {
        self.0.with(|b| {
            b.armed = slot;
            b.calls.push(Call::Rearm(slot));
        });
        Ok(())
    }

    fn active(&mut self) -> Result<Option<usize>> {
        Ok(self.0.with(|b| b.writing))
    }

    fn take(&mut self, slot: usize) -> Result<()> {
        self.log(Call::Take(slot));
        Ok(())
    }

    fn copy_out(&mut self, slot: usize, offset: u64, out: &mut [u8]) -> Result<()> {
        self.0.with(|b| {
            if b.fail_copy {
                return Err(Status::IO);
            }
            let start = offset as usize;
            let from = b.memory[slot]
                .get(start..start + out.len())
                .ok_or(Status::INVALID_ARGUMENT)?;
            out.copy_from_slice(from);
            Ok(())
        })
    }

    fn stop(&mut self) -> Result<()> {
        self.0.with(|b| {
            b.writing = None;
            b.calls.push(Call::Stop);
        });
        Ok(())
    }

    fn next_event(&mut self) -> Result<Option<MiEvent>> {
        self.0.with(|b| match b.fail_event.take() {
            Some(status) => Err(status),
            None => Ok(b.events.pop_front()),
        })
    }
}

/// 64x64 RAW10 over 2 lanes at 800 Mbit/s each: small, so a frame's bytes can be checked.
fn raw10() -> LinkConfig {
    LinkConfig {
        lanes: 2,
        virtual_channel: 0,
        data_type: 0x2b,
        bit_depth: 10,
        continuous_clock: false,
        width: 64,
        height: 64,
        pixel_rate_hz: 80_000_000,
        link_rate_mbps: 800,
    }
}

/// 64x64 at 16 bits.
const FRAME: usize = 64 * 2 * 64;

fn driver_with(stream: StreamHandle, format: Format) -> (K230Capture, Handle) {
    let handle = Handle::default();
    let options = Options {
        format,
        order: BayerOrder::Rggb,
    };
    let d = K230Capture::with_hardware(Box::new(Hw(handle.clone())), stream, options);
    (d, handle)
}

fn driver(stream: &MockStream) -> (K230Capture, Handle) {
    driver_with(
        lemnos_core::Held::boxed(Box::new(stream.clone())),
        Format::Bayer,
    )
}

fn streaming(stream: &MockStream) -> (K230Capture, Handle) {
    let (mut d, block) = driver(stream);
    d.configure(raw10(), Packing::Unpacked16).unwrap();
    d.start().unwrap();
    (d, block)
}

/// A frame's bytes: every byte different, and different per frame.
fn pattern(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| {
            (i as u8)
                .wrapping_mul(7)
                .wrapping_add(seed)
                .wrapping_add((i >> 8) as u8)
        })
        .collect()
}

#[test]
fn limits_and_the_streams_name() {
    let (mut d, _block) = driver(&MockStream::new(1));
    let limits = d.limits().unwrap();
    assert_eq!(limits.max_lanes, 4);
    assert_eq!(limits.max_link_rate_mbps, 1600);
    assert_eq!((limits.max_width, limits.max_height), (4096, 4096));
    let mut out = [0u8; MAX_STREAM_NAME];
    let n = d.frame_stream(&mut out).unwrap();
    assert_eq!(&out[..n], b"frames");
    assert_eq!(
        d.frame_stream(&mut [0u8; 2]).unwrap_err(),
        Status::BUFFER_TOO_SMALL
    );
}

#[test]
fn configure_returns_each_formats_layout() {
    // (format, link depth, packing, bit depth, stride, size) at 64x64.
    let cases = [
        (Format::Bayer, 8, Packing::Packed, 8, 64, 4096),
        (Format::Bayer, 10, Packing::Unpacked16, 10, 128, 8192),
        (Format::Bayer, 12, Packing::Unpacked16, 12, 128, 8192),
        // Y, then Cb and Cr at half height.
        (Format::Nv12, 10, Packing::Packed, 8, 64, 4096 + 2048),
        // Y, then Cb and Cr at full height.
        (Format::Nv16, 10, Packing::Packed, 8, 64, 4096 + 4096),
        (Format::Yuyv, 10, Packing::Packed, 8, 128, 8192),
        (Format::Bgr24, 10, Packing::Packed, 8, 192, 12288),
        (Format::P010, 10, Packing::Unpacked16, 10, 128, 8192 + 4096),
    ];
    for (format, depth, packing, bit_depth, stride, size) in cases {
        let (mut d, _block) = driver_with(
            lemnos_core::Held::boxed(Box::new(MockStream::new(1))),
            format,
        );
        let link = LinkConfig {
            bit_depth: depth,
            data_type: 0x2a + (depth - 8) / 2,
            ..raw10()
        };
        assert_eq!(
            d.configure(link, packing).unwrap(),
            FrameLayout {
                width: 64,
                height: 64,
                bit_depth,
                packing,
                stride,
                size,
            },
            "{}",
            format.name()
        );
        assert_eq!(d.link().unwrap(), Some(link));
    }
}

#[test]
fn configure_refuses_what_the_block_cannot_take() {
    let (mut d, _block) = driver(&MockStream::new(1));
    let bad = [
        LinkConfig {
            data_type: 0x2a,
            ..raw10()
        },
        LinkConfig {
            bit_depth: 14,
            data_type: 0x2d,
            ..raw10()
        },
        LinkConfig {
            lanes: 3,
            ..raw10()
        },
        LinkConfig {
            lanes: 0,
            ..raw10()
        },
        LinkConfig {
            virtual_channel: 4,
            ..raw10()
        },
        LinkConfig {
            link_rate_mbps: 79,
            ..raw10()
        },
        LinkConfig {
            link_rate_mbps: 1601,
            ..raw10()
        },
        LinkConfig {
            width: 72,
            ..raw10()
        },
        LinkConfig {
            width: 48,
            ..raw10()
        },
        LinkConfig {
            width: 4112,
            ..raw10()
        },
        LinkConfig {
            height: 65,
            ..raw10()
        },
        LinkConfig {
            height: 4098,
            ..raw10()
        },
    ];
    for link in bad {
        assert_eq!(
            d.configure(link, Packing::Unpacked16).unwrap_err(),
            Status::INVALID_ARGUMENT,
            "{link:?}"
        );
    }
    assert_eq!(
        d.configure(raw10(), Packing::Packed).unwrap_err(),
        Status::INVALID_ARGUMENT,
        "10-bit raw is unpacked"
    );
    assert_eq!(d.link().unwrap(), None, "nothing kept from a refusal");
}

/// The writes between the link and the memory interface for a 64x64 RAW10 frame from an RGGB
/// sensor, by hand from the register map.
const RAW10_SETUP: [(u32, u32); 26] = [
    (0x18, 0x5),     // vi_dpcl: main path to memory, main path only
    (0x404, 0x1000), // isp_acq_prop: RGGB, 10-bit input
    (0x408, 0),      // acquisition window
    (0x40C, 0),
    (0x410, 64),
    (0x414, 64),
    (0x418, 0), // frames until stopped
    (0x594, 0), // output window
    (0x598, 0),
    (0x59C, 64),
    (0x5A0, 64),
    (0x2300, 0), // image stabilisation off, its window the frame
    (0x2308, 0),
    (0x230C, 0),
    (0x2310, 64),
    (0x2314, 64),
    (0x5A4, 0x400),     // demosaic bypassed
    (0xC00, 0),         // no scaling
    (0xC6C, 0),         // no conversion
    (0x1314, 0x840),    // raw 10 bits, 16-bit aligned
    (0x1318, 0x100000), // raw byte swap
    (0x131C, 0x2000100),
    (0x13A0, 128), // raw line length, bytes
    (0x13A4, 64),
    (0x13A8, 64),
    (0x13AC, 8192),
];

#[test]
fn start_programs_the_path_in_order() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, block) = driver(&stream);
    assert_eq!(
        d.start().unwrap_err(),
        Status::INVALID_ARGUMENT,
        "configure first"
    );
    d.configure(raw10(), Packing::Unpacked16).unwrap();
    d.start().unwrap();

    let mut expected = vec![
        // Whatever ran is stopped, the devices opened, and stopped again through them.
        Call::Write(0x400, 0x400),
        Call::Stop,
        Call::Open,
        Call::Write(0x400, 0x400),
        Call::Stop,
        Call::SetLink,
        Call::Provide(8192),
    ];
    expected.extend(RAW10_SETUP.iter().map(|&(a, v)| Call::Write(a, v)));
    expected.push(Call::Start(
        Output::Raw,
        Planes::new(
            Plane {
                offset: 0,
                size: 8192,
            },
            None,
            None,
        )
        .unwrap(),
    ));
    // Enabled in raw mode with the input formatter on, then its registers updated.
    expected.push(Call::Write(0x400, 0x11));
    expected.push(Call::Write(0x400, 0x611));
    assert_eq!(block.calls(), expected);

    let attr = block.with(|b| b.link).unwrap();
    assert_eq!(attr, DevAttr::csi(2, 0, 10, 800, 64, 64).unwrap());
    assert_eq!(attr.mipi.phy_freq, 800);
    assert_eq!(attr.mipi.lane_id[..3], [0, 1, -1]);
    assert_eq!(attr.mipi.interfaces[0].data_type, 1, "RAW10");
    assert_eq!((attr.image_rect.width, attr.image_rect.height), (64, 64));
}

#[test]
fn a_yuv_or_rgb_format_goes_through_the_pipeline() {
    for (format, conv, fmt, planes) in [
        (
            Format::Nv12,
            0,
            0x1000,
            Planes::new(
                Plane {
                    offset: 0,
                    size: 4096,
                },
                Some(Plane {
                    offset: 4096,
                    size: 2048,
                }),
                None,
            )
            .unwrap(),
        ),
        (
            Format::Bgr24,
            0x1A,
            0x18,
            Planes::new(
                Plane {
                    offset: 0,
                    size: 12288,
                },
                None,
                None,
            )
            .unwrap(),
        ),
    ] {
        let (mut d, block) = driver_with(
            lemnos_core::Held::boxed(Box::new(MockStream::with_bytes(2, 12288))),
            format,
        );
        d.configure(raw10(), Packing::Packed).unwrap();
        d.start().unwrap();
        let calls = block.calls();
        let writes: Vec<(u32, u32)> = calls
            .iter()
            .filter_map(|c| match *c {
                Call::Write(a, v) => Some((a, v)),
                _ => None,
            })
            .collect();
        let at = |addr: u32| writes.iter().rev().find(|w| w.0 == addr).unwrap().1;
        assert_eq!(at(0x5A4), 0, "demosaic on");
        assert_eq!(at(0xC6C), conv);
        assert_eq!(at(0x1314), fmt);
        assert_eq!(at(0x1318), 0, "no raw swap");
        // The Y plane's geometry in pixels, and no raw geometry.
        assert_eq!(at(0x1330), 64);
        assert_eq!(at(0x1334), 64);
        assert_eq!(at(0x1338), 64);
        assert_eq!(at(0x133C), 4096);
        assert!(writes.iter().all(|w| !(0x13A0..=0x13AC).contains(&w.0)));
        // RGB-mode ISP.
        assert_eq!(&writes[writes.len() - 2..], [(0x400, 0x17), (0x400, 0x617)]);
        assert!(calls.contains(&Call::Start(Output::YCbCr, planes)));
    }
}

#[test]
fn every_setup_write_is_one_the_sys_table_allows() {
    use lemnos_driver_k230_capture_sys::access;
    for format in Format::ALL {
        for depth in [8, 10, 12] {
            let layout = plan::layout(format, depth, 64, 64).unwrap();
            let writes = plan::halt()
                .as_slice()
                .iter()
                .chain(plan::setup(&layout, 3, depth).as_slice())
                .chain(plan::run(&layout).as_slice())
                .copied()
                .collect::<Vec<_>>();
            for (addr, value) in writes {
                assert!(
                    access::allowed(addr, value),
                    "{} at {depth}: {addr:#x} = {value:#x}",
                    format.name()
                );
            }
        }
    }
}

#[test]
fn frames_rotate_through_the_ring_and_carry_the_drivers_own_numbers() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, block) = streaming(&stream);
    let mut now = d.tick(0).unwrap();
    let sent: Vec<Vec<u8>> = (0..9u8).map(|seed| pattern(seed, FRAME)).collect();
    for (i, bytes) in sent.iter().enumerate() {
        block.frame(bytes, 1_000 + i as u64);
        now = d.tick(now).unwrap();
        let frame = stream.take_frame().expect("published");
        assert_eq!(frame.bytes as usize, FRAME);
        assert_eq!(frame.sequence, i as u64 + 1);
        assert_eq!(frame.timestamp_ns, 1_000 + i as u64, "the interrupt's time");
        assert_eq!(frame.identity, 0);
        assert!(stream.contents(frame.buffer) == *bytes, "frame {i}");
        stream.return_buffer(frame.buffer);
    }
    // Memory 1 was armed at start; each frame end arms the memory left free.
    assert_eq!(block.rearms(), [2, 0, 1, 2, 0, 1, 2, 0, 1]);
    // The free memory is armed before the finished one is taken back to be read.
    let calls = block.calls();
    let first_rearm = calls.iter().position(|c| *c == Call::Rearm(2)).unwrap();
    let first_take = calls.iter().position(|c| *c == Call::Take(0)).unwrap();
    assert!(first_rearm < first_take);
    assert_eq!(
        d.counters().unwrap(),
        Counters {
            frames: 9,
            dropped: 0,
            errors: 0
        }
    );
}

#[test]
fn a_frame_larger_than_the_copy_chunk_arrives_whole() {
    // 256x192 BGR: 147456 bytes, more than two chunks.
    let link = LinkConfig {
        width: 256,
        height: 192,
        ..raw10()
    };
    let bytes = 256 * 3 * 192;
    assert!(bytes > 2 * COPY_CHUNK);
    let stream = MockStream::with_bytes(2, bytes);
    let (mut d, block) = driver_with(
        lemnos_core::Held::boxed(Box::new(stream.clone())),
        Format::Bgr24,
    );
    d.configure(link, Packing::Packed).unwrap();
    d.start().unwrap();
    let sent = pattern(5, bytes);
    block.frame(&sent, 7);
    d.tick(0).unwrap();
    let frame = stream.take_frame().unwrap();
    assert_eq!(frame.bytes as usize, bytes);
    assert!(stream.contents(frame.buffer) == sent);
}

#[test]
fn other_interrupts_are_not_frames() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, block) = streaming(&stream);
    // A self-path or bus-error status: no frame end of the main path.
    block.with(|b| {
        b.events.push_back(MiEvent {
            status: 0x40,
            timestamp_ns: 1,
        });
        b.events.push_back(MiEvent {
            status: 0x8000_0000,
            timestamp_ns: 2,
        });
    });
    d.tick(0).unwrap();
    assert!(stream.take_frame().is_none());
    assert!(block.rearms().is_empty());
    assert_eq!(d.counters().unwrap(), Counters::default());
    // The next real frame end is the first frame.
    block.frame(&pattern(1, FRAME), 3);
    d.tick(0).unwrap();
    assert_eq!(stream.take_frame().unwrap().sequence, 1);
}

#[test]
fn a_frame_end_without_a_move_or_onto_unarmed_memory_delivers_nothing() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, block) = streaming(&stream);
    // The block has not moved: nothing new finished.
    block.with(|b| {
        b.events.push_back(MiEvent {
            status: 0x1,
            timestamp_ns: 1,
        })
    });
    d.tick(0).unwrap();
    // The block names the free memory, which was never armed: the ring is lost.
    block.with(|b| {
        b.writing = Some(2);
        b.events.push_back(MiEvent {
            status: 0x1,
            timestamp_ns: 2,
        });
    });
    d.tick(0).unwrap();
    // Or none at all.
    block.with(|b| {
        b.writing = None;
        b.events.push_back(MiEvent {
            status: 0x1,
            timestamp_ns: 3,
        });
    });
    d.tick(0).unwrap();
    assert!(stream.take_frame().is_none());
    assert!(block.rearms().is_empty(), "nothing armed on a guess");
    assert_eq!(
        d.counters().unwrap(),
        Counters {
            frames: 0,
            dropped: 1,
            errors: 2
        }
    );
    assert_eq!(d.health(), Health::Ok, "not a reason to restart");
}

#[test]
fn a_stalled_consumer_drops_frames_and_the_sequence_shows_it() {
    let stream = MockStream::with_bytes(1, FRAME);
    let (mut d, block) = streaming(&stream);
    for i in 0..3u8 {
        block.frame(&pattern(i, FRAME), u64::from(i));
        d.tick(0).unwrap();
    }
    // One stream buffer, never given back: the first frame went out, the next two were dropped,
    // and the ring kept turning.
    assert_eq!(
        d.counters().unwrap(),
        Counters {
            frames: 1,
            dropped: 2,
            errors: 0
        }
    );
    assert_eq!(block.rearms(), [2, 0, 1]);
    let first = stream.take_frame().unwrap();
    stream.return_buffer(first.buffer);
    block.frame(&pattern(3, FRAME), 3);
    d.tick(0).unwrap();
    assert_eq!(stream.take_frame().unwrap().sequence, 4, "a gap of two");
}

#[test]
fn a_frame_that_cannot_be_copied_is_counted_and_its_buffer_given_back_empty() {
    let stream = MockStream::with_bytes(1, FRAME);
    let (mut d, block) = streaming(&stream);
    block.with(|b| b.fail_copy = true);
    block.frame(&pattern(0, FRAME), 0);
    d.tick(0).unwrap();
    let given_back = stream.take_frame().unwrap();
    assert_eq!(given_back.bytes, 0);
    stream.return_buffer(given_back.buffer);
    assert_eq!(d.counters().unwrap().errors, 1);
    assert_eq!(d.health(), Health::Ok);
    // The only stream buffer carries the next one.
    block.with(|b| b.fail_copy = false);
    block.frame(&pattern(1, FRAME), 1);
    d.tick(0).unwrap();
    let frame = stream.take_frame().unwrap();
    assert_eq!(stream.contents(frame.buffer), pattern(1, FRAME));
}

#[test]
fn a_stream_buffer_too_small_for_the_frame_is_an_error_not_a_write_past_it() {
    let stream = MockStream::with_bytes(1, FRAME - 1);
    let (mut d, block) = streaming(&stream);
    block.frame(&pattern(0, FRAME), 0);
    d.tick(0).unwrap();
    assert_eq!(d.counters().unwrap().errors, 1);
    assert_eq!(stream.take_frame().unwrap().bytes, 0);
}

#[test]
fn a_device_error_is_retried_with_capped_backoff() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, block) = streaming(&stream);
    assert_eq!(d.health(), Health::Ok);
    let mut now = d.tick(0).unwrap();
    assert_eq!(now, 1_000_000, "polled every millisecond");
    block.with(|b| b.fail_event = Some(Status::IO));
    let at = d.tick(now).unwrap();
    assert_eq!(at, now + 250_000_000);
    assert!(matches!(d.health(), Health::Degraded(_)));
    assert_eq!(d.start().unwrap_err(), Status::BUSY, "while recovering");
    assert_eq!(
        d.configure(raw10(), Packing::Unpacked16).unwrap_err(),
        Status::BUSY
    );

    block.with(|b| b.fail_open = Some(Status::NO_DEVICE));
    now = at;
    for delay in [
        500_000_000u64,
        1_000_000_000,
        2_000_000_000,
        4_000_000_000,
        8_000_000_000,
        8_000_000_000,
    ] {
        let at = d.tick(now).unwrap();
        assert_eq!(at - now, delay, "backoff capped at 8 s");
        now = at;
    }
    // Forever: it never gives up and never turns itself off.
    for _ in 0..100 {
        let at = d.tick(now).unwrap();
        assert_eq!(at - now, 8_000_000_000);
        now = at;
        assert!(matches!(d.health(), Health::Degraded(_)));
    }

    // It clears when an attempt works: opened, set up and started again, frames flowing.
    block.with(|b| {
        b.fail_open = None;
        b.calls.clear();
    });
    let at = d.tick(now).unwrap();
    assert_eq!(at, now + 1_000_000);
    assert_eq!(d.health(), Health::Ok);
    let calls = block.calls();
    assert_eq!(calls.iter().filter(|c| **c == Call::Open).count(), 1);
    assert!(matches!(
        calls[calls.len() - 3],
        Call::Start(Output::Raw, _)
    ));
    block.frame(&pattern(9, FRAME), 9);
    d.tick(at).unwrap();
    let frame = stream.take_frame().unwrap();
    assert_eq!(frame.sequence, 1, "counted again from the restart");
    assert_eq!(stream.contents(frame.buffer), pattern(9, FRAME));
}

#[test]
fn stop_ends_streaming_and_recovery() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, block) = streaming(&stream);
    block.with(|b| {
        b.fail_event = Some(Status::IO);
        b.calls.clear();
    });
    let _ = d.tick(0);
    assert!(matches!(d.health(), Health::Degraded(_)));
    d.stop().unwrap();
    assert_eq!(block.calls(), [Call::Write(0x400, 0x400), Call::Stop]);
    assert_eq!(d.tick(u64::MAX), None, "stopped: nothing is due");
    assert_eq!(d.health(), Health::Ok);
    // And it can start again.
    d.configure(raw10(), Packing::Unpacked16).unwrap();
    d.start().unwrap();
}

#[test]
fn a_failed_start_is_reported_and_not_retried_in_the_background() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, block) = driver(&stream);
    d.configure(raw10(), Packing::Unpacked16).unwrap();
    block.with(|b| b.fail_open = Some(Status::NO_DEVICE));
    assert_eq!(d.start().unwrap_err(), Status::NO_DEVICE);
    assert_eq!(d.tick(0), None);
    assert_eq!(d.health(), Health::Ok);
    block.with(|b| b.fail_open = None);
    d.start().unwrap();
    assert_eq!(d.start().unwrap_err(), Status::BUSY, "not twice");
}

#[test]
fn counters_start_again_at_each_start() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, block) = streaming(&stream);
    block.frame(&pattern(0, FRAME), 0);
    d.tick(0).unwrap();
    assert_eq!(d.counters().unwrap().frames, 1);
    d.stop().unwrap();
    d.start().unwrap();
    assert_eq!(d.counters().unwrap(), Counters::default());
}

#[test]
fn settings_and_spellings() {
    assert_eq!(backoff_ns(0), 250_000_000);
    assert_eq!(backoff_ns(5), 8_000_000_000);
    assert_eq!(backoff_ns(u32::MAX), 8_000_000_000);
    assert_eq!(parse_order("bggr"), Some(BayerOrder::Bggr));
    assert_eq!(parse_order("BGGR"), None);
    assert_eq!(Format::parse("p010"), Some(Format::P010));
    assert_eq!(Format::parse("nv21"), None);
    let defaults = Settings::new(None, None).unwrap();
    assert_eq!(
        (defaults.isp(), defaults.mipi()),
        ("/dev/vvcam-isp.0", "/dev/vvcam-mipi.0")
    );
    assert_eq!(
        Settings::new(Some(""), None).unwrap_err(),
        Status::INVALID_ARGUMENT
    );
}

#[test]
fn the_steady_state_allocates_nothing() {
    let stream = MockStream::with_bytes(4, FRAME);
    let (mut d, block) = streaming(&stream);
    let frames: Vec<Vec<u8>> = (0..4u8).map(|seed| pattern(seed, FRAME)).collect();
    let mut now = d.tick(0).unwrap();
    // One round first, so the mocked stream's queues have grown.
    for bytes in &frames {
        block.frame(bytes, 0);
        now = d.tick(now).unwrap();
        let frame = stream.take_frame().unwrap();
        stream.return_buffer(frame.buffer);
    }
    block.with(|b| {
        b.calls.reserve(4000);
        b.events.reserve(16);
    });
    let ((), allocated) = allocations(|| {
        for i in 0..1000u64 {
            block.frame(&frames[i as usize % 4], i);
            now = d.tick(now).unwrap();
            let frame = stream.take_frame().unwrap();
            stream.return_buffer(frame.buffer);
        }
    });
    assert_eq!(allocated, 0, "event, rearm, copy, publish and the poll");
    assert_eq!(d.counters().unwrap().frames, 1004);
}

#[test]
fn the_block_describes_no_sensor_so_the_link_is_unknown() {
    let (mut d, _hw) = driver(&MockStream::new(1));
    let mut name = [0u8; 64];
    assert_eq!(
        CameraReceiver::source(&mut d, &mut name),
        Err(Status::NOT_SUPPORTED)
    );
}

#[test]
fn layout_is_what_configure_set_and_nothing_before() {
    let (mut d, _hw) = driver(&MockStream::new(1));
    assert_eq!(CameraReceiver::layout(&mut d), Ok(None));
    let set = d.configure(raw10(), Packing::Unpacked16).unwrap();
    assert_eq!(CameraReceiver::layout(&mut d), Ok(Some(set)));
}
