//! U-Boot's redundant environment, read and written raw on its own partition: the boot count's
//! and the trial's variables (`default`, `trial`, `trial_attempts`) for the `uboot` slot record.
//!
//! The format is U-Boot's, from its sources at tag `v2026.10` (commit `5508406`; read-only), each
//! fact with the file and line it is read from:
//! - `include/env_internal.h:56-62,78-83`: `env_t` is a CRC-32 word, a one-byte serial (with
//!   `CONFIG_ENV_REDUNDANT`: a five-byte header), then the data area of `CONFIG_ENV_SIZE` less the
//!   header.
//! - `env/common.c:582-600` (`env_export`): the CRC is over the whole data area, padding included,
//!   and the serial is `++env_flags`.
//! - `env/common.c:484-551` (`env_check_redund`): the selection rule [`select`] mirrors: a copy
//!   counts when its CRC is right; of two, serial 255 against 0 makes the 0 newer, otherwise the
//!   higher serial wins and a tie takes copy one; neither right is no environment.
//! - `env/common.c:553-578` (`env_import_redund`): the serial of the chosen copy becomes
//!   `env_flags`, so the next save is that serial plus one.
//! - `env/common.c:443-466` (`env_import`): an import replaces the table (`himport_r` without
//!   `H_NOCLEAR`), so a variable absent from the saved environment is absent from U-Boot: [`set`]
//!   keeps every variable it finds and never makes a first environment.
//! - `env/mmc.c:329-376` (`env_mmc_save`): the copy that is not active is written (`copy = 1`
//!   when copy zero is the valid one), whole, then the active one flips.
//! - `env/mmc.c:319-326` (`write_env`): a copy is written in blocks, its size rounded up.
//!
//! The CRC word is little-endian: it is a `uint32_t` stored by the CPU, and every board here is
//! little-endian.
//!
//! The writer changes only the copy that is not active, in one write of the copy's sectors. The
//! serial byte is not covered by the CRC, and the new serial is the active one plus one: a cut at
//! any byte leaves either the old copy (its CRC, or its serial, is the one U-Boot takes) or the new
//! one. [`set`] is that writer; [`load`] is the reader U-Boot's rule defines.
//!
//! Where the two copies are. With `CONFIG_PARTITION_TYPE_GUID` U-Boot puts them at the top of the
//! one GPT partition of type `3de21764-95bd-54bd-a5c3-4abe786f38a8` (`env/mmc.c:114-148`: copy 0 in
//! the last `CONFIG_ENV_SIZE` rounded up to blocks, copy 1 just below it): [`Geometry::in_partition`]
//! is that rule and [`crate::gpt`] finds the partition, so the board file states only the size.
//! A board whose environment is at fixed offsets of a block device (`CONFIG_ENV_OFFSET`,
//! `CONFIG_ENV_OFFSET_REDUND`, no GPT partition) states the device and the offsets instead.
//!
//! Before the first write both stored copies are read and CRC-checked ([`set`]). A write never goes
//! out unless one copy is valid (the other may be torn by an earlier cut: it is the one to write);
//! no valid copy is refused by name, an erased partition as its own case.

use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::twocopy::crc32;

/// The header in bytes: the CRC word and the serial.
pub const HEADER: usize = 5;
/// The sector size the copies are written in.
pub const SECTOR: usize = 512;

/// The environment's geometry: `size` is `CONFIG_ENV_SIZE`, and the two copies' offsets (bytes
/// from the start of the device).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub size: usize,
    pub offset: u64,
    pub offset_redund: u64,
}

impl Geometry {
    /// A geometry, refused when it can't hold a header and a variable, or when the copies overlap.
    pub fn new(size: usize, offset: u64, offset_redund: u64) -> Result<Self, String> {
        if size <= HEADER + 2 {
            return Err(format!("the environment size {size} is too small"));
        }
        let padded = size.div_ceil(SECTOR) as u64 * SECTOR as u64;
        let (a, b) = (offset.min(offset_redund), offset.max(offset_redund));
        if b - a < padded {
            return Err("the two environment copies overlap".into());
        }
        Ok(Self {
            size,
            offset,
            offset_redund,
        })
    }

    /// U-Boot's placement in a GPT partition of `blocks` 512-byte blocks starting at block `first`:
    /// `len` is `CONFIG_ENV_SIZE` rounded up to blocks, copy 0 is the last `len` blocks of the
    /// partition and copy 1 the `len` blocks below it (`env/mmc.c:114-148`; a partition holding
    /// fewer than two copies is U-Boot's "too small" refusal at `env/mmc.c:135-141`).
    pub fn in_partition(size: usize, first: u64, blocks: u64) -> Result<Self, String> {
        let len = size.div_ceil(SECTOR) as u64;
        if len.saturating_mul(2) > blocks {
            return Err(format!(
                "the environment partition ({blocks} blocks) is too small for two copies of {size} bytes"
            ));
        }
        let end = first + blocks;
        Self::new(
            size,
            (end - len) * SECTOR as u64,
            (end - 2 * len) * SECTOR as u64,
        )
    }

    /// The data area's length.
    pub fn data_len(&self) -> usize {
        self.size - HEADER
    }

    /// The bytes one copy's write covers: its size rounded up to a sector.
    pub fn padded(&self) -> usize {
        self.size.div_ceil(SECTOR) * SECTOR
    }
}

/// One copy as U-Boot's `env_t` holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copy {
    pub crc: u32,
    pub flags: u8,
    pub data: Vec<u8>,
}

impl Copy {
    /// The copy as its bytes: the CRC word, the serial, the data area.
    pub fn parse(bytes: &[u8], geometry: &Geometry) -> Option<Self> {
        if bytes.len() < geometry.size {
            return None;
        }
        Some(Self {
            crc: u32::from_le_bytes(bytes[..4].try_into().ok()?),
            flags: bytes[4],
            data: bytes[HEADER..geometry.size].to_vec(),
        })
    }

    /// Whether the stored CRC is the data area's (`env_check_redund`'s `crc1_ok`).
    pub fn valid(&self) -> bool {
        crc32(&self.data) == self.crc
    }
}

/// Which copy is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    /// Copy one, at `offset`.
    First,
    /// Copy two, at `offset_redund`.
    Second,
}

/// U-Boot's selection (`env_check_redund`). A copy is taken when its CRC is right. With both
/// right, serial 255 against 0 makes the 0 copy the newer (the serial wrapped), otherwise the
/// higher serial wins, and equal serials take copy one. With neither, there is no environment.
pub fn select(first: Option<&Copy>, second: Option<&Copy>) -> Option<Which> {
    let first = first.filter(|c| c.valid());
    let second = second.filter(|c| c.valid());
    match (first, second) {
        (None, None) => None,
        (Some(_), None) => Some(Which::First),
        (None, Some(_)) => Some(Which::Second),
        (Some(a), Some(b)) => {
            // Serial 255 against 0: the 0 is the newer. Otherwise the higher serial wins; a tie
            // takes copy one.
            let second_newer = if a.flags == 255 && b.flags == 0 {
                true
            } else if b.flags == 255 && a.flags == 0 {
                false
            } else {
                b.flags > a.flags
            };
            Some(if second_newer {
                Which::Second
            } else {
                Which::First
            })
        }
    }
}

/// A list of `(name, value)` variables.
pub type Vars = Vec<(String, String)>;

/// The variables of a data area: `name=value` entries, each ended by a NUL, the list by an empty
/// entry (`\0`). Anything after that is padding.
pub fn vars(data: &[u8]) -> Result<Vars, String> {
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        let Some(end) = data
            .get(at..)
            .and_then(|rest| rest.iter().position(|&b| b == 0))
        else {
            return Err("the environment's list is not terminated".into());
        };
        if end == 0 {
            return Ok(out);
        }
        let entry = std::str::from_utf8(&data[at..at + end])
            .map_err(|_| "an environment variable isn't UTF-8".to_string())?;
        let (name, value) = entry
            .split_once('=')
            .ok_or_else(|| format!("the environment entry `{entry}` has no `=`"))?;
        out.push((name.to_string(), value.to_string()));
        at += end + 1;
    }
}

/// The data area holding `vars`, zero-padded to `data_len`. Refused when it does not fit, or a
/// name or value has a NUL or a `=` where it can't.
pub fn encode_vars(vars: &[(String, String)], data_len: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(data_len);
    for (name, value) in vars {
        if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') {
            return Err(format!("`{name}` can't be an environment variable"));
        }
        out.extend_from_slice(name.as_bytes());
        out.push(b'=');
        out.extend_from_slice(value.as_bytes());
        out.push(0);
    }
    out.push(0);
    if out.len() > data_len {
        return Err("the environment is full".into());
    }
    out.resize(data_len, 0);
    Ok(out)
}

/// One copy's bytes for `data` at serial `flags`: the CRC word, the serial, the data.
pub fn encode_copy(data: &[u8], flags: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + data.len());
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.push(flags);
    out.extend_from_slice(data);
    out
}

/// A device the environment is read and written on: byte offsets, nothing else.
pub trait EnvDev {
    /// Fills `buf` from byte `offset`.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    /// Writes `bytes` at byte `offset`, durably, before it returns.
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()>;
}

/// The environment's node, a block device or a file, opened read-write.
pub struct FileEnv {
    file: File,
}

impl FileEnv {
    /// Opens `node` read-write. Nothing is mounted.
    pub fn open(node: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(node)?;
        Ok(Self { file })
    }

    /// Opens `node` as [`FileEnv::open`] does, and returns its length in bytes (a block device's
    /// size, which its metadata does not give).
    pub fn open_sized(node: &Path) -> io::Result<(Self, u64)> {
        let mut env = Self::open(node)?;
        let len = env.file.seek(SeekFrom::End(0))?;
        Ok((env, len))
    }
}

impl EnvDev for FileEnv {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.file.read_exact_at(buf, offset)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all_at(bytes, offset)?;
        self.file.sync_data()
    }
}

/// Both copies as they stand on the device, each `size` bytes (raw), and as U-Boot reads them.
type Raw = (Vec<u8>, Vec<u8>);

fn read_raw<D: EnvDev + ?Sized>(dev: &mut D, g: &Geometry) -> io::Result<Raw> {
    let mut one = vec![0u8; g.size];
    dev.read_at(g.offset, &mut one)?;
    let mut two = vec![0u8; g.size];
    dev.read_at(g.offset_redund, &mut two)?;
    Ok((one, two))
}

fn read_copies<D: EnvDev + ?Sized>(
    dev: &mut D,
    g: &Geometry,
) -> io::Result<(Option<Copy>, Option<Copy>)> {
    let (one, two) = read_raw(dev, g)?;
    Ok((Copy::parse(&one, g), Copy::parse(&two, g)))
}

/// Whether a copy is erased: every byte 0xFF (erased flash) or every byte 0 (a zeroed area).
fn blank(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| b == 0xFF) || bytes.iter().all(|&b| b == 0)
}

/// Why no environment can be written here: both copies erased, or neither valid at this size.
pub fn refusal(raw: &Raw, size: usize) -> String {
    if blank(&raw.0) && blank(&raw.1) {
        "the U-Boot environment area is erased: U-Boot has not saved an environment here yet (boot.cmd saves the first one at the first start), so none is written"
            .to_string()
    } else {
        format!(
            "no valid U-Boot environment of {size} bytes here: U-Boot may be built with another CONFIG_ENV_SIZE"
        )
    }
}

/// The active copy and its variables: `None` when neither copy is valid (U-Boot has no environment
/// here, and nothing is written).
pub fn load<D: EnvDev + ?Sized>(
    dev: &mut D,
    g: &Geometry,
) -> Result<Option<(Which, Vars)>, String> {
    let (first, second) = read_copies(dev, g).map_err(|e| e.to_string())?;
    let Some(which) = select(first.as_ref(), second.as_ref()) else {
        return Ok(None);
    };
    let active = match which {
        Which::First => first,
        Which::Second => second,
    }
    .ok_or("the selected copy vanished")?;
    Ok(Some((which, vars(&active.data)?)))
}

/// [`load`], refused by name (see [`refusal`]) when neither copy is valid.
pub fn load_checked<D: EnvDev + ?Sized>(
    dev: &mut D,
    g: &Geometry,
) -> Result<(Which, Vars), String> {
    match load(dev, g)? {
        Some(found) => Ok(found),
        None => {
            let raw = read_raw(dev, g).map_err(|e| e.to_string())?;
            Err(refusal(&raw, g.size))
        }
    }
}

/// Sets (`Some`) or removes (`None`) variables, in one write of the copy that is not active. The
/// new serial is the active one plus one, wrapping as U-Boot's does.
///
/// Both stored copies are read and CRC-checked first. With neither valid nothing is written
/// ([`refusal`]). With one valid (the other torn by an earlier cut, or never written) the valid one
/// is the active one and the other is the copy written. After the write both copies are read again:
/// the new copy must be valid and the one U-Boot's rule selects, or the write is reported as
/// failed (the old copy is untouched and still valid).
pub fn set<D: EnvDev + ?Sized>(
    dev: &mut D,
    g: &Geometry,
    updates: &[(&str, Option<&str>)],
) -> Result<(), String> {
    let (first, second) = read_copies(dev, g).map_err(|e| e.to_string())?;
    let Some(which) = select(first.as_ref(), second.as_ref()) else {
        let raw = read_raw(dev, g).map_err(|e| e.to_string())?;
        return Err(refusal(&raw, g.size));
    };
    let active = match which {
        Which::First => first,
        Which::Second => second,
    }
    .ok_or("the selected copy vanished")?;
    let mut list = vars(&active.data)?;
    for (name, value) in updates {
        list.retain(|(existing, _)| existing != name);
        if let Some(value) = value {
            list.push(((*name).to_string(), (*value).to_string()));
        }
    }
    let data = encode_vars(&list, g.data_len())?;
    let flags = active.flags.wrapping_add(1);
    // The inactive copy is the one written.
    let (at, written) = match which {
        Which::First => (g.offset_redund, Which::Second),
        Which::Second => (g.offset, Which::First),
    };
    // The sectors the copy covers, the tail beyond `size` kept as the device has it.
    let mut buf = vec![0u8; g.padded()];
    dev.read_at(at, &mut buf).map_err(|e| e.to_string())?;
    buf[..g.size].copy_from_slice(&encode_copy(&data, flags));
    dev.write_at(at, &buf).map_err(|e| e.to_string())?;
    match load(dev, g)? {
        Some((now, got)) if now == written && got == vars(&data)? => Ok(()),
        _ => Err("the environment read back after the write is not the one written".into()),
    }
}

/// The file on the boot partition that names the environment for slot-init: `uboot-env.txt`,
/// written at image build by `lemnos-ctl tree --boot-dir` from the board file's `uboot_env_*` keys.
/// Nothing writes it at run time.
pub const LOCATOR: &str = "uboot-env.txt";

/// The locator's `version`.
pub const LOCATOR_VERSION: u32 = 1;

/// Where the environment is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// In the one GPT partition of the environment's type GUID on the boot disk, at the top of it
    /// ([`Geometry::in_partition`]). Only `CONFIG_ENV_SIZE` is stated.
    Gpt { size: usize },
    /// On the block device `device`, at fixed byte offsets (`CONFIG_ENV_OFFSET` and
    /// `CONFIG_ENV_OFFSET_REDUND`, which U-Boot counts from the start of that device).
    Device { device: String, geometry: Geometry },
}

/// A device node whose writes need an erase first (flash through MTD, or a UBI volume): a raw
/// block write of this form does not apply there.
fn needs_erase(device: &str) -> bool {
    ["/dev/mtd", "/dev/ubi"]
        .iter()
        .any(|p| device.starts_with(p))
}

impl Place {
    /// The `size`'s `Gpt` place, refused when it can't hold a header and a variable.
    pub fn gpt(size: usize) -> Result<Self, String> {
        Geometry::new(size, 0, size.div_ceil(SECTOR) as u64 * SECTOR as u64)?;
        Ok(Self::Gpt { size })
    }

    /// The fixed-offset place, refused by name when the device is not an absolute path or needs an
    /// erase, a copy does not start on a sector, or the geometry is not one [`Geometry::new`] takes.
    pub fn device(
        device: &str,
        size: usize,
        offset: u64,
        offset_redund: u64,
    ) -> Result<Self, String> {
        if !device.starts_with('/') || device.contains(['\n', '\r', '"']) {
            return Err(format!("`{device}` isn't an absolute device path"));
        }
        if needs_erase(device) {
            return Err(format!(
                "`{device}` is flash or a UBI volume: its writes need an erase first, and the raw environment writer does not do that"
            ));
        }
        for (name, at) in [("offset", offset), ("offset_redund", offset_redund)] {
            if at % SECTOR as u64 != 0 {
                return Err(format!(
                    "the environment's {name} {at} isn't a multiple of the {SECTOR}-byte sector"
                ));
            }
        }
        Ok(Self::Device {
            device: device.to_string(),
            geometry: Geometry::new(size, offset, offset_redund)?,
        })
    }

    /// `CONFIG_ENV_SIZE`.
    pub fn size(&self) -> usize {
        match self {
            Self::Gpt { size } => *size,
            Self::Device { geometry, .. } => geometry.size,
        }
    }

    /// The node to open: the boot disk `disk` for the GPT form, the stated device otherwise.
    pub fn node(&self, disk: &Path) -> PathBuf {
        match self {
            Self::Gpt { .. } => disk.to_path_buf(),
            Self::Device { device, .. } => PathBuf::from(device),
        }
    }

    /// The two copies' geometry on the opened node `dev` of `bytes` bytes. The GPT form reads the
    /// table ([`crate::gpt::find_env_partition`]); both forms refuse by name a copy that runs past
    /// the end of the node.
    pub fn geometry<D: EnvDev + ?Sized>(
        &self,
        dev: &mut D,
        bytes: u64,
    ) -> Result<Geometry, String> {
        let geometry = match self {
            Self::Gpt { size } => {
                let part = crate::gpt::find_env_partition(dev)?;
                Geometry::in_partition(*size, part.first, part.blocks())?
            }
            Self::Device { geometry, .. } => *geometry,
        };
        let padded = geometry.padded() as u64;
        for (name, at) in [
            ("the first copy", geometry.offset),
            ("the second copy", geometry.offset_redund),
        ] {
            if at.saturating_add(padded) > bytes {
                return Err(format!(
                    "{name} of the environment ({padded} bytes at {at}) runs past the end of its {bytes}-byte device"
                ));
            }
        }
        Ok(geometry)
    }

    /// The locator's text: a header that says who wrote it, then one `key = value` per line.
    pub fn encode(&self) -> String {
        let head = format!(
            "# Written at image build by lemnos-ctl tree --boot-dir from the board file's uboot_env_*\n\
             # keys. Do not edit: slot-init refuses a locator that does not fit its device.\n\
             version = {LOCATOR_VERSION}\n"
        );
        match self {
            Self::Gpt { size } => {
                format!("{head}form = \"gpt\"\nsize = {size}\nsector = {SECTOR}\n")
            }
            Self::Device {
                device,
                geometry: g,
            } => format!(
                "{head}form = \"device\"\ndevice = \"{device}\"\nsize = {}\nsector = {SECTOR}\noffset = {}\noffset_redund = {}\n",
                g.size, g.offset, g.offset_redund
            ),
        }
    }

    /// Reads a locator. Refused by name: a key it does not know, a key given twice or not at all
    /// for its form, a version other than [`LOCATOR_VERSION`], a form other than `gpt` or `device`,
    /// a sector other than [`SECTOR`], a number that is not decimal, and anything [`Place::gpt`] or
    /// [`Place::device`] refuses.
    pub fn parse(text: &str) -> Result<Self, String> {
        const KEYS: [&str; 7] = [
            "version",
            "form",
            "size",
            "sector",
            "device",
            "offset",
            "offset_redund",
        ];
        let mut values: [Option<&str>; 7] = [None; 7];
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| format!("{LOCATOR}: `{line}` is not `key = value`"))?;
            let key = key.trim();
            let index = KEYS
                .iter()
                .position(|k| *k == key)
                .ok_or_else(|| format!("{LOCATOR}: unknown key {key:?}"))?;
            if values[index].replace(value.trim()).is_some() {
                return Err(format!("{LOCATOR}: {key:?} is given twice"));
            }
        }
        let get =
            |i: usize| values[i].ok_or_else(|| format!("{LOCATOR}: {:?} is missing", KEYS[i]));
        let number = |i: usize| -> Result<u64, String> {
            let text = get(i)?;
            if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!(
                    "{LOCATOR}: {:?} is {text:?}, not a decimal number",
                    KEYS[i]
                ));
            }
            text.parse()
                .map_err(|_| format!("{LOCATOR}: {:?} is out of range", KEYS[i]))
        };
        let quoted = |i: usize| -> Result<String, String> {
            get(i)?
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .map(str::to_string)
                .ok_or_else(|| format!("{LOCATOR}: {:?} must be quoted", KEYS[i]))
        };
        if number(0)? != u64::from(LOCATOR_VERSION) {
            return Err(format!(
                "{LOCATOR}: version {:?}; this reader reads {LOCATOR_VERSION}",
                get(0)?
            ));
        }
        if number(3)? != SECTOR as u64 {
            return Err(format!(
                "{LOCATOR}: sector {:?}; this reader writes {SECTOR}",
                get(3)?
            ));
        }
        let size =
            usize::try_from(number(2)?).map_err(|_| format!("{LOCATOR}: size is out of range"))?;
        let only = |forbidden: &[usize], form: &str| -> Result<(), String> {
            match forbidden.iter().find(|&&i| values[i].is_some()) {
                Some(&i) => Err(format!(
                    "{LOCATOR}: {:?} is not a key of the {form} form",
                    KEYS[i]
                )),
                None => Ok(()),
            }
        };
        match quoted(1)?.as_str() {
            "gpt" => {
                only(&[4, 5, 6], "gpt")?;
                Self::gpt(size)
            }
            "device" => Self::device(&quoted(4)?, size, number(5)?, number(6)?),
            other => Err(format!(
                "{LOCATOR}: form {other:?}; this reader reads \"gpt\" and \"device\""
            )),
        }
        .map_err(|e| {
            if e.starts_with(LOCATOR) {
                e
            } else {
                format!("{LOCATOR}: {e}")
            }
        })
    }
}

#[cfg(test)]
#[path = "uboot_env_tests.rs"]
mod tests;
