// SPDX-License-Identifier: GPL-2.0
/*
 * The test fixtures' only C: one-line shims for static inline and macro kernel APIs with no exported
 * symbol and no rust_helper_* (7.3.5), of the same form as kernel/lemnos_helpers.c, and never part of
 * lemnos.ko. Each is one line, commented with the API it wraps and why there is no Rust path.
 * scripts/check-kernel-helpers.py checks that this file holds nothing else and stays within its cap.
 */
#include <linux/io.h>
#include <linux/mm.h>

/* The Rust side declares the same functions (fixture.rs); these are their C prototypes. */
void *lemnos_test_page_address(const struct page *page);
phys_addr_t lemnos_test_page_to_phys(struct page *page);
void *lemnos_test_phys_to_virt(phys_addr_t at);
int lemnos_test_pfn_valid(unsigned long pfn);

/* page_address: a macro over page_to_virt, which each architecture builds from its own memory map; no export, no rust_helper. */
void *lemnos_test_page_address(const struct page *page) { return page_address(page); }
/* page_to_phys: a macro over page_to_pfn, which the memory model (vmemmap here) defines; no export, no rust_helper. */
phys_addr_t lemnos_test_page_to_phys(struct page *page) { return page_to_phys(page); }
/* phys_to_virt: static inline over __va, the architecture's direct map base; no export, no rust_helper. */
void *lemnos_test_phys_to_virt(phys_addr_t at) { return phys_to_virt(at); }
/* pfn_valid: static inline over the memory model's sections (include/linux/mmzone.h); no export, no rust_helper. */
int lemnos_test_pfn_valid(unsigned long pfn) { return pfn_valid(pfn); }
