// SPDX-License-Identifier: GPL-2.0
/*
 * The Lemnos module's only C: one-line shims for static inline kernel APIs with no exported symbol and
 * no rust_helper_* (7.3.3, rust/exports.c). Each is one line, commented with the API it wraps and why
 * there is no Rust path. scripts/check-kernel-helpers.py checks that this file holds nothing else and
 * stays within its size cap.
 */
#include <linux/device/bus.h>
#include <linux/fs.h>
#include <linux/mm.h>
#include <linux/pwm.h>
#include <linux/shmem_fs.h>
#include <linux/vmalloc.h>

/* The Rust side declares the same two functions (lemnos_pwm_rs.rs); these are their C prototypes. */
struct device *lemnos_bus_find_device_by_name(const struct bus_type *bus, const char *name);
void lemnos_pwm_get_state(const struct pwm_device *pwm, struct pwm_state *state);
/* The frame stream's (rs/stream_reader.rs): a reader's marks page and the consumer's doorbell page. */
loff_t lemnos_i_size_read(const struct inode *inode);
unsigned int lemnos_shmem_seals(struct inode *inode);
struct page *lemnos_shmem_read_mapping_page(struct address_space *mapping, pgoff_t index);
void *lemnos_vmap_ro(struct page **pages, unsigned int count);
void lemnos_put_page(struct page *page);
/* The core's register mapping (rs/dev.rs). */
void lemnos_vm_flags_set(struct vm_area_struct *vma, vm_flags_t flags);
void lemnos_vma_noncached(struct vm_area_struct *vma);
int lemnos_io_remap_pfn_range(struct vm_area_struct *vma, unsigned long pfn, unsigned long size);

/* bus_find_device_by_name: static inline (include/linux/device/bus.h), no export; the Rust side calls it through this shim. */
struct device *lemnos_bus_find_device_by_name(const struct bus_type *bus, const char *name) { return bus_find_device_by_name(bus, NULL, name); }
/* pwm_get_state: static inline (include/linux/pwm.h:129), reads the device's cached state; no export, no rust_helper. */
void lemnos_pwm_get_state(const struct pwm_device *pwm, struct pwm_state *state) { pwm_get_state(pwm, state); }
/* i_size_read: static inline (include/linux/fs.h), a seqcount read where a long is 32 bits; no export, no rust_helper. */
loff_t lemnos_i_size_read(const struct inode *inode) { return i_size_read(inode); }
/* SHMEM_I: static inline container_of (include/linux/shmem_fs.h) to a struct the kernel crate's bindings leave out; reads the seals. */
unsigned int lemnos_shmem_seals(struct inode *inode) { return READ_ONCE(SHMEM_I(inode)->seals); }
/* shmem_read_mapping_page: static inline (include/linux/shmem_fs.h) over mapping_gfp_mask, itself inline; no export, no rust_helper. */
struct page *lemnos_shmem_read_mapping_page(struct address_space *mapping, pgoff_t index) { return shmem_read_mapping_page(mapping, index); }
/* PAGE_KERNEL_RO: a macro each arch builds from masks set at boot, so vmap's read-only protection has no value Rust can name. */
void *lemnos_vmap_ro(struct page **pages, unsigned int count) { return vmap(pages, count, VM_MAP, PAGE_KERNEL_RO); }
/* put_page: static inline (include/linux/mm.h) over the folio's reference count; no export, no rust_helper. */
void lemnos_put_page(struct page *page) { put_page(page); }
/* vm_flags_set: static inline (include/linux/mm.h), takes the VMA write lock through other inlines; no export, no rust_helper. */
void lemnos_vm_flags_set(struct vm_area_struct *vma, vm_flags_t flags) { vm_flags_set(vma, flags); }
/* pgprot_noncached: a macro each architecture defines its own way (asm/pgtable.h); Rust cannot expand it. */
void lemnos_vma_noncached(struct vm_area_struct *vma) { vma->vm_page_prot = pgprot_noncached(vma->vm_page_prot); }
/* io_remap_pfn_range: static inline (include/linux/mm.h) over an architecture hook and pgprot_decrypted; no export, no rust_helper. */
int lemnos_io_remap_pfn_range(struct vm_area_struct *vma, unsigned long pfn, unsigned long size) { return io_remap_pfn_range(vma, vma->vm_start, pfn, size, vma->vm_page_prot); }
