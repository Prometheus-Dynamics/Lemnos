// SPDX-License-Identifier: GPL-2.0
/*
 * The module's only C: one-line shims for static inline kernel APIs that have no exported symbol
 * and no rust_helper_* in the kernel (7.3.3, rust/exports.c). Each shim is one line, is commented
 * with the API it wraps, and says why no Rust path exists. scripts/check-kernel-helpers.sh checks
 * that this file holds nothing else and stays within its size cap.
 *
 * Stage 0 needs none: the skeleton calls no inline API. The classification of the 67 true gaps is in
 * inline-gaps.txt next to this file.
 */
#include <linux/module.h>
