//! The driver and the checks against models of the kernel's mapping behaviour. No test opens a
//! device node.

use std::io;

use lemnos_contract_npu::{Counters, Npu};
use lemnos_core::Status;
use lemnos_measure::test_alloc::allocations;

use crate::confine::{self, Facts, Gaps, Range, SLOT_SPAN, Table, check_reach, slot_address};
use crate::driver::{Device, Kernel, MAX_MAPPED, MAX_PROGRAM_BYTES, State, Vip9000, backoff_ns};
use crate::request::{self, RequestError, Withheld, withheld};
use crate::uapi::{APPEND_COMMAND_SIZE, Command, MAX_CMD_SIZE, alloc_flag, patch, task_cmd};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

const PAGE: u64 = 4096;
const MIB: u64 = 1 << 20;

/// Who put a mapping in the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    /// The kernel, at start-up.
    Kernel,
    /// An opener of the node, by number. Lemnos is 0.
    Opener(u32),
}

/// The vendor `vipcore`'s table, as `memory/vip_drv_mmu.c` keeps it: one table for the device
/// (`vpmdMMU_SINGLE_PAGE`), the heap and the on-chip SRAM mapped at start-up and kept, every
/// opener's wraps added at addresses the kernel picks and kept until unwrapped, heap allocations
/// carved from the heap's mapping (so freeing one unmaps nothing).
struct VipcoreTable {
    maps: Vec<(Range, Owner)>,
    heap: Range,
    heap_used: u64,
    next: u64,
}

impl VipcoreTable {
    fn start() -> Self {
        // mmu_map_vip_sram, then mmu_map_memory_heap (VIDEO_MEMORY_HEAP_SIZE, 16 MiB).
        let sram = Range {
            start: 0x0,
            len: 512 * 1024,
        };
        let heap = Range {
            start: MIB,
            len: 16 * MIB,
        };
        Self {
            maps: vec![(sram, Owner::Kernel), (heap, Owner::Kernel)],
            heap,
            heap_used: 0,
            next: 64 * MIB,
        }
    }

    /// `WrapUserFd` from `opener`: the kernel picks the address, page by page.
    fn wrap(&mut self, opener: u32, bytes: u64) -> Range {
        let range = Range {
            start: self.next,
            len: bytes,
        };
        self.next += bytes.next_multiple_of(PAGE);
        self.maps.push((range, Owner::Opener(opener)));
        range
    }

    /// `UnwrapUserFd`.
    fn unwrap(&mut self, range: Range) {
        self.maps.retain(|(r, _)| *r != range);
    }

    /// `Allocation` from the heap: inside the heap's mapping, nothing new is mapped.
    fn alloc(&mut self, bytes: u64) -> Range {
        let range = Range {
            start: self.heap.start + self.heap_used,
            len: bytes,
        };
        self.heap_used += bytes.next_multiple_of(64);
        range
    }

    fn ranges(&self) -> Vec<Range> {
        self.maps.iter().map(|(r, _)| *r).collect()
    }

    fn owner(&self, range: Range) -> Option<Owner> {
        self.maps.iter().find(|(r, _)| *r == range).map(|(_, o)| *o)
    }
}

/// A table per job, as a confining kernel path would keep it: the job's slots at their fixed
/// addresses and its command buffer, emptied when the job ends.
#[derive(Default)]
struct JobTable {
    maps: Vec<Range>,
}

impl JobTable {
    fn begin(&mut self, slots: &[u64], commands: u64) -> Vec<Range> {
        let mut own = Vec::new();
        for (slot, &bytes) in slots.iter().enumerate() {
            own.push(slot_address(slot as u16, bytes).unwrap());
        }
        own.push(Range {
            start: 0x1000,
            len: commands,
        });
        self.maps.extend_from_slice(&own);
        own
    }

    fn end(&mut self) {
        self.maps.clear();
    }
}

/// The device: a kernel answer and a table, and a count of every call.
struct Played {
    kernel: Option<Kernel>,
    table: Vec<Range>,
    /// `Some(false)`: the kernel cannot list its table.
    listed: bool,
    calls: u32,
}

impl Played {
    fn new(kernel: Kernel, table: Vec<Range>) -> Self {
        Self {
            kernel: Some(kernel),
            table,
            listed: true,
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

    fn mapped(&mut self, into: &mut [Range]) -> io::Result<Option<usize>> {
        self.calls += 1;
        if !self.listed || self.table.len() > into.len() {
            return Ok(None);
        }
        into[..self.table.len()].copy_from_slice(&self.table);
        Ok(Some(self.table.len()))
    }
}

// Layouts: the sizes, offsets and numbers are asserted at compile time in `layout.rs`.

#[test]
fn the_commands_are_numbered_in_order() {
    for (n, command) in Command::ALL.iter().enumerate() {
        assert_eq!(*command as u32, n as u32);
    }
}

#[test]
fn register_physical_and_page_table_commands_are_never_sent() {
    assert_eq!(withheld(Command::ReadReg), Some(Withheld::RegisterAccess));
    assert_eq!(withheld(Command::WriteReg), Some(Withheld::RegisterAccess));
    assert_eq!(
        withheld(Command::WrapUserPhysical),
        Some(Withheld::RawPhysical)
    );
    assert_eq!(
        withheld(Command::WrapUserMemory),
        Some(Withheld::UserMemory)
    );
    assert_eq!(withheld(Command::QueryMmuInfo), Some(Withheld::PageTables));
    assert_eq!(
        withheld(Command::MapUserLogical),
        Some(Withheld::PageTables)
    );
    let sent: Vec<_> = Command::ALL
        .into_iter()
        .filter(|c| withheld(*c).is_none())
        .collect();
    assert_eq!(
        sent,
        [
            Command::Init,
            Command::Destroy,
            Command::WaitTask,
            Command::SubmitTask,
            Command::Allocation,
            Command::Deallocation,
            Command::QueryDatabase,
            Command::OperateCache,
            Command::WrapUserFd,
            Command::UnwrapUserFd,
            Command::CancelTask,
            Command::CreateTask,
            Command::SetTaskProperty,
            Command::DestroyTask,
        ]
    );
}

// The checked builders.

#[test]
fn a_wrap_names_only_bytes_of_a_buffer_the_claim_holds() {
    let wrap = request::wrap_fd(7, 8192, 4096).unwrap();
    assert_eq!((wrap.fd, wrap.size, wrap.device_vis), (7, 4096, 1));
    assert_eq!(
        (wrap.memory_type, wrap.virtual_addr, wrap.mem_id),
        (0, 0, 0)
    );
    assert_eq!(request::wrap_fd(7, 8192, 8192).unwrap().size, 8192);
    assert_eq!(request::wrap_fd(7, 8192, 8193), Err(RequestError::TooLarge));
    assert_eq!(request::wrap_fd(7, 8192, 0), Err(RequestError::Empty));
    assert_eq!(request::wrap_fd(0, 8192, 1), Err(RequestError::Descriptor));
    assert_eq!(request::wrap_fd(-1, 8192, 1), Err(RequestError::Descriptor));
    // vip_size_t is 32 bits.
    assert_eq!(
        request::wrap_fd(7, u64::MAX, 1 << 32),
        Err(RequestError::TooLarge)
    );
}

#[test]
fn a_command_buffer_leaves_room_for_the_kernels_append_and_takes_no_address() {
    assert_eq!(request::command_capacity(8), Ok(64));
    assert_eq!(request::command_capacity(64), Ok(128));
    assert_eq!(request::command_capacity(0), Err(RequestError::Empty));
    assert_eq!(request::command_capacity(12), Err(RequestError::Unaligned));
    let last = MAX_PROGRAM_BYTES;
    assert!(request::command_capacity(last).is_ok());
    assert!(last + APPEND_COMMAND_SIZE <= MAX_CMD_SIZE);
    assert_eq!(
        request::command_capacity(last + 8),
        Err(RequestError::TooLarge)
    );
    assert_eq!(
        request::command_capacity(u32::MAX - 7),
        Err(RequestError::TooLarge)
    );

    let buffer = request::command_buffer(64).unwrap();
    assert_eq!((buffer.size, buffer.align), (128, 64));
    assert_eq!((buffer.specified, buffer.physical), (0, 0));
    assert_eq!(buffer.alloc_flag & alloc_flag::NO_MMU_PAGE, 0);
    assert_eq!(buffer.device_vis, 1);
}

#[test]
fn a_subtask_fits_its_command_buffer_with_the_append_and_carries_no_patch() {
    let capacity = request::command_capacity(64).unwrap();
    let set = request::subtask(3, 9, capacity, 64).unwrap();
    assert_eq!(
        (set.task_id, set.kind, set.index),
        (3, task_cmd::SUBTASK, 0)
    );
    assert_eq!(set.patch_type, patch::NONE);
    assert_eq!(set.param, [9, 64, 0, 0, 0]);
    // 64 + 56 = 120 bytes: a 119-byte buffer would take the append past its end.
    assert_eq!(request::subtask(3, 9, 119, 64), Err(RequestError::TooLarge));
    assert_eq!(request::subtask(3, 9, 1 << 20, 0), Err(RequestError::Empty));
}

#[test]
fn a_submit_is_one_subtask_on_one_core() {
    let submit = request::submit(3, 2000);
    assert_eq!(
        (submit.subtask_index, submit.subtask_cnt, submit.core_cnt),
        (0, 1, 1)
    );
    assert_eq!((submit.device_index, submit.core_index), (0, 0));
    assert_eq!(request::wait(3).subtask_index, 0);
    let task = request::create_task(b"a-name-longer-than-sixteen");
    assert_eq!(task.subtask_count, 1);
    assert_eq!(task.task_name[15], 0);
}

// What the kernel driver lets a job reach.

#[test]
fn vipcore_has_every_gap_read_from_its_source() {
    let gaps = confine::VIPCORE.gaps();
    for gap in [
        Gaps::SHARED_TABLE,
        Gaps::MAPPED_AT_START,
        Gaps::REGISTER_ACCESS,
        Gaps::RAW_PHYSICAL,
        Gaps::SHARED_OPEN,
    ] {
        assert!(gaps.contains(gap), "{gap:?} missing from {gaps:?}");
    }
    assert!(!gaps.contains(Gaps::NOT_AUDITED));
    assert!(confine::GALCORE.gaps().contains(Gaps::NOT_AUDITED));
    assert!(confine::GALCORE.gaps().contains(Gaps::MAPPED_AT_START));
}

#[test]
fn only_a_per_job_table_with_nothing_else_open_has_no_gap() {
    let closed = Facts {
        table: Table::Job,
        mapped_at_start: false,
        register_access: false,
        raw_physical: false,
        shared_open: false,
        audited: true,
    };
    assert!(closed.gaps().is_empty());
    let process = Facts {
        table: Table::Process,
        ..closed
    };
    assert_eq!(process.gaps(), Gaps::SHARED_TABLE);
    let none = Facts {
        table: Table::None,
        ..closed
    };
    assert_eq!(none.gaps(), Gaps::NO_TRANSLATION);
}

#[test]
fn on_vipcores_table_a_job_reaches_the_heap_and_another_openers_buffer() {
    let mut table = VipcoreTable::start();
    // Lemnos (opener 0) maps a job's input and output, and its command buffer from the heap.
    let input = table.wrap(0, 3 * PAGE);
    let output = table.wrap(0, PAGE);
    let commands = table.alloc(256);
    // Another process (opener 1) has its own buffer mapped, in the same table.
    let theirs = table.wrap(1, 2 * PAGE);

    let own = [input, output, commands];
    let escape = check_reach(&table.ranges(), &own).unwrap_err();
    assert_eq!(table.owner(escape), Some(Owner::Kernel));

    // Even with the start-up mappings set aside, the other opener's buffer is in reach.
    let others: Vec<_> = table
        .maps
        .iter()
        .filter(|(_, o)| *o != Owner::Kernel)
        .map(|(r, _)| *r)
        .collect();
    assert_eq!(check_reach(&others, &own), Err(theirs));
    table.unwrap(theirs);
    let others: Vec<_> = table
        .maps
        .iter()
        .filter(|(_, o)| *o != Owner::Kernel)
        .map(|(r, _)| *r)
        .collect();
    assert_eq!(check_reach(&others, &[input, output]), Ok(()));

    // Unwrapping the job's buffers between jobs still leaves the heap mapped, with an earlier
    // job's command buffer in it.
    table.unwrap(input);
    table.unwrap(output);
    assert!(check_reach(&table.ranges(), &[]).is_err());
    assert!(table.heap.holds(&commands));
}

#[test]
fn on_a_per_job_table_a_job_reaches_only_its_own_buffers() {
    let mut table = JobTable::default();
    let own = table.begin(&[3 * PAGE, PAGE], 256);
    assert_eq!(check_reach(&table.maps, &own), Ok(()));

    // A slot mapped one page past its buffer is reach.
    let mut long = table.maps.clone();
    long[0].len += PAGE;
    assert_eq!(check_reach(&long, &own), Err(long[0]));
    // So is a slot moved off its address, and one that wraps the address space.
    let mut moved = table.maps.clone();
    moved[1].start += 1;
    assert_eq!(check_reach(&moved, &own), Err(moved[1]));
    let wraps = [Range {
        start: u64::MAX,
        len: 2,
    }];
    let everything = [Range {
        start: 0,
        len: u64::MAX,
    }];
    assert!(check_reach(&wraps, &everything).is_err());

    // A table not emptied between jobs leaves the last job's buffers in the next one's reach.
    let next = table.begin(&[PAGE], 64);
    assert!(check_reach(&table.maps, &next).is_err());
    table.end();
    let next = table.begin(&[PAGE], 64);
    assert_eq!(check_reach(&table.maps, &next), Ok(()));
}

#[test]
fn slots_sit_at_fixed_addresses_that_never_overlap() {
    let a = slot_address(0, SLOT_SPAN).unwrap();
    let b = slot_address(1, 1).unwrap();
    assert_eq!(a.end(), Some(b.start));
    assert_eq!(slot_address(2, SLOT_SPAN + 1), None);
    assert_eq!(slot_address(2, 0), None);
    let last = slot_address(u16::MAX, SLOT_SPAN).unwrap();
    assert!(last.end().is_some());
    // A slot's buffer is never in another slot's range.
    assert!(!a.holds(&b) && !b.holds(&a));
}

// The driver.

#[test]
fn the_driver_finds_vipcore_unconfined_and_sees_the_heap_mapped_while_idle() {
    let table = VipcoreTable::start();
    let mut npu = Vip9000::new(Played::new(Kernel::Vipcore, table.ranges()));
    assert_eq!(npu.state(), State::Unchecked);
    let State::Unconfined(gaps) = npu.check(0) else {
        panic!("{:?}", npu.state());
    };
    assert!(gaps.contains(confine::VIPCORE.gaps()));
    assert!(gaps.contains(Gaps::REACH));
}

#[test]
fn a_kernel_that_cannot_list_its_table_or_is_unknown_is_unconfined() {
    let mut device = Played::new(Kernel::Vipcore, Vec::new());
    device.listed = false;
    let mut npu = Vip9000::new(device);
    let State::Unconfined(gaps) = npu.check(0) else {
        panic!();
    };
    assert!(gaps.contains(Gaps::REACH));

    let mut npu = Vip9000::new(Played::new(Kernel::Other, Vec::new()));
    assert_eq!(npu.check(0), State::Unconfined(Gaps::UNKNOWN_KERNEL));

    let too_many = vec![Range { start: 0, len: 1 }; MAX_MAPPED + 1];
    let mut npu = Vip9000::new(Played::new(Kernel::Other, too_many));
    assert_eq!(
        npu.check(0),
        State::Unconfined(Gaps::UNKNOWN_KERNEL.with(Gaps::REACH))
    );
}

#[test]
fn a_failing_device_backs_off_to_eight_seconds_and_is_tried_again() {
    assert_eq!(backoff_ns(1), 250_000_000);
    assert_eq!(backoff_ns(2), 500_000_000);
    assert_eq!(backoff_ns(6), 8_000_000_000);
    assert_eq!(backoff_ns(100), 8_000_000_000);

    let mut device = Played::new(Kernel::Vipcore, VipcoreTable::start().ranges());
    device.kernel = None;
    let mut npu = Vip9000::new(device);
    let mut now = 0;
    for failures in 1..=8u32 {
        let retry = now + backoff_ns(failures);
        assert_eq!(npu.check(now), State::BackingOff { retry_at_ns: retry });
        let calls = npu.device().calls;
        // Before it is due, the device is not touched.
        assert_eq!(
            npu.check(retry - 1),
            State::BackingOff { retry_at_ns: retry }
        );
        assert_eq!(npu.device().calls, calls);
        now = retry;
    }
    // The device comes back: the check answers at once, and the backoff clears.
    npu.device_mut().kernel = Some(Kernel::Vipcore);
    let State::Unconfined(_) = npu.check(now) else {
        panic!("{:?}", npu.state());
    };
    npu.device_mut().kernel = None;
    assert_eq!(
        npu.check(now + 1),
        State::BackingOff {
            retry_at_ns: now + 1 + backoff_ns(1)
        }
    );
}

#[test]
fn the_contract_loads_nothing_and_says_so() {
    let mut npu = Vip9000::new(Played::new(Kernel::Vipcore, VipcoreTable::start().ranges()));
    let info = npu.info().unwrap();
    assert_eq!(info.max_program_bytes, MAX_PROGRAM_BYTES);
    assert_eq!((info.max_programs, info.max_attached), (0, 0));
    assert_eq!(npu.load(0, 0), Err(Status::INVALID_ARGUMENT));
    assert_eq!(
        npu.load(0, MAX_PROGRAM_BYTES + 1),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(npu.load(0, 64), Err(Status::NOT_SUPPORTED));
    assert_eq!(npu.unload(1), Err(Status::NOT_FOUND));
    assert_eq!(npu.attach(1, 0, 0, 4), Err(Status::NOT_FOUND));
    assert_eq!(npu.detach(1, 0), Err(Status::NOT_FOUND));
    assert_eq!(npu.job_stream(&mut [0; 32]), Err(Status::NOT_SUPPORTED));
    assert_eq!(npu.counters(), Ok(Counters::default()));
    // Refusing touched no device.
    assert_eq!(npu.device().calls, 0);
}

#[test]
fn checks_and_refusals_allocate_nothing() {
    let mut npu = Vip9000::new(Played::new(Kernel::Vipcore, VipcoreTable::start().ranges()));
    let mut job = JobTable::default();
    let own = job.begin(&[PAGE, PAGE], 64);
    npu.check(0);
    let ((), count) = allocations(|| {
        for now in 0..1000 {
            assert!(matches!(npu.check(now), State::Unconfined(_)));
            assert_eq!(npu.load(0, 64), Err(Status::NOT_SUPPORTED));
            assert_eq!(check_reach(&job.maps, &own), Ok(()));
        }
    });
    assert_eq!(count, 0, "allocations in 1000 checks");
}
