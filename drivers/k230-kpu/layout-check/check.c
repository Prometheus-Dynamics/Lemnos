/*
 * Checks the layouts and numbers in drivers/k230-kpu/src/layout.rs against the C compiler. Not
 * part of the crate build: run ./run.sh, which compiles this file for riscv64 (the K230 Linux
 * target) and for the host, and fails if any assertion does.
 *
 * struct mmz_info and the two requests are declared as drivers/misc/canaan/mmz/mmz.c declares
 * them (ruyisdk/linux-xuantie-kernel at 7d4e1f444f461dbe3833bd99a4640e7b6c2cd529); the request
 * encoding is the generic one from <asm-generic/ioctl.h>, which RISC-V uses.
 */
#include <stddef.h>
#include <asm-generic/ioctl.h>

struct mmz_info {
	void *user_virt_addr;
	void *kernel_virt_addr;
	unsigned long mmz_phys;
	unsigned long length;
};

#define MMZ_ALLOC_MEM _IOWR('g', 1, unsigned long)
#define MMZ_FREE_MEM _IOWR('g', 2, unsigned long)

_Static_assert(sizeof(struct mmz_info) == 32, "MmzInfo size");
_Static_assert(offsetof(struct mmz_info, user_virt_addr) == 0, "user_virt_addr");
_Static_assert(offsetof(struct mmz_info, kernel_virt_addr) == 8, "kernel_virt_addr");
_Static_assert(offsetof(struct mmz_info, mmz_phys) == 16, "mmz_phys");
_Static_assert(offsetof(struct mmz_info, length) == 24, "length");
_Static_assert(MMZ_ALLOC_MEM == 0xc0086701u, "MMZ_ALLOC_MEM");
_Static_assert(MMZ_FREE_MEM == 0xc0086702u, "MMZ_FREE_MEM");
