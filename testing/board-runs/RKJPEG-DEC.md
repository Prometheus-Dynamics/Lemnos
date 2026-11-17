# RKJPEG-DEC board run: the JPEG decoder's chroma placement, table units and reach

Status: untested on board. The probe, the board runner and their PC tests pass
(`drivers/rk-jpeg/src/probe.rs`, `drivers/rk-jpeg/tests/decoder_probe.rs`). The board run is
not started. It is on the blocked list: the coordinator holds board access.

## What is proven on the PC

- `cargo test -p lemnos-driver-rk-jpeg`: 20 lib tests pass; `decoder_probe` runs 13 tests and
  passes. Two are `#[ignore]`d: the board probe, and the fixture writer that produces the file.
- The fixture generator's output parses back to the stated layout and lengths. Fail-before: setting
  `TARGETS` to `[64, 32, 64]` fails `the_stated_targets_share_one_field_value_under_four_byte_words`.
- Against the stand-in `Model`, the probe finds each chroma candidate, each table-unit triple,
  and each candidate's reach. A lenient stand-in is reported `Unbounded`. A layout no candidate
  predicts matches none. An empty observed set matches several triples, so it is reported
  ambiguous, not as a unique answer.
- The board runner's job path runs through `rk-mpp`'s claim and job API against the played kernel
  (`lemnos_driver_rk_mpp::played::Kernel`). It translates the five address registers, puts the
  stream length in `reg8`, and leaves the output buffer as the played kernel left it.
- Fail-before: swapping the `REG_HMIN` and `REG_HVAL` address registers in `Board::run` fails
  `the_board_runner_places_every_address_through_the_claim` with the register values printed
  (`left: 272631808`, `right: 272630784`). The file was restored afterwards.
- `cargo fmt -- --check` and `cargo clippy -p lemnos-driver-rk-jpeg --all-targets -- -D warnings`
  pass. `scripts/check-docs.py` reports "docs hold" and `scripts/check-boundaries.sh` reports
  "boundaries hold".

## Provenance of the register facts

- Register indices and bit positions (`REG_SYS`, `YUV_FMT_SHIFT`, `REG_PIC`, `REG_VSTRIDE`,
  `REG_TBL`, `REG_STRM_LEN`, `REG_QTBL`, `REG_HMIN`, `REG_HVAL`, `REG_STRM`, `REG_OUT`, and the
  `QTBL`/`HMIN`/`HVAL` field masks): rockchip-linux/mpp `9b56348`,
  `mpp/hal/vpu/jpegd/hal_jpegd_rkv_reg.h`, Apache-2.0. These were read through a summary of that
  file, not a raw copy. Before the run, open the raw header at that commit and check every value
  named above. If one differs, stop and record it here.
- Register-file size, status bits and the start word: `drivers/rk-jpeg/src/regs.rs`, from
  rockchip-linux/kernel `develop-6.1` at `77168c8d5ab82399f65a80e9f807b50ba37cf483`.
- The candidate chroma placements and table units are hypotheses. The run decides between them.
  A board answer that matches none is the finding. Record it as such, do not force a row.

## The probe

Three checks, run in this order because each depends on the one before:

1. **Joint table sweep** (`probe_joint`): for each `yuv_out_format` code `0..=7`, the value `k`
   in `0..=31` is put in all three table-length fields. The accepted set goes to
   `match_units(accepted, targets)`. `targets` are the fixture's own table lengths in bytes
   (`qtbl`, `mincode`, `value`), passed as `RKJPEG_TARGETS`. One unit triple matching is the
   answer. Several matching is ambiguous. None matching is a candidate the probe does not know.
   The fixture must make the three tables' lengths reachable with one common `k`, or the sweep
   accepts nothing. If the fixture cannot do that, record the set and stop.
2. **Format and chroma** (`probe_formats`, `probe_chroma`): the format code that completes, and
   the written mask on a sentinel-filled 1 MiB output. Candidates on the probe's 64 x 50 picture
   with luma stride 80 (luma 4000 bytes, 4:2:0 chroma 2000 bytes):
   - `AlignedAfterLuma64`: chroma at 4032, ends at 6032;
   - `Adjacent`: chroma at 4000, ends at 6000;
   - `Aligned4096`: chroma at 4096, ends at 6096.
3. **Reach** (`probe_reach`): the smallest output the job completes in, by bisection. It must
   equal the chroma candidate's end. `Unbounded` (completes in an empty buffer) and `Failed`
   (not even in 64 KiB) are results to record.

The probe covers the three table-length fields with one joint sweep. It does not sweep one field
at a time. If the joint sweep is ambiguous, the next step is a per-field sweep with the other
fields held at the unit the joint sweep named.

## Board access (for the coordinator)

- Board: ROCK 5C (RK3588S), vendor kernel, `mpp_service` bound, decoder node
  `rockchip,rkv-jpeg-decoder-v1`. Reached over the network. The address and host key pin are the
  coordinator's to supply and are not recorded here.
- Lock: take `/run/lock/board-gpu.lock` on the board for the whole run.
- Build: on NVMe or SSD on the board, never on the Github HDD. The PC builds the test binary and
  copies it over.
- Input: the fixture file, in the layout `[qtable 0 | mincode 1024 | value 2048 | stream 4096]`
  (`probe::QTBL_OFF`, `probe::HMIN_OFF`, `probe::HVAL_OFF`, `probe::STREAM_OFF`). Its three table
  lengths are `RKJPEG_TARGETS=64,64,64`.
- Allocator: a DMA heap node, for `dma_heap::allocate` (`RKJPEG_HEAP`).

## The fixture

The fixture is written by the PC-side generator `drivers/rk-jpeg/tests/fixture/mod.rs`, from the
JPEG syntax, with no vendor data. The file is 4250 bytes: the 4096-byte table region, then a
154-byte baseline JPEG (64 by 50, one grey component, 8-bit, every pixel 128).

Generate it on the PC:

    RKJPEG_FIXTURE_OUT=<path> CARGO_TARGET_DIR=<target> cargo test -p lemnos-driver-rk-jpeg \
      --test decoder_probe write_board_fixture -- --ignored

The generator's output is checked by `the_board_input_holds_each_table_at_its_offset_and_the_stream_after_them`,
and the JPEG parses back by `the_fixture_jpeg_parses_back_to_the_picture_and_tables_it_declares`.
An independent decoder reads it too: PIL reads a 64 by 50 `L` image of 128 only, and
`identify` reports `JPEG 64x50 8-bit Grayscale`.

Each table is allotted 64 bytes. The mincode region holds the DC table's counts (16 bytes) and the
value region the DC symbol (1 byte), each zero-padded to 64 bytes so the three lengths share one
field value under 4-byte words: 16 for all three. `RKJPEG_TARGETS=64,64,64` is that length in
bytes. The padding is part of the fixture's stated length, not of the JPEG.

The file the PC built: `<H>/RKJPEG-DEC/rkjpeg-fixture.bin`, SHA-256 `afdbf21e299432fabb4f5e55a4cf1218bf5544cde80b7bb10d8d25f514ee7584`. Check the
board's copy against this digest before the run.

## The exact run

On the PC, build the test binary (not run here):

    CARGO_TARGET_DIR=<target> CARGO_PROFILE_TEST_DEBUG=0 cargo test -p lemnos-driver-rk-jpeg \
      --test decoder_probe --no-run

On the board, with the binary copied to a path `<bin>`, in a named systemd user unit
`RKJPEG-DEC-probe` with `-p AllowedCPUs=<the board's CPUs>`:

    RKJPEG_NODE=/dev/mpp_service \
    RKJPEG_HEAP=/dev/dma_heap/system \
    RKJPEG_FIXTURE=<path to rkjpeg-fixture.bin> \
    RKJPEG_TARGETS=64,64,64 \
    <bin> board_probe_on_the_service_node --ignored --nocapture

The test sweeps every format code with the joint table sweep, prints each code's accepted set
and unit matches, then takes the first code with one unit triple and runs the chroma and reach
probes on it. It prints these lines:

- `format <n>: accepted [...], units [...]` for each of the eight codes;
- `chroma at format <n>: ...` and `reach at format <n>: ...`.

It panics if no code gives one unit triple: record the sweep and stop there.

Record the raw output lines under Results.

## Not done

- Nothing has run on a block. The board run is blocked on the coordinator's board access.
- The register indices are not checked against the raw header (see Provenance).
- The board runner's job path is tested only against the played kernel, which does not produce
  the block's output. The block's real output is what the board run measures.

## Results

Not yet run.
