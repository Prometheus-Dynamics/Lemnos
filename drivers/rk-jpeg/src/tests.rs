//! Placements against every check, read-back decoding, and the session against a played device.
//! No device node is opened.

use std::cell::{Cell, RefCell};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use lemnos_linux_sys::dma_buf;
use lemnos_measure::test_alloc::allocations;

use crate::device::find_at;
use crate::mpp::{CMD_SET_REG_READ, FLAG_REG_NO_OFFSET, OffsetElem};
use crate::regs::{Variant, dec, enc};
use crate::status::{self, Outcome};
use crate::{
    Addresses, At, Buffer, Device, Jpeg, PlaceError, SessionError, State, backoff_ns, read_window,
};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

fn memfd() -> OwnedFd {
    dma_buf::memfd(c"rk-jpeg-test").expect("memfd")
}

fn buf(fd: BorrowedFd<'_>, len: u64) -> Buffer<'_> {
    Buffer::with_len(fd, len).expect("buffer")
}

fn raw(fd: &OwnedFd) -> u32 {
    u32::try_from(fd.as_raw_fd()).expect("descriptor")
}

#[test]
fn placements_fill_the_address_registers_and_nothing_else() {
    let a = memfd();
    let b = memfd();
    let mut addresses = Addresses::new(Variant::EncoderV1);
    addresses
        .place(enc::SRC0, At::new(buf(a.as_fd(), 4096), 0), 4096)
        .expect("source");
    addresses
        .place(enc::BS_START, At::new(buf(b.as_fd(), 8192), 100), 8092)
        .expect("bitstream");
    assert_eq!(
        addresses.words(),
        [(enc::SRC0, raw(&a)), (enc::BS_START, raw(&b))]
    );
    assert_eq!(
        addresses.offsets(),
        [
            OffsetElem {
                index: enc::SRC0,
                offset: 0
            },
            OffsetElem {
                index: enc::BS_START,
                offset: 100
            }
        ]
    );
    // A caller's numbers in the other address registers are cleared; the rest are untouched.
    let mut file = [0xdead_beef; enc::REG_NUM_V1 as usize];
    assert!(addresses.fill(&mut file));
    for (i, &value) in file.iter().enumerate() {
        let i = u32::try_from(i).expect("index");
        let want = match i {
            enc::SRC0 => raw(&a),
            enc::BS_START => raw(&b),
            _ if enc::ADDRESS_REGISTERS.contains(&i) => 0,
            _ => 0xdead_beef,
        };
        assert_eq!(value, want, "register {i}");
    }
    // A file of the wrong size is left alone.
    let mut short = [7; enc::REG_NUM_V1 as usize - 1];
    assert!(!addresses.fill(&mut short));
    assert!(short.iter().all(|&v| v == 7));
    let mut v2 = [7; enc::REG_NUM_V2 as usize];
    assert!(!addresses.fill(&mut v2));
}

#[test]
fn an_extent_that_fills_its_buffer_exactly_is_taken_one_more_byte_is_not() {
    let fd = memfd();
    let b = buf(fd.as_fd(), 10_000);
    let mut addresses = Addresses::new(Variant::DecoderV2);
    addresses
        .place(12, At::new(b, 1_000), 9_000)
        .expect("exact fit");
    assert_eq!(
        addresses.place(13, At::new(b, 1_000), 9_001),
        Err(PlaceError::OutOfBuffer {
            register: 13,
            end: 10_001,
            len: 10_000
        })
    );
    assert_eq!(
        addresses.place(13, At::new(b, 10_000), 1),
        Err(PlaceError::OutOfBuffer {
            register: 13,
            end: 10_001,
            len: 10_000
        })
    );
    assert_eq!(addresses.words().len(), 1);
}

#[test]
fn offsets_that_overflow_or_pass_u32_are_refused() {
    let fd = memfd();
    let b = buf(fd.as_fd(), u64::MAX);
    let mut addresses = Addresses::new(Variant::DecoderV1);
    assert_eq!(
        addresses.place(9, At::new(b, u64::MAX - 1), 2),
        Err(PlaceError::OutOfBuffer {
            register: 9,
            end: u64::MAX,
            len: u64::MAX
        })
    );
    assert!(matches!(
        addresses.place(9, At::new(b, u64::from(u32::MAX) + 1), 1),
        Err(PlaceError::OutOfBuffer { register: 9, .. })
    ));
    addresses
        .place(9, At::new(b, u64::from(u32::MAX)), 1)
        .expect("last offset");
    assert_eq!(addresses.offsets()[0].offset, u32::MAX);
}

#[test]
fn only_address_registers_once_each_and_never_the_link_table() {
    let fd = memfd();
    let at = At::new(buf(fd.as_fd(), 4096), 0);
    let mut d = Addresses::new(Variant::DecoderV1);
    for register in [0, 1, 3, 8, 14, 41, 69, 1 << 30] {
        assert_eq!(
            d.place(register, at, 1),
            Err(PlaceError::NotAnAddress(register))
        );
    }
    d.place(10, at, 1).expect("address");
    assert_eq!(d.place(10, at, 1), Err(PlaceError::Twice(10)));
    assert_eq!(d.place(11, at, 0), Err(PlaceError::Empty(11)));
    let mut e = Addresses::new(Variant::EncoderV2);
    assert_eq!(e.place(enc::LINK_TABLE, at, 1), Err(PlaceError::LinkTable));
    assert_eq!(e.place(25, at, 1), Err(PlaceError::NotAnAddress(25)));
    for register in &enc::ADDRESS_REGISTERS[1..] {
        e.place(*register, at, 1).expect("address");
    }
    assert_eq!(e.words().len(), 9);
}

#[test]
fn descriptor_zero_is_refused_and_buffers_are_measured() {
    let stdin = io::stdin();
    let error = Buffer::with_len(stdin.as_fd(), 1 << 30).expect_err("descriptor 0");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    let fd = dma_buf::sealed_memfd(c"rk-jpeg-measured", 8_192).expect("memfd");
    assert_eq!(Buffer::new(fd.as_fd()).expect("buffer").len(), 8_192);
}

#[test]
fn reads_stay_inside_the_register_file() {
    let whole = read_window(Variant::DecoderV1, 0, dec::REG_NUM_V1).expect("whole file");
    assert_eq!(
        (whole.cmd, whole.offset, whole.size, whole.data_ptr),
        (CMD_SET_REG_READ, 0, 42 * 4, 0)
    );
    assert_ne!(whole.flags & FLAG_REG_NO_OFFSET, 0);
    let last = read_window(Variant::DecoderV2, dec::V2_INT_STA_INDEX, 1).expect("status");
    assert_eq!((last.offset, last.size), (0x114, 4));
    assert_eq!(read_window(Variant::DecoderV1, 41, 2), None);
    assert_eq!(read_window(Variant::DecoderV1, 0, 0), None);
    assert_eq!(read_window(Variant::EncoderV1, u32::MAX, 2), None);
    assert!(read_window(Variant::EncoderV2, 0, 241).is_some());
    assert_eq!(read_window(Variant::EncoderV2, 0, 242), None);
}

#[test]
fn read_back_status_decodes() {
    assert_eq!(
        status::decoder_v1(dec::READY_STA | dec::IRQ_RAW),
        Outcome::Done
    );
    assert_eq!(
        status::decoder_v1(dec::READY_STA | dec::BUS_STA),
        Outcome::Error(dec::BUS_STA)
    );
    assert_eq!(status::decoder_v1(dec::IRQ), Outcome::Unfinished(dec::IRQ));
    assert_eq!(status::decoder_v2(1), Outcome::Done);
    assert_eq!(status::decoder_v2(1 | 0x8), Outcome::Error(0x8));
    assert_eq!(status::decoder_v2(2), Outcome::Unfinished(2));
    assert_eq!(status::encoder(1), Outcome::Done);
    assert_eq!(status::encoder(1 | 0x40), Outcome::Error(0x40));
    assert_eq!(status::encoder(0x80), Outcome::Unfinished(0x80));
    assert_eq!(status::decoded_bytes(12_345 << 10), 12_345);
    assert_eq!(status::decoded_bytes((-5i32 << 10).cast_unsigned()), -5);
    assert_eq!(status::decoder_picture(1079 << 16 | 1919), (1920, 1080));
    assert_eq!(status::encoder_picture(134 << 16 | 239), (1920, 1080));
    assert_eq!(
        status::encoder_resolution(1920, 1080),
        Some(134 << 16 | 239)
    );
    assert_eq!(status::encoder_resolution(1920, 1081), None);
    assert_eq!(status::encoder_resolution(0, 8), None);
    assert_eq!(status::encoder_resolution(65_536, 8), Some(0x1fff));
    assert_eq!(status::encoder_resolution(65_544, 8), None);
    for (w, h) in [(8, 8), (640, 480), (65_536, 65_536)] {
        let rsl = status::encoder_resolution(w, h).expect("size");
        assert_eq!(status::encoder_picture(rsl), (w, h));
    }
}

#[test]
fn placing_allocates_nothing() {
    let fd = memfd();
    let b = buf(fd.as_fd(), 1 << 20);
    let ((), allocated) = allocations(|| {
        for _ in 0..1_000 {
            let mut addresses = Addresses::new(Variant::EncoderV1);
            for (i, register) in enc::ADDRESS_REGISTERS[1..].iter().enumerate() {
                let offset = u64::try_from(i).expect("index") * 4096;
                addresses
                    .place(*register, At::new(b, offset), 4096)
                    .expect("place");
            }
            let mut file = [0; enc::REG_NUM_V1 as usize];
            assert!(addresses.fill(&mut file));
        }
    });
    assert_eq!(allocated, 0);
}

/// What the played device saw, and what it is to fail next.
#[derive(Default)]
struct Log {
    opens: Cell<u32>,
    clients: RefCell<Vec<u32>>,
    queries: Cell<u32>,
    releases: Cell<u32>,
    fail_open: Cell<Option<i32>>,
    fail_init: Cell<Option<i32>>,
    fail_query: Cell<Option<i32>>,
    fail_release: Cell<Option<i32>>,
}

struct Played(Rc<Log>);

fn failed(errno: &Cell<Option<i32>>) -> io::Result<()> {
    errno
        .take()
        .map_or(Ok(()), |errno| Err(io::Error::from_raw_os_error(errno)))
}

impl Device for Played {
    fn init_client(&mut self, device_type: u32) -> io::Result<()> {
        failed(&self.0.fail_init)?;
        self.0.clients.borrow_mut().push(device_type);
        Ok(())
    }

    fn hw_id(&mut self) -> io::Result<u32> {
        failed(&self.0.fail_query)?;
        self.0.queries.set(self.0.queries.get() + 1);
        Ok(dec::HWID_VPU720)
    }

    fn release_buffers(&mut self) -> io::Result<()> {
        failed(&self.0.fail_release)?;
        self.0.releases.set(self.0.releases.get() + 1);
        Ok(())
    }
}

type PlayedJpeg = Jpeg<Played, Box<dyn FnMut() -> io::Result<Played>>>;

fn jpeg(variant: Variant) -> (PlayedJpeg, Rc<Log>) {
    let log = Rc::new(Log::default());
    let held = Rc::clone(&log);
    let open: Box<dyn FnMut() -> io::Result<Played>> = Box::new(move || {
        failed(&held.fail_open)?;
        held.opens.set(held.opens.get() + 1);
        Ok(Played(Rc::clone(&held)))
    });
    (Jpeg::new(variant, open), log)
}

#[test]
fn a_session_opens_when_first_used_and_binds_its_block() {
    for (variant, device_type) in [(Variant::DecoderV1, 13), (Variant::EncoderV2, 20)] {
        let (mut session, log) = jpeg(variant);
        assert_eq!(session.state(), State::Closed);
        assert!(session.release_buffers(0).is_ok());
        assert_eq!(log.opens.get(), 0);
        assert_eq!(session.hw_id(1).expect("hw id"), dec::HWID_VPU720);
        assert_eq!(session.hw_id(2).expect("hw id"), dec::HWID_VPU720);
        assert_eq!(session.state(), State::Open);
        assert_eq!(log.opens.get(), 1);
        assert_eq!(*log.clients.borrow(), [device_type]);
        session.release_buffers(3).expect("release");
        assert_eq!(log.releases.get(), 1);
        session.stop();
        assert_eq!(session.state(), State::Closed);
    }
}

#[test]
fn failures_back_off_doubling_to_eight_seconds_and_never_give_up() {
    let (mut session, log) = jpeg(Variant::DecoderV2);
    let mut now = 1_000;
    for attempt in 0..10u32 {
        match attempt % 3 {
            0 => log.fail_open.set(Some(libc::ENOENT)),
            1 => log.fail_init.set(Some(libc::EINVAL)),
            _ => log.fail_query.set(Some(libc::EIO)),
        }
        let Err(SessionError::Device { retry_at_ns, .. }) = session.hw_id(now) else {
            panic!("attempt {attempt} did not fail");
        };
        assert_eq!(retry_at_ns - now, backoff_ns(attempt));
        assert!(retry_at_ns - now <= 8_000_000_000);
        assert!(matches!(
            session.hw_id(retry_at_ns - 1),
            Err(SessionError::BackingOff { .. })
        ));
        assert!(matches!(
            session.state(),
            State::BackingOff { failures, .. } if failures == attempt + 1
        ));
        now = retry_at_ns;
    }
    assert_eq!(backoff_ns(9), 8_000_000_000);
    session.hw_id(now).expect("recovers");
    assert_eq!(session.state(), State::Open);
    log.fail_release.set(Some(libc::EIO));
    let Err(SessionError::Device { retry_at_ns, .. }) = session.release_buffers(now) else {
        panic!("release did not fail");
    };
    assert_eq!(retry_at_ns - now, backoff_ns(0));
}

#[test]
fn the_node_is_found_when_present() {
    assert_eq!(find_at(Path::new("/nonexistent/mpp_service")), None);
    let here = std::env::current_dir().expect("cwd");
    assert_eq!(find_at(&here), Some(PathBuf::from(&here)));
}
