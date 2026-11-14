# Building the shipped programs

The programs an OS image ships are listed once, in `packaging/shipped.txt`: `lemnosd`, `lemnos-ctl`
and `lemnos-orion`. The files an image installs from `packaging/` (the units, rules, scripts and drop-ins) are
listed in `packaging/files.txt`, which is the one place a packager reads for them; see
[packaging/README.md](../packaging/README.md). `scripts/check-shipped.sh` reads `shipped.txt`, and this page lists each program in its
table. The image builder builds them `--locked --offline` from vendored sources, with the
`service` profile, for each target below. The kernel module is built separately:
[kernel-module.md](kernel-module.md), "For an image builder", and the symbols a board kernel needs, in
[kernel-config.md](kernel-config.md). The slot board's kernel requirements are the machine-read file
[contracts/slot-kernel.toml](../contracts/slot-kernel.toml), which an image builder reads from the tree
(the table in kernel-config.md is checked against it).

## The lock file

`Cargo.lock` is tracked. A change to any `Cargo.toml` changes it in the same commit. The gate's
first step, `cargo metadata --locked`, fails when the lock does not match the manifests, and says
what to run:

```text
cargo metadata --format-version 1 >/dev/null
```

Then commit the `Cargo.lock` that run changed. It keeps every version already locked and adds only
what the manifests newly need. Other cargo calls in the gate leave a consistent lock unchanged.

## Vendored, offline build

```text
cargo vendor --locked vendor/ > vendor-config.toml
# append vendor-config.toml to .cargo/config.toml (keep the repository's target settings)
cargo build --locked --offline --profile service --target <triple> -p <package>
```

`cargo vendor` also vendors the Orion git dependency of `service/orion`, pinned by `rev` in its
manifest.

## Targets

`.cargo/config.toml` holds one entry per musl target. Each links with `rust-lld` and Rust's
self-contained musl, with `+crt-static`, so no C cross toolchain or container is needed. The page
size (`-zmax-page-size`) is the alignment of the loadable segments, and it must not be larger than
the smallest page size the target's kernels use:

- `aarch64-unknown-linux-musl`: `0x10000` (64 KiB). arm64 kernels run with 4, 16 or 64 KiB pages,
  so the binary is aligned for 64 KiB to run on all of them.
- `armv7-unknown-linux-musleabihf`: `0x1000` (4 KiB). 32-bit ARM Linux has only 4 KiB pages.
- `riscv64gc-unknown-linux-musl`: `0x1000` (4 KiB). Mainline RISC-V Linux has only 4 KiB pages.

The builder adds to these settings; it does not replace them.

## Profile

The `service` profile (`Cargo.toml`) is the shipped profile: `opt-level = "z"`, fat LTO, one codegen
unit, `panic = "unwind"` (required: it is what lets the host contain a panicking driver), and
`strip = true`. The smallest reasonable footprint, not a fixed budget. On the same base, one codegen
unit was 1 to 2 percent smaller than the default 16 on aarch64 (lemnosd 2004048 to 1979760 bytes,
lemnos-ctl 699048 to 692992, lemnos-orion 789552 to 775912).

## Shipped programs

| Binary (`-p` package)           | Binary name       | Features needed                                               | Musl targets built (`--locked --offline`, `service`, rust-lld) |
|---------------------------------|-------------------|---------------------------------------------------------------|-----------------------------------------------------------------|
| `lemnosd` (`lemnosd`)           | `lemnosd`         | none (`fake-devices` is for the simulated board only)         | `aarch64-unknown-linux-musl`, `armv7-unknown-linux-musleabihf`, `riscv64gc-unknown-linux-musl` |
| `lemnos-ctl` (`lemnos-ctl`)     | `lemnos-ctl`      | none                                                          | the same three                                                  |
| `lemnos-orion` (`lemnos-orion`) | `lemnos-orion`    | none (Orion is pinned by its git `rev`)                       | the same three                                                  |
| `lemnos-slot-init` (`service/slot-init`) | `lemnos-slot-init` | none | the same three (the initramfs's `/init`, static)                |

Stripped sizes, from the vendored builds of this table (bytes):

| Binary          | aarch64 musl | armv7 musleabihf | riscv64 musl |
|-----------------|-------------:|-----------------:|-------------:|
| `lemnosd`       | 2018968      | 2021444          | 2017376      |
| `lemnos-ctl`    | 732632       | 714820           | 728928       |
| `lemnos-orion`  | 775368       | 768908           | 772048       |
| `lemnos-slot-init` | 491768   | 466144           | 483896       |

`lemnos-slot-init` was measured from a worktree build (`cargo build -p lemnos-slot-init --profile
service --target <triple>`, the repo's rust-lld config) on the I5c branch, which added the UBI
path, not from the vendored tree. It is plain `std` with no serde and no async.

The worktree build of the same commit gave `lemnos-ctl` 733224 and `lemnos-orion` 775912 on
aarch64: the same to within 0.1 percent. The sizes move with the base; these are measured on this
base only.

Verified: each of the nine (program, target) pairs built with `cargo build --locked --offline
--profile service --target <triple> -p <package>` from a `cargo vendor` tree, with a `PATH` where
`cc`, `gcc`, `g++`, `clang` and `ld` fail when run. The host build scripts were compiled first with
the host toolchain, because rustc needs the host linker for them; the target links did not use any C
tool. Each result is `stripped` and of the right word size (`file`).

Not run: the glibc targets; the vendored build of `lemnos-slot-init`; a board.

## No C in the shipped programs

`scripts/check-shipped.sh` (run by the gate) takes each program's normal and build dependency tree.
It fails if the tree holds a crate that compiles C or runs a C generator (`cc`, `cmake`, `bindgen`,
`pkg-config`, `vcpkg`, `cxx-build` and similar), or a `-sys` crate from outside the workspace that
is not on its allow list. The one allowed foreign `-sys` crate is `linux-raw-sys`: generated Rust
bindings to the Linux uapi, with no build script. It also fails if `docs/building.md` does not list a
shipped program. `engine/capi/build.rs` runs a C compiler, but it is the C library, not a shipped
program.

## Building `lemnos-ctl` as a host tool

`lemnos-ctl tree` runs on a build host and writes an image's device-tree overlay, boot files and udev
or mdev permission fragments from a board file. What follows was run on this PC at the I5i6 head
(`testing/board-runs/I5i6.md`, "The one invocation"); each claim names its run.

**One invocation with every output.** The reference board (`testing/boards/reference.toml`, `record =
"tryboot"`, `soc = "bcm2712-rp1"`), run at the I5i8 head:

```text
lemnos-ctl tree --socs <repo>/socs testing/boards/reference.toml -o overlay.dts \
  --modprobe modprobe.conf --cmdline cmdline.txt --udev udev.rules --mdev mdev.conf \
  --manifest-boot manifest-boot.json --cmdline-slot cmdline-slot.txt --slot a1
```

Exit 0; nothing refused. Files written: `overlay.dts` 2964 bytes, `modprobe.conf` 949, `cmdline.txt` 86,
`udev.rules` 1124, `mdev.conf` 1326, `manifest-boot.json` 110 and `cmdline-slot.txt` 92. `--uboot-config FILE`
writes the U-Boot Kconfig fragment the board's U-Boot is built with for a `record = "uboot"` board
([Boards](boards.md#u-boot-environment-system-slots-record--uboot)).

**The boot partition is one image.** `tree --boot-dir` is removed and the flag is refused (the message names
`boot-partition`). `lemnos-ctl boot-partition` writes everything it wrote, in the image, and the payload bays
besides (see [Slots](slots.md#the-boot-partition-payload-bays)):

```text
lemnos-ctl boot-partition --layout-of testing/boards/reference.toml --socs <repo>/socs --min-size
lemnos-ctl boot-partition --layout-of BOARD --socs <repo>/socs --slot a1 --payload DIR --size BYTES --out boot.img
lemnos-ctl boot-partition --layout-of BOARD --socs <repo>/socs --verify boot.img
```

The reference board's `--min-size` is 38242816 bytes (its made-up bays: 11 MiB kernel, 1 MiB initramfs). With
`--payload DIR` (five files named for the roles: `kernel`, `initramfs`, `device-tree`, `cmdline`,
`board-overlay`) it prints the same minimum first, then `role stored_bytes bay_bytes` per role. A `record = "tryboot"` image holds
`config.txt`, `tryboot.txt`, `default.txt`, `previous.txt`, `trial.txt` (two sectors each), `trial.rec`,
`reason.rec`, `episodes.rec`, `slot-layout.toml` and the three bay sets; a `record = "uboot"` image holds
`slot-layout.toml`, `uboot-env.txt`, `boot.cmd` (with the board's `lemnos_usb` and `lemnos_led` lines before
the script), `reason.rec` and the bay sets. The U-Boot environment partition is the disk image's, not this
image's.

**Output formats** (the three modes a build script reads):

- `--min-size` prints ONE line: the smallest `--size` in bytes, decimal digits and a newline, nothing
  else (`38242816`). Read it with `read n` or `int(line)`.
- `--min-size --payload DIR` prints that same line FIRST, then five lines `role stored_bytes bay_bytes`
  (three fields separated by one space; roles in the order `kernel`, `initramfs`, `device-tree`,
  `cmdline`, `board-overlay`; `stored_bytes` is the payload file's size, `bay_bytes` the board's bay).
  When a payload is larger than its bay the lines are printed and the exit is non-zero, with the error
  `the payload of <roles> is bigger than its bay: rebuild with a larger bay or a smaller payload`.
- `--out FILE` prints one line, `wrote FILE: N bytes, slot ID, minimum M bytes`.
- `--verify FILE` prints one line per finding, `ok  <what was checked>` or `BAD <what failed>` (the status
  word padded to three characters, then a space), reads the image or a dump of a partition and writes
  nothing. The exit is 0 when every line is `ok`; otherwise it is non-zero and the error is
  `FILE: N finding(s) failed`.

**A slot bundle** is written and verified by the same tool, after `tree` and `boot-partition` (the
sequence: `tree` writes the overlay, `--manifest-boot` and `--cmdline-slot`; `bundle` makes the slot's bundle and
the boot files with the root hash filled in; `boot-partition` takes those files as the first slot's payload):

```text
lemnos-ctl tree --socs <repo>/socs BOARD -o overlay.dts --cmdline-slot cmdline-slot.txt --slot a1
# payload/ holds kernel, initramfs, device-tree, cmdline (cmdline-slot.txt), board-overlay
lemnos-ctl bundle --layout-of BOARD --socs <repo>/socs --slot a1 --payload payload --rootfs rootfs.erofs \
  --verity-table verity.txt --product P --version V --key-file signing.seed --payload-out payload-final --out a1.bundle
lemnos-ctl bundle --layout-of BOARD --socs <repo>/socs --verify a1.bundle --pubkey signing.pub
lemnos-ctl boot-partition --layout-of BOARD --socs <repo>/socs --slot a1 --payload payload-final --size BYTES --out boot.img
```

Output of the first `bundle`: `slot ID: product P, version V, board B`, `root hash HEX`, `signed with key id HEX`
(or `unsigned`), `payload written to DIR` and `wrote FILE: N bytes, SHA-256 HEX`. Output of `--verify`: one line
per check, `ok `, `BAD` or `n/a ` then `check: text`, then `ok` or `refused: N` (exit 0 or 1). The format and
every refusal are in [Slots](slots.md#writing-one-with-lemnos-ctl).

A board file that cannot be built is refused naming the file, the device and the key, for example
`BOARD.toml: system-slots device "slots": uboot_env_size is needed for the "uboot" record (an integer)`.

`--bridge-env FILE` writes the bridge's USB link setting, the one line `LEMNOS_ORION_LINKS=<gadget device
id>=<link name>` (a comment only for a board with no gadget; [Boards](boards.md#serving-the-link-at-boot)). It was
not part of the run above: its golden test is `service/ctl/tests/bridge_env.rs`.

**The SoC map.** A board with `soc = NAME` reads `DIR/NAME.toml`, where `DIR` is `--socs DIR`, or the relative
directory `socs` in the working directory when `--socs` is absent (`load_map` in `service/ctl/src/lib.rs`).
Nothing is compiled in. `--map FILE` names one map file instead. For the reference board the map is
`socs/bcm2712-rp1.toml`; the run above used `--socs <repo>/socs`.

**The board overlay.** `-o` writes DTS source (not a blob); compile it with `dtc -@` (see
[boards.md](boards.md#the-board-overlay-file)). The run compiled it with `dtc -@ -I dts -O dtb` (exit 0).

**What building needs.** Checked with a fresh `CARGO_HOME` for each case:

- `cargo build -p lemnos-ctl --locked` online: exit 0. Cargo fetched the crates.io crates in `Cargo.lock` **and**
  the git checkout of `https://github.com/Prometheus-Dynamics/Orion.git` at rev `455a956e`, although `lemnos-ctl`
  does not use Orion (`cargo tree -p lemnos-ctl` shows no git package). `service/orion` declares those git
  dependencies, and the workspace member is part of the resolution.
- The same build offline with only the crates.io cache (no Orion checkout): fails with `can't checkout from
  'https://github.com/Prometheus-Dynamics/Orion.git': you are in the offline mode`. So the git source must be
  present, from the network or from a vendor directory.
- `cargo vendor --locked DIR` (run online) vendors the Orion sources too (`DIR/orion`, and the other Orion crates).
  Then `cargo build -p lemnos-ctl --locked --offline --config <the vendor config>` from an **empty** `CARGO_HOME`:
  exit 0. The vendor config is the output of `cargo vendor` (its `[source.*]` entries).
- At run time `lemnos-ctl tree` reads nothing but its arguments: the board file, the SoC map, and the outputs.
  The layout contract is embedded (`include_str!("../../../contracts/slot-layout.toml")` in `boot_dir.rs`), so
  the built binary needs no file from the repository.
