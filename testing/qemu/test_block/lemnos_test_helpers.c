// SPDX-License-Identifier: GPL-2.0
/*
 * The test fixtures' only C: shims for static inline and macro kernel APIs that the kernel's Rust
 * bindings can't reach (no exported symbol, no rust_helper_* in 7.3.5), never part of lemnos.ko. Each
 * shim is one call with its arguments passed through and no logic of its own, commented with the API
 * it wraps and why there is no Rust path. scripts/check-kernel-helpers.py enforces that form and a cap.
 */
#include <linux/io.h>
#include <linux/mm.h>

/* The Rust side declares the same functions (fixture.rs); these are their C prototypes. */
void *lemnos_test_page_address(const struct page *page);
phys_addr_t lemnos_test_page_to_phys(struct page *page);
void *lemnos_test_phys_to_virt(phys_addr_t at);
int lemnos_test_pfn_valid(unsigned long pfn);
struct page *lemnos_test_pfn_to_page(unsigned long pfn);

/* page_address: a macro over lowmem_page_address (static inline); exported only by mm/highmem.c with HIGHMEM, not on 64-bit; no rust_helper. */
void *lemnos_test_page_address(const struct page *page) { return page_address(page); }
/* page_to_phys: an architecture macro (asm/io.h) over page_to_pfn, the memory model's macro; no export, no rust_helper. */
phys_addr_t lemnos_test_page_to_phys(struct page *page) { return page_to_phys(page); }
/* phys_to_virt: static inline (asm/io.h) over __va, the architecture's direct map; no export, no rust_helper. */
void *lemnos_test_phys_to_virt(phys_addr_t at) { return phys_to_virt(at); }
/* pfn_valid: static inline over the sparse memory sections (linux/mmzone.h); exported only on arm and arc, no rust_helper. */
int lemnos_test_pfn_valid(unsigned long pfn) { return pfn_valid(pfn); }
/* pfn_to_page: the memory model's macro (asm-generic/memory_model.h, vmemmap here); no export, no rust_helper. */
struct page *lemnos_test_pfn_to_page(unsigned long pfn) { return pfn_to_page(pfn); }
