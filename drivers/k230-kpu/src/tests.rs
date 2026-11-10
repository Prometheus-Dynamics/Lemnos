//! The driver and the checks against a model of what the block reaches. No test opens a device
//! node.

use std::io;

use lemnos_contract_npu::{Counters, Npu};
use lemnos_core::Status;
use lemnos_measure::test_alloc::allocations;

use crate::confine::{self, Facts, Gaps, Range, Table, check_reach};
use crate::driver::{Device, Kernel, Kpu, MAX_REACH, State, backoff_ns};
use crate::request::{self, Allocated, MAX_ALLOC, PAGE, RequestError};
use crate::uapi::{self, MmzInfo};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

/// The memory zone as `mmz.c` behaves: allocations answer physical addresses, and nothing in
/// front of the block narrows what it reaches, which is all of memory whether or not a job runs.
struct Board {
    next: u64,
}

impl Board {
    fn new() -> Self {
        Self { next: 0x1000_0000 }
    }

    /// `MMZ_ALLOC_MEM`: a physical range, page aligned.
    fn alloc(&mut self, asked: &MmzInfo) -> MmzInfo {
        let answer = MmzInfo {
            user_virt_addr: asked.user_virt_addr,
            kernel_virt_addr: 0xffff_ffd8_0000_0000 + self.next,
            mmz_phys: self.next,
            length: asked.length,
        };
        self.next += asked.length.next_multiple_of(PAGE);
        answer
    }

    /// What the block reaches: no translation, so everything.
    fn reach(&self) -> Vec<Range> {
        vec![Range::EVERYTHING]
    }
}

/// A block behind a protection unit that holds one job's windows, as a confining path would need:
/// the windows set when the job starts, cleared when it ends.
#[derive(Default)]
struct Windows {
    open: Vec<Range>,
}

/// The device: a kernel answer and a reach, and a count of every call.
struct Played {
    kernel: Option<Kernel>,
    reach: Vec<Range>,
    known: bool,
    calls: u32,
}

impl Played {
    fn new(kernel: Kernel, reach: Vec<Range>) -> Self {
        Self {
            kernel: Some(kernel),
            reach,
            known: true,
            calls: 0,
        }
    }
}

impl Device for Played {
    fn kernel(&mut self) -> io::Result<Kernel> {
        self.calls += 1;
        self.kernel
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }

    fn reach(&mut self, into: &mut [Range]) -> io::Result<Option<usize>> {
        self.calls += 1;
        if !self.known || self.reach.len() > into.len() {
            return Ok(None);
        }
        into[..self.reach.len()].copy_from_slice(&self.reach);
        Ok(Some(self.reach.len()))
    }
}

// Layouts: `layout.rs` asserts them at compile time; these pin the facts that are not layouts.

#[test]
fn the_nodes_window_and_requests_are_pinned() {
    assert_eq!(uapi::DEVICE, "/dev/k230-gnne");
    assert_eq!(uapi::COMPATIBLE, "k230-gnne");
    assert_eq!((uapi::REGISTERS, uapi::REGISTERS_LEN), (0x8040_0000, 0x800));
    assert_eq!(uapi::IRQ, 189);
    assert_eq!(uapi::IRQ_CLEAR, (0x128, 0x4_0000_0004));
    assert_eq!(uapi::MMZ_DEVICE, "/dev/mmz");
    assert_eq!(uapi::MMZ_ALLOC_MEM, 0xc008_6701);
    assert_eq!(uapi::MMZ_FREE_MEM, 0xc008_6702);
}

// The checked builders.

#[test]
fn an_allocation_is_whole_pages_with_no_address_from_the_caller() {
    let asked = request::alloc(2 * PAGE).unwrap();
    assert_eq!(asked.length, 2 * PAGE);
    assert_eq!(
        (asked.user_virt_addr, asked.kernel_virt_addr, asked.mmz_phys),
        (0, 0, 0)
    );
    assert_eq!(request::alloc(0), Err(RequestError::Empty));
    assert_eq!(request::alloc(PAGE + 1), Err(RequestError::Unaligned));
    assert_eq!(
        request::alloc(MAX_ALLOC),
        Ok(MmzInfo {
            length: MAX_ALLOC,
            ..MmzInfo::default()
        })
    );
    assert_eq!(
        request::alloc(MAX_ALLOC + PAGE),
        Err(RequestError::TooLarge)
    );
}

#[test]
fn only_a_sound_answer_becomes_an_allocation_and_a_free_names_only_it() {
    let mut board = Board::new();
    let asked = request::alloc(PAGE).unwrap();
    let answer = board.alloc(&asked);
    let held = Allocated::new(&asked, &answer).unwrap();
    assert_eq!(request::free(&held), answer);

    for broken in [
        MmzInfo {
            length: 2 * PAGE,
            ..answer
        },
        MmzInfo {
            mmz_phys: 0,
            ..answer
        },
        MmzInfo {
            kernel_virt_addr: 0,
            ..answer
        },
        MmzInfo {
            mmz_phys: answer.mmz_phys + 8,
            ..answer
        },
        MmzInfo {
            mmz_phys: u64::MAX - PAGE + 1,
            ..answer
        },
    ] {
        assert_eq!(
            Allocated::new(&asked, &broken),
            Err(RequestError::Answer),
            "{broken:?}"
        );
    }
}

#[test]
fn a_mapping_names_only_pages_of_an_allocation() {
    let mut board = Board::new();
    let asked = request::alloc(4 * PAGE).unwrap();
    let held = Allocated::new(&asked, &board.alloc(&asked)).unwrap();
    let base = held.physical();
    assert_eq!(request::map(&held, 0, 4 * PAGE), Ok((base, 4 * PAGE)));
    assert_eq!(
        request::map(&held, PAGE, 3 * PAGE),
        Ok((base + PAGE, 3 * PAGE))
    );
    assert_eq!(
        request::map(&held, PAGE, 4 * PAGE),
        Err(RequestError::TooLarge)
    );
    assert_eq!(request::map(&held, 0, 0), Err(RequestError::Empty));
    assert_eq!(request::map(&held, 1, PAGE), Err(RequestError::Unaligned));
    assert_eq!(
        request::map(&held, u64::MAX - PAGE + 1, PAGE),
        Err(RequestError::TooLarge)
    );
}

// What the kernel lets a job reach.

#[test]
fn the_sdk_kernel_has_every_gap_read_from_its_source() {
    let gaps = confine::GNNE.gaps();
    for gap in [
        Gaps::NO_TRANSLATION,
        Gaps::NO_SUBMISSION,
        Gaps::REGISTER_ACCESS,
        Gaps::RAW_PHYSICAL,
        Gaps::SHARED_OPEN,
    ] {
        assert!(gaps.contains(gap), "{gap:?} missing from {gaps:?}");
    }
    assert!(!gaps.contains(Gaps::NOT_AUDITED));
}

#[test]
fn closing_everything_a_kernel_path_could_still_leaves_no_translation() {
    // What a Lemnos kernel path could close: it takes the job, keeps the registers and physical
    // memory to itself, and holds the block for one claim.
    let module = Facts {
        submits: true,
        register_access: false,
        raw_physical: false,
        shared_open: false,
        ..confine::GNNE
    };
    assert_eq!(module.gaps(), Gaps::NO_TRANSLATION);
    // Only a per-job protection unit in front of the block leaves no gap.
    let protected = Facts {
        table: Table::Job,
        ..module
    };
    assert!(protected.gaps().is_empty());
    let shared = Facts {
        table: Table::Device,
        ..module
    };
    assert_eq!(shared.gaps(), Gaps::SHARED_TABLE);
}

#[test]
fn on_the_board_a_job_reaches_past_its_buffers() {
    let mut board = Board::new();
    let asked = request::alloc(4 * PAGE).unwrap();
    let input = Allocated::new(&asked, &board.alloc(&asked)).unwrap();
    let output = Allocated::new(&asked, &board.alloc(&asked)).unwrap();
    let own = [
        Range {
            start: input.physical(),
            len: input.length(),
        },
        Range {
            start: output.physical(),
            len: output.length(),
        },
    ];
    assert_eq!(check_reach(&board.reach(), &own), Err(Range::EVERYTHING));
    // Including, at the least, the block's own register window and every other allocation.
    let registers = Range {
        start: uapi::REGISTERS,
        len: uapi::REGISTERS_LEN,
    };
    assert!(Range::EVERYTHING.holds(&registers));
    assert!(!own.iter().any(|b| b.holds(&registers)));
}

#[test]
fn behind_per_job_windows_a_job_reaches_only_its_own_buffers() {
    let mut windows = Windows::default();
    let own = [
        Range {
            start: 0x1000_0000,
            len: 4 * PAGE,
        },
        Range {
            start: 0x2000_0000,
            len: PAGE,
        },
    ];
    windows.open.extend_from_slice(&own);
    assert_eq!(check_reach(&windows.open, &own), Ok(()));

    // A window one page past its buffer is reach.
    let mut long = windows.open.clone();
    long[0].len += PAGE;
    assert_eq!(check_reach(&long, &own), Err(long[0]));
    // So is a window that starts before its buffer, and one that wraps the address space.
    let mut early = windows.open.clone();
    early[1].start -= 1;
    early[1].len += 1;
    assert_eq!(check_reach(&early, &own), Err(early[1]));
    let wraps = [Range {
        start: u64::MAX,
        len: 2,
    }];
    assert!(check_reach(&wraps, &[Range::EVERYTHING]).is_err());

    // Windows not cleared between jobs leave the last job's buffers in the next one's reach.
    let next = [Range {
        start: 0x3000_0000,
        len: PAGE,
    }];
    windows.open.extend_from_slice(&next);
    assert!(check_reach(&windows.open, &next).is_err());
    windows.open.clear();
    windows.open.extend_from_slice(&next);
    assert_eq!(check_reach(&windows.open, &next), Ok(()));
}

// The driver.

#[test]
fn the_driver_finds_the_block_unconfined_and_reaching_everything_while_idle() {
    let board = Board::new();
    let mut kpu = Kpu::new(Played::new(Kernel::Gnne, board.reach()));
    assert_eq!(kpu.state(), State::Unchecked);
    let State::Unconfined(gaps) = kpu.check(0) else {
        panic!("{:?}", kpu.state());
    };
    assert!(gaps.contains(confine::GNNE.gaps()));
    assert!(gaps.contains(Gaps::REACH));
}

#[test]
fn a_reach_that_cannot_be_read_or_an_unknown_kernel_is_unconfined() {
    let mut device = Played::new(Kernel::Gnne, Vec::new());
    device.known = false;
    let mut kpu = Kpu::new(device);
    let State::Unconfined(gaps) = kpu.check(0) else {
        panic!();
    };
    assert!(gaps.contains(Gaps::REACH));

    let mut kpu = Kpu::new(Played::new(Kernel::Other, Vec::new()));
    assert_eq!(kpu.check(0), State::Unconfined(Gaps::UNKNOWN_KERNEL));

    let too_many = vec![Range { start: 0, len: 1 }; MAX_REACH + 1];
    let mut kpu = Kpu::new(Played::new(Kernel::Other, too_many));
    assert_eq!(
        kpu.check(0),
        State::Unconfined(Gaps::UNKNOWN_KERNEL.with(Gaps::REACH))
    );
}

#[test]
fn a_failing_device_backs_off_to_eight_seconds_and_is_tried_again() {
    assert_eq!(backoff_ns(1), 250_000_000);
    assert_eq!(backoff_ns(2), 500_000_000);
    assert_eq!(backoff_ns(6), 8_000_000_000);
    assert_eq!(backoff_ns(100), 8_000_000_000);

    let mut device = Played::new(Kernel::Gnne, Board::new().reach());
    device.kernel = None;
    let mut kpu = Kpu::new(device);
    let mut now = 0;
    for failures in 1..=8u32 {
        let retry = now + backoff_ns(failures);
        assert_eq!(kpu.check(now), State::BackingOff { retry_at_ns: retry });
        let calls = kpu.device().calls;
        // Before it is due, the device is not touched.
        assert_eq!(
            kpu.check(retry - 1),
            State::BackingOff { retry_at_ns: retry }
        );
        assert_eq!(kpu.device().calls, calls);
        now = retry;
    }
    // The device comes back: the check answers at once, and the backoff clears.
    kpu.device_mut().kernel = Some(Kernel::Gnne);
    let State::Unconfined(_) = kpu.check(now) else {
        panic!("{:?}", kpu.state());
    };
    kpu.device_mut().kernel = None;
    assert_eq!(
        kpu.check(now + 1),
        State::BackingOff {
            retry_at_ns: now + 1 + backoff_ns(1)
        }
    );
}

#[test]
fn the_contract_loads_nothing_and_says_so() {
    let mut kpu = Kpu::new(Played::new(Kernel::Gnne, Board::new().reach()));
    let info = kpu.info().unwrap();
    assert_eq!(
        (info.max_program_bytes, info.max_programs, info.max_attached),
        (0, 0, 0)
    );
    assert_eq!(kpu.load(0, 0), Err(Status::INVALID_ARGUMENT));
    assert_eq!(kpu.load(0, 64), Err(Status::NOT_SUPPORTED));
    assert_eq!(kpu.unload(1), Err(Status::NOT_FOUND));
    assert_eq!(kpu.attach(1, 0, 0, 4), Err(Status::NOT_FOUND));
    assert_eq!(kpu.detach(1, 0), Err(Status::NOT_FOUND));
    assert_eq!(kpu.job_stream(&mut [0; 32]), Err(Status::NOT_SUPPORTED));
    assert_eq!(kpu.counters(), Ok(Counters::default()));
    // Refusing touched no device.
    assert_eq!(kpu.device().calls, 0);
}

#[test]
fn checks_and_refusals_allocate_nothing() {
    let mut kpu = Kpu::new(Played::new(Kernel::Gnne, Board::new().reach()));
    let own = [Range {
        start: 0x1000_0000,
        len: PAGE,
    }];
    kpu.check(0);
    let ((), count) = allocations(|| {
        for now in 0..1000 {
            assert!(matches!(kpu.check(now), State::Unconfined(_)));
            assert_eq!(kpu.load(0, 64), Err(Status::NOT_SUPPORTED));
            assert_eq!(check_reach(&own, &own), Ok(()));
        }
    });
    assert_eq!(count, 0, "allocations in 1000 checks");
}
