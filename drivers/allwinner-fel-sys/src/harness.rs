//! The thunk's pinned bytes, run under user-mode emulation.
//!
//! The harness runs the blob the crate pins, not the naked functions. `scripts/check-blob.sh` checks that the
//! built thunk's bytes are those pinned words, so a run of the pinned bytes is a run of the built thunk. The
//! blob is copied into an executable, shared mapping at a low address (the thunk loads 32-bit addresses),
//! next to a return-only entry, a stand-in SPL that records what it saw, and the words the fields name.
//!
//! Each run is a forked child. A child that spins is killed by the parent after a bound, and the shared words
//! show what the thunk had done when it stopped: the watchdog still armed, nothing swapped, no SPL call. That
//! is the "ends in a reset" state of a failed check, as far as a user-mode run can show it.
//!
//! The stand-in SPL is a few instructions, written here as words, so it runs on the same CPU as the thunk.
//! It stores the watchdog word it finds, and the first word of the first buffer, into the shared cells.

use core::ffi::c_void;
use std::time::{Duration, Instant};

use lemnos_allwinner_fel_blob::layout::{self, Layout};
use lemnos_allwinner_fel_blob::{Fields, Swap, Watchdog, blob};

#[cfg(target_arch = "arm")]
const LAYOUT: Layout = layout::ARMV7;
#[cfg(target_arch = "aarch64")]
const LAYOUT: Layout = layout::AARCH64;

#[cfg(target_arch = "arm")]
const PINNED: &[u32] = lemnos_allwinner_fel_blob::pinned::ARMV7_WORDS;
#[cfg(target_arch = "aarch64")]
const PINNED: &[u32] = lemnos_allwinner_fel_blob::pinned::AARCH64_WORDS;

/// Offsets inside one arena, the single shared mapping a test uses.
const BLOB_AT: usize = 0x0000;
const SPL_AT: usize = 0x1000;
const PROBE_AT: usize = 0x1100;
const WDT_AT: usize = 0x2000;
const SEEN_WDT_AT: usize = 0x2008;
const SEEN_BUF_AT: usize = 0x2010;
const BUF_AT: usize = 0x3000;
const BACKUP_AT: usize = 0x3100;
const ARENA_LEN: usize = 0x4000;
const SWAP_LEN: usize = 0x100;

const ARM_VALUE: u32 = 0x5a5a_0001;
const DISARM_VALUE: u32 = 0x0000_0002;
const SENTINEL: u32 = 0xdead_beef;
const BUF_PATTERN: u32 = 0x1100_0000;
const BACKUP_PATTERN: u32 = 0x2200_0000;

/// One arena per test, so tests can run in parallel. Arenas sit below 4 GiB, one megabyte apart.
fn arena_base(slot: usize) -> usize {
    0x2000_0000 + slot * 0x0010_0000
}

/// A shared, executable mapping at a fixed low address. Children inherit it, so the parent reads what they wrote.
struct Arena {
    base: usize,
}

impl Arena {
    fn new(slot: usize) -> Arena {
        let base = arena_base(slot);
        // SAFETY: a new anonymous mapping at a hint address that must be free (MAP_FIXED_NOREPLACE); it aliases
        // no memory this process uses, and the result is checked against the hint below.
        let got = unsafe {
            libc::mmap(
                base as *mut c_void,
                ARENA_LEN,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_SHARED | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE,
                -1,
                0,
            )
        };
        assert_eq!(got as usize, base, "the arena did not map at {base:#x}");
        Arena { base }
    }

    fn at(&self, offset: usize) -> usize {
        self.base + offset
    }

    fn poke32(&self, offset: usize, value: u32) {
        // SAFETY: the offset is inside the arena, which this process mapped read-write, and the word is aligned.
        unsafe { core::ptr::write_volatile((self.base + offset) as *mut u32, value) };
    }

    fn peek32(&self, offset: usize) -> u32 {
        // SAFETY: the offset is inside the arena, which this process mapped read-write, and the word is aligned.
        unsafe { core::ptr::read_volatile((self.base + offset) as *const u32) }
    }

    #[cfg(target_arch = "aarch64")]
    fn poke64(&self, offset: usize, value: u64) {
        // SAFETY: the offset is inside the arena, which this process mapped read-write, and the word is aligned.
        unsafe { core::ptr::write_volatile((self.base + offset) as *mut u64, value) };
    }

    fn poke_bytes(&self, offset: usize, bytes: &[u8]) {
        // SAFETY: the destination is inside the arena (the caller keeps every write in the mapping), and the source
        // is a slice of this process, so the two do not overlap.
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                (self.base + offset) as *mut u8,
                bytes.len(),
            );
        };
    }

    fn peek_bytes(&self, offset: usize, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        // SAFETY: the source range is inside the arena, which this process mapped read-write; the copy is into a
        // fresh vector of the same length.
        unsafe {
            core::ptr::copy_nonoverlapping((self.base + offset) as *const u8, out.as_mut_ptr(), len)
        };
        out
    }
}

/// How a run ended: the thunk returned to its caller, or it was still running when the bound expired.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Returned,
    Spun,
}

/// Runs the thunk at `entry` in a child, with a bound. A child that returns exits with status zero; one still
/// running at the bound is killed (it spun).
fn run_thunk(entry: usize) -> Outcome {
    // SAFETY: fork has no arguments; the child only calls the thunk and `_exit`, which allocate nothing.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed");
    if pid == 0 {
        // SAFETY: the transmute reads the address the caller wrote as the thunk's entry, a 4-byte-aligned address in
        // the arena's executable mapping, and the call is the FEL loop's entry in miniature: the thunk returns here.
        let thunk = unsafe { core::mem::transmute::<usize, extern "C" fn()>(entry) };
        // The thunk's contract is a return through the saved link register, or no return at all.
        thunk();
        // SAFETY: `_exit` ends the child without running the test harness's teardown, which must not run twice.
        unsafe { libc::_exit(0) };
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut status: libc::c_int = 0;
        // SAFETY: waits on our own child (pid) for its status; the status pointer is a local of this process.
        let done = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if done == pid {
            assert!(
                libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
                "the child ended with status {status:#x}"
            );
            return Outcome::Returned;
        }
        if Instant::now() >= deadline {
            // SAFETY: kills our own child, which is still running at the bound; this is the only kill in the harness.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            // SAFETY: reaps the child killed above, so no zombie is left behind.
            unsafe { libc::waitpid(pid, &mut status, 0) };
            return Outcome::Spun;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The stand-in SPL: it stores the watchdog word it finds into `SEEN_WDT_AT`, and the first word of the first
/// buffer into `SEEN_BUF_AT`, then returns. Words, not source, so the test does not depend on an assembler.
#[cfg(target_arch = "arm")]
fn write_spl(arena: &Arena) {
    // ldr r1,[pc,#0x1c]; ldr r2,[r1]; ldr r3,[pc,#0x18]; str r2,[r3];
    // ldr r1,[pc,#0x14]; ldr r2,[r1]; ldr r3,[pc,#0x10]; str r2,[r3]; bx lr
    let code: [u32; 9] = [
        0xe59f101c, 0xe5912000, 0xe59f3018, 0xe5832000, 0xe59f1014, 0xe5912000, 0xe59f3010,
        0xe5832000, 0xe12fff1e,
    ];
    for (i, word) in code.iter().enumerate() {
        arena.poke32(SPL_AT + 4 * i, *word);
    }
    // The literal pool: the watchdog, the seen cell, the first buffer, the seen-buffer cell.
    arena.poke32(SPL_AT + 36, arena.at(WDT_AT) as u32);
    arena.poke32(SPL_AT + 40, arena.at(SEEN_WDT_AT) as u32);
    arena.poke32(SPL_AT + 44, arena.at(BUF_AT) as u32);
    arena.poke32(SPL_AT + 48, arena.at(SEEN_BUF_AT) as u32);
}

/// The stand-in SPL for AArch64: the same steps, with 64-bit literals and 32-bit loads and stores.
#[cfg(target_arch = "aarch64")]
fn write_spl(arena: &Arena) {
    // ldr x1,[lit40]; ldr w2,[x1]; ldr x3,[lit48]; str w2,[x3];
    // ldr x1,[lit56]; ldr w2,[x1]; ldr x3,[lit64]; str w2,[x3]; ret
    let code: [u32; 9] = [
        0x58000141, 0xb9400022, 0x58000143, 0xb9000062, 0x58000141, 0xb9400022, 0x58000143,
        0xb9000062, 0xd65f03c0,
    ];
    for (i, word) in code.iter().enumerate() {
        arena.poke32(SPL_AT + 4 * i, *word);
    }
    arena.poke64(SPL_AT + 40, arena.at(WDT_AT) as u64);
    arena.poke64(SPL_AT + 48, arena.at(SEEN_WDT_AT) as u64);
    arena.poke64(SPL_AT + 56, arena.at(BUF_AT) as u64);
    arena.poke64(SPL_AT + 64, arena.at(SEEN_BUF_AT) as u64);
}

/// The return-only entry: one instruction that returns to the thunk.
#[cfg(target_arch = "arm")]
fn write_probe(arena: &Arena) {
    arena.poke32(PROBE_AT, 0xe12f_ff1e); // bx lr
}

#[cfg(target_arch = "aarch64")]
fn write_probe(arena: &Arena) {
    arena.poke32(PROBE_AT, 0xd65f_03c0); // ret
}

/// Lays out one run: the buffers, the cells, the stand-ins, and the blob written by `blob::write`. `patch` edits
/// the written blob before it is copied in, so a test can make a table the writer would refuse.
fn install(arena: &Arena, entry_at: usize, patch: impl FnOnce(&mut [u8])) {
    for i in 0..SWAP_LEN / 4 {
        arena.poke32(BUF_AT + 4 * i, BUF_PATTERN | i as u32);
        arena.poke32(BACKUP_AT + 4 * i, BACKUP_PATTERN | i as u32);
    }
    arena.poke32(WDT_AT, 0);
    arena.poke32(SEEN_WDT_AT, SENTINEL);
    arena.poke32(SEEN_BUF_AT, SENTINEL);
    write_spl(arena);
    write_probe(arena);

    let fields = Fields {
        entry: entry_at as u32,
        watchdog: Watchdog {
            address: arena.at(WDT_AT) as u32,
            arm: ARM_VALUE,
            disarm: DISARM_VALUE,
        },
        swaps: &[Swap {
            buf1: arena.at(BUF_AT) as u32,
            buf2: arena.at(BACKUP_AT) as u32,
            size: SWAP_LEN as u32,
        }],
    };
    let mut bytes = vec![0u8; LAYOUT.blob_len];
    blob::write(&LAYOUT, &fields, &mut bytes).expect("the blob writer accepts the test fields");
    patch(&mut bytes);
    arena.poke_bytes(BLOB_AT, &bytes);
}

/// The table's entry `index` as the layout places it, for a test that edits the written table.
fn table_entry(index: usize) -> usize {
    LAYOUT.table_offset + index * 12
}

fn spl_entry(arena: &Arena) -> usize {
    arena.at(SPL_AT)
}

fn probe_entry(arena: &Arena) -> usize {
    arena.at(PROBE_AT)
}

fn words_of(arena: &Arena, offset: usize) -> u32 {
    arena.peek32(offset)
}

/// The blob's code is the pinned words, so the bytes the test runs are the bytes the build produces.
#[test]
fn the_harness_runs_the_pinned_words() {
    let pinned: Vec<u8> = PINNED.iter().flat_map(|word| word.to_le_bytes()).collect();
    assert_eq!(pinned.len(), LAYOUT.blob_len);
    let arena = Arena::new(0);
    install(&arena, spl_entry(&arena), |_| {});
    let code = arena.peek_bytes(BLOB_AT, LAYOUT.blob_len);
    // The data words are written by the fields; the code before the table is exactly the pinned code.
    assert_eq!(&code[..LAYOUT.table_offset], &pinned[..LAYOUT.table_offset]);
}

/// Armed before the SPL, disarmed after it, and the SPL runs.
#[test]
fn the_thunk_arms_the_watchdog_before_the_spl_and_disarms_it_after() {
    let arena = Arena::new(1);
    install(&arena, spl_entry(&arena), |_| {});
    assert_eq!(run_thunk(arena.at(BLOB_AT)), Outcome::Returned);
    assert_eq!(
        words_of(&arena, SEEN_WDT_AT),
        ARM_VALUE,
        "the SPL found the watchdog unarmed"
    );
    assert_eq!(
        words_of(&arena, WDT_AT),
        DISARM_VALUE,
        "the watchdog was not disarmed before the return"
    );
}

/// The buffers are swapped while the SPL runs, and swapped back by the time the thunk returns.
#[test]
fn the_thunk_returns_with_the_buffers_swapped_back() {
    let arena = Arena::new(2);
    install(&arena, spl_entry(&arena), |_| {});
    assert_eq!(run_thunk(arena.at(BLOB_AT)), Outcome::Returned);
    assert_eq!(
        words_of(&arena, SEEN_BUF_AT),
        BACKUP_PATTERN,
        "the SPL saw the stack buffer, not its backup"
    );
    for i in 0..SWAP_LEN / 4 {
        assert_eq!(
            words_of(&arena, BUF_AT + 4 * i),
            BUF_PATTERN | i as u32,
            "stack word {i} after the return"
        );
        assert_eq!(
            words_of(&arena, BACKUP_AT + 4 * i),
            BACKUP_PATTERN | i as u32,
            "backup word {i} after the return"
        );
    }
}

/// A return-only entry proves the thunk returns to its caller, with the swaps undone and the watchdog disarmed.
/// This is the probe a PC side runs before any write of a real SPL.
#[test]
fn a_return_only_entry_proves_the_thunk_returns() {
    let arena = Arena::new(3);
    install(&arena, probe_entry(&arena), |_| {});
    assert_eq!(run_thunk(arena.at(BLOB_AT)), Outcome::Returned);
    assert_eq!(words_of(&arena, WDT_AT), DISARM_VALUE);
    assert_eq!(words_of(&arena, BUF_AT), BUF_PATTERN);
    assert_eq!(words_of(&arena, BACKUP_AT), BACKUP_PATTERN);
}

/// A table size that is not a whole number of words stops the thunk before any swap or SPL call. It spins with
/// the watchdog armed, so the watchdog is the exit.
#[test]
fn a_size_that_is_not_a_whole_number_of_words_spins_before_any_swap() {
    let arena = Arena::new(4);
    install(&arena, spl_entry(&arena), |blob| {
        blob[table_entry(0) + 8..table_entry(0) + 12].copy_from_slice(&6u32.to_le_bytes());
    });
    assert_eq!(run_thunk(arena.at(BLOB_AT)), Outcome::Spun);
    assert_eq!(
        words_of(&arena, SEEN_WDT_AT),
        SENTINEL,
        "the SPL ran after a failed check"
    );
    assert_eq!(
        words_of(&arena, WDT_AT),
        ARM_VALUE,
        "the watchdog was disarmed on a failed check"
    );
    assert_eq!(
        words_of(&arena, BUF_AT),
        BUF_PATTERN,
        "a buffer was swapped on a failed check"
    );
}

/// A table with no terminator (eight live entries) stops the thunk before the SPL, with the watchdog armed.
#[test]
fn a_table_with_no_terminator_spins_before_the_spl() {
    let arena = Arena::new(5);
    install(&arena, spl_entry(&arena), |blob| {
        // Entry 0 is the swap `install` wrote. Make entries 1 to 7 live too, so no entry is a terminator.
        for index in 1..8 {
            let at = table_entry(index);
            blob[at..at + 4].copy_from_slice(&(arena.at(BUF_AT) as u32).to_le_bytes());
            blob[at + 4..at + 8].copy_from_slice(&(arena.at(BACKUP_AT) as u32).to_le_bytes());
            blob[at + 8..at + 12].copy_from_slice(&(SWAP_LEN as u32).to_le_bytes());
        }
    });
    assert_eq!(run_thunk(arena.at(BLOB_AT)), Outcome::Spun);
    assert_eq!(
        words_of(&arena, SEEN_WDT_AT),
        SENTINEL,
        "the SPL ran with no terminator in the table"
    );
    assert_eq!(words_of(&arena, WDT_AT), ARM_VALUE);
}
