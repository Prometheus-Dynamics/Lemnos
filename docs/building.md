# Building the shipped programs

The programs an OS image ships are `lemnosd`, `lemnos-ctl` and `lemnos-orion`. They are built from
the workspace with `--locked --offline` from vendored sources, so the build can be planned and run
without the network. This page records what each one needs and which targets it builds for.

## The lock file

`Cargo.lock` is tracked. A change to any `Cargo.toml` changes it in the same commit. The gate's
first step, `cargo metadata --locked`, fails when the lock does not match the manifests, and says
what to run:

```text
cargo metadata --format-version 1 >/dev/null
```

Then commit the `Cargo.lock` that run changed. The run keeps every version already locked and adds
only what the manifests newly need. It does not upgrade anything else.

Every other cargo command in the gate is run after that check, and none of them rewrite a lock
that matches the manifests.

## Vendored, offline build

The OS build vendors the dependencies once, then builds against them:

```text
cargo vendor --locked vendor/ > .cargo/config.toml   # append to any existing config instead
cargo build --locked --offline -p lemnosd -p lemnos-ctl
```

`cargo vendor` also vendors the Orion git dependency of `service/orion`, pinned by `rev` in its
manifest. Nothing else is fetched.

## Shipped programs

| Binary (`-p` package)          | Binary name     | Features needed                       | Built for musl targets                                                  |
|--------------------------------|-----------------|---------------------------------------|-------------------------------------------------------------------------|
| `lemnosd` (`lemnosd`)          | `lemnosd`       | none (`fake-devices` is for the simulated board only) | `aarch64-unknown-linux-musl` yes; `armv7-unknown-linux-musleabihf` yes with rust-lld, see below; `riscv64gc-unknown-linux-musl` no |
| `lemnos-ctl` (`lemnos-ctl`)    | `lemnos-ctl`    | none                                  | same as `lemnosd`                                                       |
| `lemnos-orion` (`lemnos-orion`)| `lemnos-orion`  | none (Orion is pinned by its git `rev`) | same as `lemnosd`                                                       |
| `lemnos-slot-init` (`service/slot-init`) | `lemnos-slot-init` | none | planned: not merged, not built                                    |

What was run, on the M5 branch (`cargo build --locked -p <package> --target <target>`, dev profile):

- `aarch64-unknown-linux-musl`: all three packages build. The repo's `.cargo/config.toml` selects
  `rust-lld` with self-contained musl for this target, so no C toolchain is needed.
- `armv7-unknown-linux-musleabihf`: plain builds fail at link time, because the default linker is
  the host `cc`, which cannot link 32-bit ARM. All three packages build when the linker is
  overridden with `CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_LINKER=rust-lld` and
  `CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_RUSTFLAGS="-C link-self-contained=yes"`. The repo has
  no config entry for this target yet.
- `riscv64gc-unknown-linux-musl`: plain builds fail at link time for the same reason as armv7. With
  `rust-lld` the link fails with `unable to find library -lgcc_s`, because the host has no cross
  libgcc for this target. Needs a cross toolchain or a libgcc_s for riscv64 musl before this target
  can be shipped.

The host target builds `lemnosd` and `lemnos-ctl` with `--locked --offline` from a vendored copy.

Not run: the glibc targets; the `service` profile (fat LTO) for any target; `lemnos-slot-init`.
