//! Finding the data partition by its ext2/3/4 label, on the boot disk only (its disk is
//! [`lemnos_slot_store::disk`]'s to find), from sysfs and the disks themselves: an initramfs has no blkid.
//!
//! The kernel lists every block device under `/sys/class/block`; a partition's directory has a
//! `partition` file (its number) and sits in its disk's directory. The label is the superblock's
//! volume name, which the image builder sets.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The ext2/3/4 volume name, from a partition's first 2 KiB.
pub fn ext_label(head: &[u8]) -> Option<String> {
    let sb = head.get(1024..1024 + 136)?;
    if u16::from_le_bytes([sb[56], sb[57]]) != 0xef53 {
        return None;
    }
    let name = &sb[120..136];
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    let label = std::str::from_utf8(&name[..end]).ok()?;
    (!label.is_empty()).then(|| label.to_owned())
}

/// Whether the ext superblock in a partition's first 2 KiB says the filesystem has errors.
pub fn ext_has_errors(head: &[u8]) -> bool {
    head.get(1024 + 58..1024 + 60)
        .is_some_and(|state| u16::from_le_bytes([state[0], state[1]]) & 0x0002 != 0)
}

fn read_at(path: &Path, offset: u64, len: usize) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// The first 2 KiB of a partition, for its label and error state.
pub fn head(dev: &Path) -> Option<Vec<u8>> {
    read_at(dev, 0, 2048)
}

pub use lemnos_slot_store::disk::{Part, partitions};

/// Every partition of `disk` whose ext superblock label is `label`, with the names of the others
/// on that disk (for the sentence when none matches). Other disks are never looked at.
pub fn labelled(label: &str, sys: &Path, dev: &Path, disk: &str) -> (Vec<Part>, Vec<String>) {
    let mut hits = Vec::new();
    let mut seen = Vec::new();
    for part in partitions(sys).into_iter().filter(|p| p.disk == disk) {
        let text = head(&dev.join(&part.name)).and_then(|h| ext_label(&h));
        if text.as_deref() == Some(label) {
            hits.push(part);
        } else {
            seen.push(format!(
                "{} (partition {}, LABEL {})",
                part.name,
                part.number,
                text.as_deref().unwrap_or("-")
            ));
        }
    }
    (hits, seen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ext_superblock_gives_its_label_and_error_state() {
        let mut head = vec![0u8; 2048];
        head[1024 + 56..1024 + 58].copy_from_slice(&0xef53u16.to_le_bytes());
        head[1024 + 120..1024 + 124].copy_from_slice(b"data");
        assert_eq!(ext_label(&head).as_deref(), Some("data"));
        assert!(!ext_has_errors(&head));
        head[1024 + 58] = 2;
        assert!(ext_has_errors(&head));
        assert_eq!(ext_label(&[0; 2048]), None);
    }
}
