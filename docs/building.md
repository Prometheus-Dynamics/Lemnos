# Building the shipped programs

The programs an OS image ships are listed once, in `packaging/shipped.txt`: `lemnosd`, `lemnos-ctl`
and `lemnos-orion`. The files an image installs from `packaging/` (the units, rules, scripts and drop-ins) are
listed in `packaging/files.txt`, which is the one place a packager reads for them; see
[packaging/README.md](../packaging/README.md). `scripts/check-shipped.sh` reads `shipped.txt`, and this page lists each program in its
table. The image builder builds them `--locked --offline` from vendored sources, with the
`service` profile, for each target below. The kernel module is built separately:
[kernel-module.md](kernel-module.md), "For an image builder", and the symbols a board kernel needs, in
[kernel-config.md](kernel-config.md).

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
