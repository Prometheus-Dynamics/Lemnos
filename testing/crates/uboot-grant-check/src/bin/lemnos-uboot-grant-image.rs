//! Writes the guest's disk image to the path given: `lemnos-uboot-grant-image <path>`. A regular file, never a
//! device: the guest boots it as a virtio disk. Prints the environment partition's blocks, which the
//! harness checks the disk's changes against.

use std::io::{self, Write};

fn fail(why: &str) -> ! {
    let _ = writeln!(io::stderr(), "lemnos-uboot-grant-image: {why}");
    std::process::exit(1);
}

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        fail("usage: lemnos-uboot-grant-image <path>");
    };
    let (disk, first, last) = lemnos_uboot_grant_check::disk().unwrap_or_else(|why| fail(&why));
    std::fs::write(&path, &disk).unwrap_or_else(|e| fail(&format!("{path}: {e}")));
    let _ = writeln!(
        io::stdout(),
        "env-blocks {first} {last} bytes {}",
        disk.len()
    );
}
