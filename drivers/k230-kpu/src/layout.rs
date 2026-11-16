//! The sizes, offsets and numbers of the kernel's declarations, checked at compile time. The
//! declarations are those of `drivers/misc/canaan/mmz/mmz.c` (ruyisdk/linux-xuantie-kernel at
//! `7d4e1f444f461dbe3833bd99a4640e7b6c2cd529`). They are the numbers the C compiler gave on riscv64
//! and on the host before that check was removed.

use core::mem::{offset_of, size_of};

use crate::uapi::{MMZ_ALLOC_MEM, MMZ_FREE_MEM, MmzInfo};

const _: () = {
    assert!(size_of::<MmzInfo>() == 32);
    assert!(offset_of!(MmzInfo, user_virt_addr) == 0);
    assert!(offset_of!(MmzInfo, kernel_virt_addr) == 8);
    assert!(offset_of!(MmzInfo, mmz_phys) == 16);
    assert!(offset_of!(MmzInfo, length) == 24);
    assert!(MMZ_ALLOC_MEM == 0xc008_6701);
    assert!(MMZ_FREE_MEM == 0xc008_6702);
};
