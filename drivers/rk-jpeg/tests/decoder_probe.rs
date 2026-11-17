//! RKJPEG-DEC: the decoder probe's logic against stand-ins, and the board runner's job path
//! against the played kernel. The one board test is `#[ignore]` and runs only on the board
//! (`testing/board-runs/RKJPEG-DEC.md`).

#![cfg(target_os = "linux")]

use std::cell::RefCell;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::rc::Rc;

use lemnos_driver_rk_jpeg::probe::{
    self, CHROMA_FILL, ChromaAt, FIELDS, HMIN_OFF, HVAL_OFF, LUMA_BYTES, LUMA_FILL, MEM_BYTES,
    QTBL_OFF, REG_HMIN, REG_HVAL, REG_OUT, REG_QTBL, REG_STRM, REG_STRM_LEN, Reach, Runner,
    SENTINEL, STREAM_OFF, TableUnit, Targets, expected_joint, file, match_units, probe_chroma,
    probe_formats, probe_joint, probe_reach,
};
use lemnos_driver_rk_jpeg::regs::dec;
use lemnos_driver_rk_jpeg::status::Outcome;
use lemnos_driver_rk_mpp::played::{Kernel, iova};
use lemnos_driver_rk_mpp::{Block, Device as ServiceDevice, Session};
use lemnos_linux_sys::dma_buf;

mod fixture;
use fixture::{COUNTS, DQT_PAYLOAD, SYMBOLS, TABLE_BYTES, TARGETS as FIXTURE_TARGETS};

/// The fixture's table sizes the stand-in tests use. Equal, so one field value can satisfy all
/// three: a joint sweep can then tell the units apart.
const TARGETS: Targets = Targets {
    qtbl: 16,
    mincode: 16,
    value: 16,
};

/// The stand-in block. It completes only in format `format` with each table field making its
/// table's length, and it writes the planes its candidate places. Writes past the dma-buf fail
/// the job (`bounded`); the lenient stand-in clips them and completes instead.
#[derive(Clone, Copy, Debug)]
struct Model {
    chroma: ChromaAt,
    units: [TableUnit; 3],
    format: u32,
    bounded: bool,
}

impl Runner for Model {
    fn run(&mut self, file: &[u32], mem: &mut [u8]) -> io::Result<u32> {
        let fmt = (file[probe::REG_SYS] >> probe::YUV_FMT_SHIFT) & probe::YUV_FMT_MASK;
        let fields_ok = FIELDS
            .iter()
            .zip(self.units)
            .zip([TARGETS.qtbl, TARGETS.mincode, TARGETS.value])
            .all(|((field, unit), want)| {
                unit.consumed((file[probe::REG_TBL] >> field.shift) & field.mask) == want
            });
        if fmt != self.format || !fields_ok {
            return Ok(dec::ERROR_STA);
        }
        let (start, len) = self.chroma.place(LUMA_BYTES);
        let out = file[REG_OUT] as usize;
        if out + start + len > mem.len() && self.bounded {
            return Ok(dec::BUS_STA);
        }
        for byte in mem.iter_mut().skip(out).take(LUMA_BYTES) {
            *byte = LUMA_FILL;
        }
        for byte in mem.iter_mut().skip(out + start).take(len) {
            *byte = CHROMA_FILL;
        }
        Ok(dec::READY_STA)
    }
}

fn model(chroma: ChromaAt, units: [TableUnit; 3]) -> Model {
    Model {
        chroma,
        units,
        format: 1,
        bounded: true,
    }
}

/// The field values that make `units` complete for [`TARGETS`]. A unit that cannot make its
/// target in its field is a test error.
fn encoding(units: [TableUnit; 3]) -> [u32; 3] {
    let want = [TARGETS.qtbl, TARGETS.mincode, TARGETS.value];
    let mut out = [0; 3];
    for (i, field) in FIELDS.iter().enumerate() {
        out[i] = units[i]
            .encode(want[i], *field)
            .unwrap_or_else(|| panic!("{:?} cannot make {} bytes", units[i], want[i]));
    }
    out
}

#[test]
fn the_joint_sweep_names_the_unit_of_every_field() {
    for unit in TableUnit::ALL {
        let units = [unit; 3];
        let mut m = model(ChromaAt::Adjacent, units);
        let accepted = probe_joint(&mut m, 1).expect("run");
        let found = match_units(&accepted, TARGETS);
        assert!(
            found.contains(&units),
            "{unit:?} not among {found:?} for {accepted:?}"
        );
    }
}

#[test]
fn the_joint_sweep_separates_one_field_with_a_different_unit() {
    // Words4 for two fields and Bytes for the value table: no single value makes all three, so
    // the observed set is empty and the matcher must not return the uniform triple.
    let units = [TableUnit::Words4, TableUnit::Words4, TableUnit::Bytes];
    let mut m = model(ChromaAt::Adjacent, units);
    let accepted = probe_joint(&mut m, 1).expect("run");
    assert!(accepted.is_empty(), "{accepted:?}");
    let found = match_units(&accepted, TARGETS);
    assert!(!found.contains(&[TableUnit::Words4; 3]));
    assert!(found.contains(&units));
}

#[test]
fn the_formats_sweep_finds_only_the_code_the_model_takes() {
    let mut m = Model {
        format: 3,
        ..model(ChromaAt::Adjacent, [TableUnit::Words4; 3])
    };
    let rows = probe_formats(&mut m, encoding([TableUnit::Words4; 3])).expect("run");
    let done: Vec<_> = rows
        .iter()
        .filter(|(_, o, _)| *o == Outcome::Done)
        .collect();
    assert_eq!(done.len(), 1, "{rows:?}");
    assert_eq!(done[0].0, 3);
    assert_eq!(done[0].2, LUMA_BYTES + LUMA_BYTES / 2);
}

#[test]
fn the_chroma_plane_is_found_for_each_candidate() {
    let units = [TableUnit::Words4; 3];
    for at in ChromaAt::ALL {
        let mut m = model(at, units);
        let found = probe_chroma(&mut m, 1, encoding(units)).expect("run");
        let (start, len) = at.place(LUMA_BYTES);
        assert_eq!(found, Some((at, start, len)), "{at:?}");
    }
}

#[test]
fn the_reach_is_the_predicted_end_for_each_candidate() {
    let units = [TableUnit::Words4; 3];
    for at in ChromaAt::ALL {
        let mut m = model(at, units);
        let (start, len) = at.place(LUMA_BYTES);
        assert_eq!(
            probe_reach(&mut m, 1, encoding(units)).expect("run"),
            Reach::Bounded(start + len),
            "{at:?}"
        );
    }
}

#[test]
fn a_stand_in_that_clips_its_writes_is_reported_unbounded() {
    let units = [TableUnit::Words4; 3];
    let mut lenient = Model {
        bounded: false,
        ..model(ChromaAt::Adjacent, units)
    };
    assert_eq!(
        probe_reach(&mut lenient, 1, encoding(units)).expect("run"),
        Reach::Unbounded
    );
}

#[test]
fn a_layout_no_candidate_predicts_matches_none() {
    /// Chroma eight bytes past the luma end: no candidate.
    struct Shifted;
    impl Runner for Shifted {
        fn run(&mut self, _file: &[u32], mem: &mut [u8]) -> io::Result<u32> {
            for b in mem.iter_mut().take(LUMA_BYTES) {
                *b = LUMA_FILL;
            }
            for b in mem.iter_mut().skip(LUMA_BYTES + 8).take(LUMA_BYTES / 2) {
                *b = CHROMA_FILL;
            }
            Ok(dec::READY_STA)
        }
    }
    let units = [TableUnit::Words4; 3];
    assert_eq!(
        probe_chroma(&mut Shifted, 1, encoding(units)).expect("run"),
        None
    );
}

#[test]
fn the_stand_in_fails_where_the_block_fails() {
    let units = [TableUnit::Words4; 3];
    let mut m = model(ChromaAt::Adjacent, units);
    // Writes past a too-small output buffer set the bus status.
    let mut small = vec![SENTINEL; 100];
    let status = m
        .run(&file(0, 1, encoding(units)), &mut small)
        .expect("run");
    assert_eq!(status, dec::BUS_STA);
    // A table field that does not make its table's length is an error.
    let mut big = vec![SENTINEL; MEM_BYTES];
    let status = m.run(&file(0, 1, [3, 4, 4]), &mut big).expect("run");
    assert_eq!(status, dec::ERROR_STA);
    // A format the model does not take is an error.
    let status = m.run(&file(0, 5, encoding(units)), &mut big).expect("run");
    assert_eq!(status, dec::ERROR_STA);
}

#[test]
fn an_observed_empty_set_is_ambiguous_and_is_not_a_unique_answer() {
    // Several unit triples predict an empty set for these targets; the matcher returns them all,
    // so the run reports ambiguity instead of picking one.
    let found = match_units(&[], TARGETS);
    assert!(found.len() > 1, "{found:?}");
}

// ---------------------------------------------------------------------------------------------
// The board runner's job path, against the played kernel.

struct Played(Rc<RefCell<Kernel>>);

impl Session for Played {
    fn send(&mut self, messages: &mut [lemnos_driver_rk_mpp::Message<'_>]) -> io::Result<()> {
        self.0.borrow_mut().call(messages)
    }

    fn read_back(&mut self, slot: usize, into: &mut [u8]) {
        let n = into.len();
        into.copy_from_slice(&self.0.borrow().areas[slot][..n]);
    }
}

struct PlayedDevice(Rc<RefCell<Kernel>>);

impl ServiceDevice for PlayedDevice {
    type Session = Played;

    fn open(&mut self) -> io::Result<Played> {
        let mut kernel = self.0.borrow_mut();
        if let Some(errno) = kernel.fail_open.take() {
            return Err(io::Error::from_raw_os_error(errno));
        }
        kernel.opens += 1;
        kernel.client = None;
        Ok(Played(Rc::clone(&self.0)))
    }

    fn confined(&mut self, block: Block) -> io::Result<bool> {
        self.0.borrow_mut().bound = Some(block);
        Ok(self.0.borrow().confined)
    }

    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64> {
        dma_buf::size(fd)
    }
}

fn sealed(len: u64) -> io::Result<OwnedFd> {
    dma_buf::sealed_memfd(c"rkjpeg-probe", len)
}

#[test]
fn the_board_runner_places_every_address_through_the_claim() {
    let kernel = Rc::new(RefCell::new(Kernel::new(1 << 13)));
    // A fixture of the stream's length: 64 bytes after the stream offset.
    let input = vec![0x5a; STREAM_OFF as usize + 64];
    let mut board = probe::Board::new(PlayedDevice(Rc::clone(&kernel)), sealed, input);
    let mut mem = vec![SENTINEL; 4096];
    let status = board
        .run(&file(0, 1, [4, 4, 4]), &mut mem)
        .expect("the job runs on the played kernel");
    // The played kernel does not write the output, so the buffer comes back as it went in.
    assert!(mem.iter().all(|&b| b == SENTINEL));
    // The read-back is the played kernel's, which is zero for a job it did not finish.
    assert_eq!(status, 0);

    let kernel = kernel.borrow();
    // Five address registers, each translated once, in the job's order.
    assert_eq!(kernel.translated_len, 5);
    let fds = &kernel.translated[..5];
    let task = &kernel.tasks[0].regs;
    // Each address is its buffer's device address plus the offset the job names for it.
    assert_eq!(task[REG_QTBL], iova(fds[0]) + QTBL_OFF);
    assert_eq!(task[REG_HMIN], iova(fds[1]) + HMIN_OFF);
    assert_eq!(task[REG_HVAL], iova(fds[2]) + HVAL_OFF);
    assert_eq!(task[REG_STRM], iova(fds[3]) + STREAM_OFF);
    assert_eq!(task[REG_OUT], iova(fds[4]));
    // The stream length: 64 bytes, in bits 31:4.
    assert_eq!(task[REG_STRM_LEN], 64 << 4);
    // The output is its own buffer, not the fixture's.
    assert_ne!(fds[4], fds[0]);
}

#[test]
fn the_fixture_jpeg_parses_back_to_the_picture_and_tables_it_declares() {
    let parsed = fixture::parse_jpeg(&fixture::jpeg()).expect("the fixture is a JPEG");
    assert_eq!(parsed.dims, (fixture::WIDTH, fixture::HEIGHT));
    assert_eq!(parsed.dqt, DQT_PAYLOAD);
    assert_eq!(parsed.dc_counts, COUNTS);
    assert_eq!(parsed.dc_symbols, SYMBOLS);
    assert_eq!(parsed.entropy_len, fixture::blocks() * 2 / 8);
}

#[test]
fn the_board_input_holds_each_table_at_its_offset_and_the_stream_after_them() {
    let input = fixture::board_input();
    let jpeg = fixture::jpeg();
    let q = QTBL_OFF as usize;
    let hmin = HMIN_OFF as usize;
    let hval = HVAL_OFF as usize;
    assert_eq!(&input[q..q + TABLE_BYTES], DQT_PAYLOAD);
    assert_eq!(&input[hmin..hmin + COUNTS.len()], COUNTS);
    assert!(
        input[hmin + COUNTS.len()..hmin + TABLE_BYTES]
            .iter()
            .all(|&b| b == 0)
    );
    assert_eq!(&input[hval..hval + SYMBOLS.len()], SYMBOLS);
    assert!(
        input[hval + SYMBOLS.len()..hval + TABLE_BYTES]
            .iter()
            .all(|&b| b == 0)
    );
    assert_eq!(&input[STREAM_OFF as usize..], jpeg);
    // The probe's stream length is the bytes after the stream offset: the whole JPEG.
    assert_eq!(input.len() - STREAM_OFF as usize, jpeg.len());
    let parsed = fixture::parse_jpeg(&input[STREAM_OFF as usize..]).expect("the stream parses");
    assert_eq!(parsed.dqt, DQT_PAYLOAD);
}

#[test]
fn the_stated_targets_share_one_field_value_under_four_byte_words() {
    // The allotted length is the table's length the probe states. Under the words unit each one
    // makes the same field value, so the joint sweep accepts exactly that value.
    assert_eq!(FIXTURE_TARGETS, [TABLE_BYTES as u32; 3]);
    let targets = Targets {
        qtbl: FIXTURE_TARGETS[0],
        mincode: FIXTURE_TARGETS[1],
        value: FIXTURE_TARGETS[2],
    };
    assert_eq!(
        expected_joint([TableUnit::Words4; 3], targets),
        vec![TABLE_BYTES as u32 / 4]
    );
    assert!(match_units(&[TABLE_BYTES as u32 / 4], targets).contains(&[TableUnit::Words4; 3]));
}

#[test]
#[ignore = "writes the board fixture file: RKJPEG_FIXTURE_OUT=<path>"]
fn write_board_fixture() {
    let path = std::env::var("RKJPEG_FIXTURE_OUT").expect("RKJPEG_FIXTURE_OUT");
    std::fs::write(&path, fixture::board_input()).expect("the fixture can be written");
}

// ---------------------------------------------------------------------------------------------
// The board. Run only on the board: `RKJPEG_NODE` (the service node), `RKJPEG_HEAP` (a DMA heap
// node), `RKJPEG_FIXTURE` (the input buffer as bytes, `[qtable | mincode | value | stream]` at the
// offsets in `probe`), `RKJPEG_TARGETS` (the fixture's table lengths, `qtbl,mincode,value`).
// Results print to stdout for `testing/board-runs/RKJPEG-DEC.md`.

#[test]
#[ignore = "runs on the board only; see testing/board-runs/RKJPEG-DEC.md"]
#[allow(clippy::print_stdout)] // the board run prints each answer as its evidence
fn board_probe_on_the_service_node() {
    use lemnos_driver_rk_mpp_sys::ServiceNode;

    let node = std::env::var("RKJPEG_NODE").expect("RKJPEG_NODE");
    let heap_path = std::env::var("RKJPEG_HEAP").expect("RKJPEG_HEAP");
    let fixture = std::fs::read(std::env::var("RKJPEG_FIXTURE").expect("RKJPEG_FIXTURE"))
        .expect("read the fixture");
    let heap = std::fs::File::open(heap_path).expect("open the heap");
    let heap = OwnedFd::from(heap);
    let alloc = move |len: u64| lemnos_linux_sys::dma_heap::allocate(heap.as_fd(), len);
    let mut board = probe::Board::new(ServiceNode::new(node), alloc, fixture);

    let targets = std::env::var("RKJPEG_TARGETS").expect("RKJPEG_TARGETS as qtbl,mincode,value");
    let parts: Vec<u32> = targets
        .split(',')
        .map(|v| v.trim().parse().expect("a byte count"))
        .collect();
    let targets = Targets {
        qtbl: parts[0],
        mincode: parts[1],
        value: parts[2],
    };

    // Each format code's joint table sweep. The first code with one unit triple names the units
    // the chroma and reach probes use.
    let mut answer = None;
    for fmt in 0..=probe::YUV_FMT_MASK {
        let accepted = probe_joint(&mut board, fmt).expect("joint sweep");
        let units = match_units(&accepted, targets);
        println!("format {fmt}: accepted {accepted:?}, units {units:?}");
        if let (&[unit], None) = (units.as_slice(), answer) {
            answer = Some((fmt, unit));
        }
    }
    let Some((fmt, units)) = answer else {
        panic!("no format code gave one unit triple: record the sweep above and stop");
    };
    let encoding = std::array::from_fn(|i| {
        let want = [targets.qtbl, targets.mincode, targets.value][i];
        units[i]
            .encode(want, FIELDS[i])
            .unwrap_or_else(|| panic!("{:?} cannot make {want} bytes", units[i]))
    });
    println!(
        "chroma at format {fmt}: {:?}",
        probe_chroma(&mut board, fmt, encoding).expect("chroma")
    );
    println!(
        "reach at format {fmt}: {:?}",
        probe_reach(&mut board, fmt, encoding).expect("reach")
    );
}
