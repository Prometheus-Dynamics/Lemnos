//! Finding the data partition by `PARTUUID=` or `LABEL=`, and the boot partition beside it,
//! from sysfs and the disks themselves: an initramfs has no blkid.
//!
//! The kernel lists every block device under `/sys/class/block`; a partition's directory has a
//! `partition` file (its number) and sits in its disk's directory. A PARTUUID is the MBR disk
//! signature and the partition number (`1234abcd-02`), or the GPT entry's unique GUID. A label
//! is the ext2/3/4 superblock's volume name.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// What names the data partition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    PartUuid(String),
    Label(String),
}

impl Spec {
    /// `PARTUUID=<id>` or `LABEL=<name>`.
    ///
    /// # Errors
    ///
    /// When it is neither.
    pub fn parse(text: &str) -> Result<Self, String> {
        match text.split_once('=') {
            Some(("PARTUUID", id)) if !id.is_empty() => Ok(Self::PartUuid(id.to_ascii_lowercase())),
            Some(("LABEL", name)) if !name.is_empty() => Ok(Self::Label(name.to_owned())),
            _ => Err(format!("{text:?} isn't PARTUUID=<id> or LABEL=<name>")),
        }
    }
}

/// An MBR disk's PARTUUID for partition `number`, from its first sector. `None` for a GPT disk
/// (its protective MBR) or no MBR.
pub fn mbr_partuuid(sector0: &[u8], number: u32) -> Option<String> {
    if sector0.len() < 512 || sector0[510..512] != [0x55, 0xaa] || sector0[450] == 0xee {
        return None;
    }
    let signature = u32::from_le_bytes(sector0[440..444].try_into().ok()?);
    Some(format!("{signature:08x}-{number:02x}"))
}

/// A GUID as GPT stores it: the first three fields little-endian.
pub fn guid(bytes: &[u8]) -> Option<String> {
    let b: &[u8; 16] = bytes.get(..16)?.try_into().ok()?;
    Some(format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        u16::from_le_bytes([b[4], b[5]]),
        u16::from_le_bytes([b[6], b[7]]),
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    ))
}

/// A GPT disk's PARTUUID for partition `number`. `read(offset, len)` reads the disk; `sector`
/// is its logical block size.
pub fn gpt_partuuid(
    read: &mut dyn FnMut(u64, usize) -> Option<Vec<u8>>,
    sector: u64,
    number: u32,
) -> Option<String> {
    let header = read(sector, 92)?;
    if header.get(..8)? != b"EFI PART" || number == 0 {
        return None;
    }
    let table = u64::from_le_bytes(header.get(72..80)?.try_into().ok()?);
    let count = u32::from_le_bytes(header.get(80..84)?.try_into().ok()?);
    let size = u32::from_le_bytes(header.get(84..88)?.try_into().ok()?);
    if number > count || size < 128 {
        return None;
    }
    let at = table
        .checked_mul(sector)?
        .checked_add(u64::from(number - 1) * u64::from(size))?;
    let entry = read(at, 32)?;
    // An unused entry's type GUID is zero.
    if entry.get(..16)?.iter().all(|&b| b == 0) {
        return None;
    }
    guid(entry.get(16..32)?)
}

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

/// One partition the kernel lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    pub number: u32,
    pub disk: String,
}

/// Every partition under `sys/class/block`, by name.
pub fn partitions(sys: &Path) -> Vec<Part> {
    let mut found: Vec<Part> = std::fs::read_dir(sys.join("class/block"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let real = std::fs::canonicalize(entry.path()).ok()?;
            let number = std::fs::read_to_string(real.join("partition"))
                .ok()?
                .trim()
                .parse()
                .ok()?;
            let disk = real.parent()?.file_name()?.to_string_lossy().into_owned();
            Some(Part { name, number, disk })
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

fn partuuid(sys: &Path, dev: &Path, part: &Part) -> Option<String> {
    let disk = dev.join(&part.disk);
    read_at(&disk, 0, 512)
        .and_then(|sector0| mbr_partuuid(&sector0, part.number))
        .or_else(|| {
            let sector = std::fs::read_to_string(
                sys.join("block")
                    .join(&part.disk)
                    .join("queue/logical_block_size"),
            )
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(512);
            gpt_partuuid(&mut |at, len| read_at(&disk, at, len), sector, part.number)
        })
}

/// The partition `spec` names, with sysfs at `sys` and the device nodes in `dev`.
///
/// # Errors
///
/// None matches: every partition looked at, and what it is called.
pub fn find(spec: &Spec, sys: &Path, dev: &Path) -> Result<Part, String> {
    let mut seen = Vec::new();
    for part in partitions(sys) {
        let id = partuuid(sys, dev, &part);
        let label = head(&dev.join(&part.name)).and_then(|h| ext_label(&h));
        let hit = match spec {
            Spec::PartUuid(want) => id.as_deref() == Some(want.as_str()),
            Spec::Label(want) => label.as_deref() == Some(want.as_str()),
        };
        if hit {
            return Ok(part);
        }
        seen.push(format!(
            "{} (partition {} of {}, PARTUUID {}, LABEL {})",
            part.name,
            part.number,
            part.disk,
            id.as_deref().unwrap_or("?"),
            label.as_deref().unwrap_or("-")
        ));
    }
    Err(if seen.is_empty() {
        format!("no partitions under {}", sys.join("class/block").display())
    } else {
        format!("no partition matches; saw {}", seen.join("; "))
    })
}

/// The boot partition: partition 1 of the data partition's disk.
pub fn boot_beside(sys: &Path, dev: &Path, data: &Part) -> Option<PathBuf> {
    partitions(sys)
        .into_iter()
        .find(|p| p.disk == data.disk && p.number == 1)
        .map(|p| dev.join(p.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mbr(signature: u32) -> Vec<u8> {
        let mut s = vec![0u8; 512];
        s[440..444].copy_from_slice(&signature.to_le_bytes());
        s[450] = 0x83;
        s[510] = 0x55;
        s[511] = 0xaa;
        s
    }

    #[test]
    fn specs_are_partuuid_or_label() {
        assert_eq!(
            Spec::parse("PARTUUID=ABCD-02"),
            Ok(Spec::PartUuid("abcd-02".into()))
        );
        assert_eq!(Spec::parse("LABEL=data"), Ok(Spec::Label("data".into())));
        assert!(Spec::parse("UUID=x").is_err());
        assert!(Spec::parse("PARTUUID=").is_err());
    }

    #[test]
    fn an_mbr_partuuid_is_the_signature_and_the_number() {
        assert_eq!(
            mbr_partuuid(&mbr(0x1234_abcd), 2).as_deref(),
            Some("1234abcd-02")
        );
        let mut gpt = mbr(1);
        gpt[450] = 0xee;
        assert_eq!(mbr_partuuid(&gpt, 2), None);
        assert_eq!(mbr_partuuid(&[0; 512], 2), None);
    }

    #[test]
    fn a_gpt_partuuid_is_the_entrys_guid() {
        let mut disk = vec![0u8; 4096];
        disk[512..520].copy_from_slice(b"EFI PART");
        disk[512 + 72..512 + 80].copy_from_slice(&2u64.to_le_bytes());
        disk[512 + 80..512 + 84].copy_from_slice(&4u32.to_le_bytes());
        disk[512 + 84..512 + 88].copy_from_slice(&128u32.to_le_bytes());
        let second = 1024 + 128;
        disk[second] = 1;
        let unique: Vec<u8> = (1..=16).collect();
        disk[second + 16..second + 32].copy_from_slice(&unique);
        let mut read =
            |at: u64, len: usize| disk.get(at as usize..at as usize + len).map(<[u8]>::to_vec);
        assert_eq!(
            gpt_partuuid(&mut read, 512, 2).as_deref(),
            Some("04030201-0605-0807-090a-0b0c0d0e0f10")
        );
        assert_eq!(gpt_partuuid(&mut read, 512, 1), None, "an unused entry");
        assert_eq!(gpt_partuuid(&mut read, 512, 5), None, "past the table");
    }

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
