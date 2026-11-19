// SPDX-License-Identifier: GPL-2.0
/*
 * The test fixtures' only C: one-line shims for static inline and macro kernel APIs with no exported
 * symbol and no rust_helper_* (7.3.5), of the same form as kernel/lemnos_helpers.c, and never part of
 * lemnos.ko. Each is one line, commented with the API it wraps and why there is no Rust path.
 * scripts/check-kernel-helpers.py checks that this file holds nothing else and stays within its cap.
 */
#include <linux/io.h>
#include <linux/irqflags.h>
#include <linux/mm.h>

/* The Rust side declares the same functions (fixture.rs); these are their C prototypes. */
void *lemnos_test_page_address(const struct page *page);
phys_addr_t lemnos_test_page_to_phys(struct page *page);
void *lemnos_test_phys_to_virt(phys_addr_t at);
int lemnos_test_pfn_valid(unsigned long pfn);
unsigned long lemnos_test_local_irq_save(void);
void lemnos_test_local_irq_restore(unsigned long flags);
int lemnos_test_page_refcount(unsigned long pfn);

/* page_address: a macro over page_to_virt, which each architecture builds from its own memory map; no export, no rust_helper. */
void *lemnos_test_page_address(const struct page *page) { return page_address(page); }
/* page_to_phys: a macro over page_to_pfn, which the memory model (vmemmap here) defines; no export, no rust_helper. */
phys_addr_t lemnos_test_page_to_phys(struct page *page) { return page_to_phys(page); }
/* phys_to_virt: static inline over __va, the architecture's direct map base; no export, no rust_helper. */
void *lemnos_test_phys_to_virt(phys_addr_t at) { return phys_to_virt(at); }
/* pfn_valid: static inline over the memory model's sections (include/linux/mmzone.h); no export, no rust_helper. */
int lemnos_test_pfn_valid(unsigned long pfn) { return pfn_valid(pfn); }
/* local_irq_save: a macro over arch_local_irq_save (inline asm, no export); the flags it saves are the result. */
unsigned long lemnos_test_local_irq_save(void) { unsigned long flags; local_irq_save(flags); return flags; }
/* local_irq_restore: a macro over arch_local_irq_restore (inline asm, no export); the Rust token carries no flags. */
void lemnos_test_local_irq_restore(unsigned long flags) { local_irq_restore(flags); }
/* page_ref_count: static inline over the page's refcount; pfn_to_page is the memory model's macro (vmemmap). No export, no rust_helper. */
int lemnos_test_page_refcount(unsigned long pfn) { return page_ref_count(pfn_to_page(pfn)); }
