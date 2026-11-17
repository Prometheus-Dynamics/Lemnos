# The Allwinner FEL thunk

The thunk is the routine the boot ROM's FEL loop runs with FEL_RUN: it arms the watchdog first, saves the ROM's
registers, swaps each table buffer with its backup, calls the SPL, swaps the buffers back, disarms the watchdog and
returns into the ROM's FEL loop. The layout, the data words and the writer are in `lemnos-allwinner-fel-blob`, whose
README states the rules.

- `armv7a-none-eabi` (ARM state): the 32-bit thunk, one naked function.
- `aarch64-unknown-none`: the AArch64 thunk, one naked function. Its FEL behaviour is not confirmed (see the blob's
  README), so it is **untested on board**.

Each thunk is `#[unsafe(naked)]`, its body is only `naked_asm!`, and its labels are local, so the assembler resolves
every address inside the section. A relocation into the section would leave a placeholder where the linker writes an
address, so `scripts/check-blob.sh` refuses one.

## Checks

`scripts/check-blob.sh` runs three checks, and a missing tool fails it:

1. The build: each target's thunk is built (armv7 with `core` built from source under `RUSTC_BOOTSTRAP=1`, no
   download), its section is copied from the object, and its bytes must equal the pinned words in
   `lemnos-allwinner-fel-blob`. Each data symbol must sit at its layout offset.
2. The harness (`src/harness.rs`): the pinned bytes run for armv7 and aarch64 under user-mode emulation
   (`qemu-arm-static`, `qemu-aarch64-static`), each run in a forked child with a bound. The harness checks that the
   watchdog is armed before the SPL and disarmed after it, that the buffers are swapped during the SPL and restored
   after it, that a return-only entry returns, and that a bad size and a table with no terminator spin with the
   watchdog armed and nothing swapped. Six tests per target.
3. `--emit` prints the built words in the `pinned.rs` format, for re-pinning after an assembly change.

The emulation runs link with `scripts/emu-link.sh`: clang driving the toolchain's `rust-lld`, with no C library of its
own (Rust supplies its self-contained musl set). The harness is a test for the two Linux ARM targets only, so
`cargo test` on a host does not run it; `check-blob.sh` is what runs it.

The emulation shows what a user-mode run can show: the thunk's logic and its bounds. It does not show the SoC's
watchdog, the FEL loop, the SRAM or the boot ROM. Those are checked on a board (`testing/board-runs/FEL.md`).
