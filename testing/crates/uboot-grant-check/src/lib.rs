//! The disk the QEMU guest of `testing/uboot-grant/run.sh` boots: the boot partition (partition 1, a FAT
//! volume with slot `os-1` in bay 0, built by the image builder) and the U-Boot environment (partition 2,
//! one valid copy at the top of the partition). Shared by the host image builder and the guest.

use lemnos_slot_store::bays;
use lemnos_slot_store::bootimage::{self, Family, Parts};
use lemnos_slot_store::gpt;
use lemnos_slot_store::uboot_env::{self, Geometry};

/// The slot the boot partition holds, and the default the guest's daemon sets.
pub const SLOT: &str = "os-1";
/// The environment's size (`uboot_env_size`) and its partition's length in 512-byte blocks.
pub const ENV_SIZE: usize = 8192;
pub const ENV_BLOCKS: u64 = 128;
/// The first block of the boot partition: the usual 1 MiB alignment.
pub const BOOT_FIRST: u64 = 2048;

/// The boot partition's layout: the bays, the uboot form, as the image builder of slot-store makes it.
fn parts() -> Parts {
    Parts {
        family: Family::UbootBlock,
        layout: bays::Layout::new([8192, 4096, 4096, 4096, 4096])
            .unwrap()
            .with_form(bays::Form::Uboot),
        root: vec![("slot-layout.toml", b"version = 6\n".to_vec())],
    }
}

/// The boot partition's bytes: a FAT volume with [`SLOT`] installed in bay 0.
pub fn boot_partition() -> Result<Vec<u8>, String> {
    let parts = parts();
    let size = bootimage::min_size(&parts)?;
    let payloads: Vec<Vec<u8>> = ["kernel", "initramfs", "device tree", "console", "overlay"]
        .iter()
        .map(|what| format!("{what} of {SLOT}\n").into_bytes())
        .collect();
    let refs: Vec<&[u8]> = payloads.iter().map(Vec::as_slice).collect();
    bootimage::build(&parts, size, SLOT, &refs)
}

/// The environment partition's bytes: one valid copy with `default` set, the other copy erased.
pub fn env_partition(default: &str) -> Vec<u8> {
    let g = Geometry::in_partition(ENV_SIZE, 0, ENV_BLOCKS).unwrap();
    let list = vec![("default".to_string(), default.to_string())];
    let data = uboot_env::encode_vars(&list, g.data_len()).unwrap();
    let mut part = vec![0xFF; ENV_BLOCKS as usize * 512];
    let at = g.offset as usize;
    part[at..at + ENV_SIZE].copy_from_slice(&uboot_env::encode_copy(&data, 1));
    part
}

/// The whole disk: a GPT with the boot partition (1) and the environment (2), and their bytes.
/// Returns the disk and the environment's first and last block.
pub fn disk() -> Result<(Vec<u8>, u64, u64), String> {
    let boot = boot_partition()?;
    let boot_blocks = boot.len().div_ceil(512) as u64;
    let boot_last = BOOT_FIRST + boot_blocks - 1;
    let env_first = boot_last + 1;
    let env_last = env_first + ENV_BLOCKS - 1;
    // A whole number of 4 KiB (8 blocks): QEMU takes a disk image in whole logical blocks of that size.
    let total = (env_last + 2048).next_multiple_of(8);
    let mut disk = gpt::image(
        &[
            ([1; 16], BOOT_FIRST, boot_last),
            (gpt::ENV_TYPE_BYTES, env_first, env_last),
        ],
        total,
    );
    complete_gpt(&mut disk, total);
    let at = BOOT_FIRST as usize * 512;
    disk[at..at + boot.len()].copy_from_slice(&boot);
    let at = env_first as usize * 512;
    disk[at..at + ENV_BLOCKS as usize * 512].copy_from_slice(&env_partition("os-1"));
    Ok((disk, env_first, env_last))
}

/// Completes the GPT `gpt::image` writes, as a kernel's partition scan needs it: the protective MBR, the usable
/// range (LBA 34 to `total - 34`), the alternate header and the backup table and header at the disk's end, each
/// header's CRC set. `gpt::image` leaves the usable range and the backup out, which the environment reader does not need
/// but the kernel refuses without.
fn complete_gpt(disk: &mut [u8], total: u64) {
    const SECTOR: usize = 512;
    let table = disk[2 * SECTOR..2 * SECTOR + 128 * 128].to_vec();
    let table_crc = lemnos_slot_store::twocopy::crc32(&table);
    let backup_table = total - 33;
    let usable_last = total - 34;
    let header = |my: u64, alt: u64, entries: u64| {
        let mut h = vec![0u8; 92];
        h[..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&my.to_le_bytes());
        h[32..40].copy_from_slice(&alt.to_le_bytes());
        h[40..48].copy_from_slice(&34u64.to_le_bytes());
        h[48..56].copy_from_slice(&usable_last.to_le_bytes());
        h[56..72].copy_from_slice(&[0x5a; 16]);
        h[72..80].copy_from_slice(&entries.to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        h[88..92].copy_from_slice(&table_crc.to_le_bytes());
        let crc = lemnos_slot_store::twocopy::crc32(&h);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        h
    };
    // The protective MBR the kernel requires before it reads a GPT: one partition of type 0xEE over the disk.
    let mut mbr = [0u8; 512];
    mbr[446 + 4] = 0xEE;
    mbr[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
    let size = (total - 1).min(u64::from(u32::MAX)) as u32;
    mbr[446 + 12..446 + 16].copy_from_slice(&size.to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    disk[..SECTOR].copy_from_slice(&mbr);
    let primary = header(1, total - 1, 2);
    let backup = header(total - 1, 1, backup_table);
    disk[SECTOR..SECTOR + 92].copy_from_slice(&primary);
    let at = backup_table as usize * SECTOR;
    disk[at..at + table.len()].copy_from_slice(&table);
    let at = (total - 1) as usize * SECTOR;
    disk[at..at + 92].copy_from_slice(&backup);
}
