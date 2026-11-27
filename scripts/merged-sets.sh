# shellcheck shell=bash disable=SC2034 # the lists are read by the scripts that source this file
# Sourced by scripts/gate.sh, scripts/check-libraries.sh and scripts/check-merged-sets.sh. The tail lane
# checks each crate set below in one cargo call per target, or one clippy and one test call, instead of
# one call per crate. One call unifies features across the crates it names, so a crate whose features
# change when it is built beside the others is built alone instead (the *_alone lists).
# scripts/check-merged-sets.sh compares every crate in a merged list against the same crate built alone
# and fails, naming the crate, if any unit's feature set differs.

# What must still build without std, on the targets a microcontroller host uses.
no_std_targets=(thumbv7em-none-eabihf riscv32imac-unknown-none-elf armv7-unknown-linux-gnueabihf)
no_std_merged=(lemnos-abi lemnos-contract-accelerometer lemnos-contract-gyroscope lemnos-contract-magnetometer
  lemnos-contract-orientation lemnos-driver-orientation lemnos-host-static lemnos-mcu lemnos-driver-usb-gadget
  lemnos-driver-generic-sensor shm-sample-time)
# Beside the others, lemnos-core and lemnos-hal get their `default` feature.
no_std_alone=(lemnos-core lemnos-hal)

# What must build on the desktop targets: the crates meant to be portable, checked for Windows and macOS.
# Linux-only crates stay off this list.
desktop_targets=(x86_64-pc-windows-msvc aarch64-apple-darwin x86_64-apple-darwin)
portable_crates=(shm-ring shm-ring-time shm-sample-time)

# The crates other programs take as plain libraries (scripts/check-libraries.sh), each with its default
# features off. The shared-memory ring family (docs/ring.md) is in it: standalone, so no Lemnos crate at all.
libraries_merged=(
  lemnos-v4l2
  lemnos-driver-pisp lemnos-driver-pisp-sys lemnos-driver-pisp-fe-sys
  lemnos-driver-rkvenc
  lemnos-driver-rkisp lemnos-driver-rkisp-sys
  lemnos-driver-k230isp lemnos-driver-k230isp-sys
  lemnos-driver-isp600 lemnos-driver-isp600-sys
  lemnos-linux-hotplug
  lemnos-linux-stream
  shm-ring shm-ring-latest shm-ring-time shm-ring-doorbell shm-ring-buffers shm-ring-holds shm-fdpass
)
# Beside the others: lemnos-driver-rk-mpp gets default, device and played; lemnos-wire gets default and
# protocol; shm-sample-time gets default; lemnos-driver-rkjpege's lemnos-hal gets mock; lemnos-identity's
# syn gets full.
libraries_alone=(lemnos-driver-rk-mpp lemnos-driver-rkjpege lemnos-identity lemnos-wire shm-sample-time)
