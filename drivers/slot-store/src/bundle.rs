//! A slot bundle, written and read by one implementation.
//!
//! The installer (`system-slots`), `lemnos-ctl bundle` (which writes a bundle and verifies one) and
//! the tests all go through this module for the format: the member rules and the sizes they take
//! ([`check_order`], [`check_member_size`]), the checks of a bundle's contents against its manifest
//! ([`check_root`], [`check_boot`]), the command line's words ([`check_cmdline`]), the manifest's
//! bytes ([`Manifest::to_json`](crate::manifest::Manifest::to_json)), the signed message and the
//! record ([`signing`]), and the tar ([`crate::tar`]). A refusal text is written once, here.
//!
//! - [`build`] and [`write_to`] make a bundle from a board's rules, the five boot payloads, the
//!   root image with its hash tree and the builder's verity table, refusing by name before anything
//!   is written.
//! - [`Reader`] is a [`Sink`] that takes a bundle as the installer does and keeps what the installer
//!   checks; [`verify`] runs it and reports each check, short of installing anything.

use crate::bays::{self, Layout, ROLES};
use crate::bootimage::{self, Family};
use crate::manifest::{BLOCK, Manifest, Overlay, Verity, is_hex, tree_bytes, valid_name};
use crate::signing;
use crate::store::valid_id;
use crate::tar::{Kind, Sink, Tar, write};
use crate::verity;
use sha2::{Digest, Sha256};
use std::io::{self, Read, Seek, SeekFrom, Write};

/// The first member.
pub const MANIFEST: &str = "slot.json";
/// The second member, when the bundle is signed.
pub const SIGNATURE: &str = "slot.sig";
/// The image, its hash tree appended.
pub const ROOTFS: &str = "rootfs.erofs";
/// The directory of the boot files.
pub const BOOT_DIR: &str = "boot";
/// The largest manifest taken.
pub const MAX_MANIFEST: u64 = 64 * 1024;
/// The hole `lemnos-ctl tree --cmdline-slot` leaves for the root hash: [`build`] fills it.
pub const ROOTHASH_HOLE: &str = "@roothash@";

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if !is_hex(text, N * 2) {
        return None;
    }
    let mut out = [0u8; N];
    for (at, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[at * 2..at * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// What a bundle may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Member {
    Manifest,
    Signature,
    Root,
    Boot,
}

/// The kind of member a name is, `None` for a name a bundle may not hold.
pub fn member(name: &str) -> Option<Member> {
    match name {
        MANIFEST => Some(Member::Manifest),
        SIGNATURE => Some(Member::Signature),
        ROOTFS => Some(Member::Root),
        name if name.starts_with("boot/") => Some(Member::Boot),
        _ => None,
    }
}

/// The member's place in the bundle's order, given the members begun before it, refused by a
/// sentence naming the rule. `slot.json` first, `slot.sig` second when present, then `rootfs.erofs`
/// and `boot/` in any order, each once.
///
/// # Errors
///
/// The sentence for the rule the member breaks.
pub fn check_order(entries: &[String], name: &str, kind: Kind) -> Result<Member, String> {
    let what = if name == BOOT_DIR && kind == Kind::Dir {
        Member::Boot
    } else {
        member(name).ok_or_else(|| {
            format!("the bundle has {name}: only slot.json, slot.sig, rootfs.erofs and boot/")
        })?
    };
    if entries.is_empty() && what != Member::Manifest {
        return Err(format!(
            "the bundle starts with {name}, and slot.json must be its first member: the order is slot.json, then slot.sig when the bundle is signed, then rootfs.erofs and boot/ in any order"
        ));
    }
    if entries.iter().any(|seen| seen == name) {
        return Err(format!("the bundle has {name} twice"));
    }
    if what == Member::Signature && entries.len() != 1 {
        return Err("slot.sig must be the bundle's second member, right after slot.json: the signature is decided before any payload is written".into());
    }
    Ok(what)
}

/// What a manifest or signature member's announced size must be.
///
/// # Errors
///
/// `slot.json` over [`MAX_MANIFEST`]; a `slot.sig` that is not a whole record.
pub fn check_member_size(what: Member, size: u64) -> Result<(), String> {
    match what {
        Member::Manifest if size > MAX_MANIFEST => Err("slot.json is too large".into()),
        Member::Signature if size != signing::RECORD_LEN as u64 => Err(
            "slot.sig is not a signature record: 108 bytes, the magic LMNSSIG1, algorithm 1 (a bare 64-byte signature is refused)"
                .into(),
        ),
        _ => Ok(()),
    }
}

/// A slot id the manifest may carry.
///
/// # Errors
///
/// When `id` is not a slot id.
pub fn check_slot_id(id: &str) -> Result<(), String> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(format!("slot.json: `{id}` isn't a slot id"))
    }
}

/// A boot file the manifest must list, checked at its header, before its bytes.
///
/// # Errors
///
/// When `file` (a path under `boot/`) is not in `manifest.boot.files`.
pub fn check_listed(manifest: &Manifest, file: &str) -> Result<(), String> {
    if manifest.boot_files.iter().any(|(listed, _)| listed == file) {
        Ok(())
    } else {
        Err(format!(
            "boot file {file} isn't in slot.json: a file the manifest does not list is refused"
        ))
    }
}

/// `hashes`: each member's SHA-256 by the name it has in the bundle (`rootfs.erofs`, `boot/<path>`).
fn hash_of<'a>(hashes: &'a [(String, String)], name: &str) -> Option<&'a str> {
    hashes
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, sha)| sha.as_str())
}

/// The root image against the manifest: its size (`root_size`, `None` when the bundle had none) and
/// its SHA-256.
///
/// # Errors
///
/// The sentence naming what differs.
pub fn check_root(
    manifest: &Manifest,
    root_size: Option<u64>,
    hashes: &[(String, String)],
) -> Result<(), String> {
    let size = root_size
        .filter(|_| hash_of(hashes, ROOTFS).is_some())
        .ok_or("not a slot bundle: no rootfs.erofs")?;
    if size != manifest.image_size {
        return Err("rootfs.erofs isn't the size slot.json says".into());
    }
    if hash_of(hashes, ROOTFS) != Some(manifest.image_sha256.as_str()) {
        return Err("rootfs.erofs doesn't match slot.json's sha256".into());
    }
    Ok(())
}

/// The boot files against the manifest: each listed file is in the bundle with its SHA-256, and the
/// bundle has no other.
///
/// # Errors
///
/// The sentence naming the file.
pub fn check_boot(manifest: &Manifest, hashes: &[(String, String)]) -> Result<(), String> {
    for (name, want) in &manifest.boot_files {
        match hash_of(hashes, &format!("boot/{name}")) {
            Some(got) if got == want => {}
            None => return Err(format!("the bundle has no boot file {name}")),
            _ => return Err(format!("boot file {name} doesn't match slot.json")),
        }
    }
    for (name, _) in hashes {
        if let Some(file) = name.strip_prefix("boot/")
            && !manifest.boot_files.iter().any(|(listed, _)| listed == file)
        {
            return Err(format!("boot file {file} isn't in slot.json"));
        }
    }
    Ok(())
}

/// The five boot files by their names under `boot/`, in role order: what a slot's boot files are.
pub fn role_files() -> [&'static str; 5] {
    std::array::from_fn(|i| ROLES[i].path)
}

/// `boot.files` is exactly the five role files: nothing missing, nothing more. A file outside the
/// five has no bay to be stored in, and the every-boot check hashes every listed file from the bay,
/// so it would report missing at each boot.
///
/// # Errors
///
/// The sentence naming the file.
pub fn check_roles(manifest: &Manifest) -> Result<(), String> {
    let roles = role_files();
    for want in roles {
        if !manifest.boot_files.iter().any(|(name, _)| name == want) {
            return Err(format!(
                "boot.files has no {want}: a slot's boot files are the five {}",
                roles.join(", ")
            ));
        }
    }
    for (name, _) in &manifest.boot_files {
        if !roles.contains(&name.as_str()) {
            return Err(format!(
                "boot.files lists {name}, which is not one of the five boot files ({}): a file outside the five has no bay and would report missing at every boot",
                roles.join(", ")
            ));
        }
    }
    Ok(())
}

/// The command line's words as the installer reads them: the bundle's line without its line ending.
pub fn cmdline_line(text: &str) -> &str {
    text.trim_end_matches(['\r', '\n'])
}

/// The command line against the slot: it names its own slot and root hash with the board's
/// `boot_args` prefix, and each of the board's boot sources. `boot_sources` are the board file's
/// entries (`source=path`).
///
/// # Errors
///
/// The sentence naming the word that is missing.
pub fn check_cmdline(
    prefix: &str,
    id: &str,
    root_hash: &str,
    boot_sources: &[String],
    cmdline: &str,
) -> Result<(), String> {
    let has = |word: &str| cmdline.split_ascii_whitespace().any(|w| w == word);
    if !has(&format!("{prefix}.slot={id}")) {
        return Err(format!(
            "boot/cmdline.txt doesn't name its slot: it needs {prefix}.slot={id}"
        ));
    }
    if !has(&format!("{prefix}.roothash={root_hash}")) {
        return Err(format!(
            "boot/cmdline.txt doesn't carry the root hash slot.json names: it needs {prefix}.roothash={root_hash}"
        ));
    }
    for entry in boot_sources {
        let (source, _) = entry.split_once('=').unwrap_or((entry, ""));
        if !has(&format!("{prefix}.src.{entry}")) {
            return Err(format!(
                "boot/cmdline.txt doesn't list the board's boot source {source}: it needs {prefix}.src.{entry}"
            ));
        }
    }
    Ok(())
}

/// The kernel in the form the board's record stores: as shipped for the Pi firmware's form (gzip is
/// fine), raw for the U-Boot form.
///
/// # Errors
///
/// For the U-Boot form, a compressed kernel.
pub fn check_kernel(family: Family, kernel: &[u8]) -> Result<(), String> {
    match family {
        Family::Tryboot => Ok(()),
        Family::UbootBlock => bootimage::refuse_compressed_kernel(kernel),
    }
}

/// The five payloads against the board's bays, as the installer checks them before a write.
///
/// # Errors
///
/// The sentence naming the role whose payload is bigger than its bay.
pub fn check_bays(layout: &Layout, id: &str, payloads: &[&[u8]; 5]) -> Result<(), String> {
    bays::check_install(layout, 0, id, payloads).map_err(|e| e.to_string())
}

/// What the board's record needs of the command line: the U-Boot form composes it into one line of
/// its loader's file.
///
/// # Errors
///
/// For the U-Boot form, a command line that is not one line or does not fit.
pub fn check_loader_line(family: Family, cmdline: &[u8]) -> Result<(), String> {
    match family {
        Family::Tryboot => Ok(()),
        Family::UbootBlock => bootimage::extlinux_text(cmdline).map(|_| ()),
    }
}

/// What a board says about the slots it takes.
#[derive(Debug, Clone)]
pub struct Rules {
    /// The board file's `[board] id`: a slot's `board`.
    pub board: String,
    /// The `system-slots` device's `boot_args`: the command line's prefix.
    pub prefix: String,
    /// Its `boot_sources` entries.
    pub boot_sources: Vec<String>,
    /// The record's form.
    pub family: Family,
    /// The bays' sizes.
    pub layout: Layout,
    /// The board overlay's marker the manifest must carry (`boot.overlay`), `None` for a board
    /// whose overlay has none.
    pub overlay: Option<Overlay>,
}

/// Signs the manifest: the key's id and the signature over a message.
pub struct Signer<'a> {
    /// SHA-256 of the signing public key.
    pub key_id: [u8; 32],
    /// Ed25519 over the message it is given.
    pub sign: &'a dyn Fn(&[u8]) -> [u8; 64],
}

/// What the builder supplies, beside the board's rules.
pub struct Build<'a> {
    pub id: &'a str,
    pub product: &'a str,
    pub version: &'a str,
    pub variant: &'a str,
    pub kernel: &'a str,
    pub display_name: &'a str,
    /// The builder's dm-verity facts, which the root image is held to.
    pub table: &'a verity::Table,
    pub rules: &'a Rules,
    pub signer: Option<Signer<'a>>,
}

/// A bundle ready to write: every refusal has been made.
#[derive(Debug)]
pub struct Built {
    pub manifest: Manifest,
    /// `slot.json`'s bytes: what is signed.
    pub manifest_bytes: Vec<u8>,
    /// `slot.sig`'s bytes when signed.
    pub signature: Option<Vec<u8>>,
    /// The five boot files as the bundle holds them (the command line with its root hash), in role
    /// order.
    pub payloads: [Vec<u8>; 5],
    /// The whole image's length, tree included.
    pub image_len: u64,
    /// The image's length before the tree.
    pub data_len: u64,
}

/// The erofs superblock's magic, at byte 1024 of the image.
const EROFS_MAGIC: [u8; 4] = [0xe2, 0xe1, 0xf5, 0xe0];

/// The image is an erofs image of 4096-byte blocks written whole: its superblock says its block
/// count, and the file is that many blocks. An image with a hash tree already appended, or another
/// filesystem, is refused.
///
/// # Errors
///
/// The sentence naming what differs.
pub fn check_erofs(head: &[u8], len: u64) -> Result<(), String> {
    if !len.is_multiple_of(BLOCK) {
        return Err(format!(
            "the root image is {len} bytes, not a whole number of {BLOCK}-byte blocks: write it with mkfs.erofs -b {BLOCK}"
        ));
    }
    let sb = head.get(1024..1024 + 40);
    let Some(sb) = sb.filter(|sb| sb[..4] == EROFS_MAGIC) else {
        return Err(
            "the root image is not an erofs image (no erofs superblock at byte 1024)".into(),
        );
    };
    if sb[12] != 12 {
        return Err(format!(
            "the root image's block size is 2^{} bytes: the slot's dm-verity takes {BLOCK} (mkfs.erofs -b {BLOCK})",
            sb[12]
        ));
    }
    let blocks = u64::from(u32::from_le_bytes([sb[36], sb[37], sb[38], sb[39]]));
    if blocks * BLOCK != len {
        return Err(format!(
            "the root image's superblock says {blocks} blocks ({} bytes) and the file is {len} bytes: give the erofs image as mkfs.erofs wrote it, with no hash tree after it (the bundle tool appends the tree)",
            blocks * BLOCK
        ));
    }
    Ok(())
}

/// The command line with the root hash in its hole: `<prefix>.roothash=@roothash@` becomes
/// `<prefix>.roothash=<root_hash>`; every other byte is as it was.
///
/// # Errors
///
/// A line that is not text.
pub fn fill_roothash(cmdline: &[u8], prefix: &str, root_hash: &str) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(cmdline).map_err(|_| "boot/cmdline.txt is not text")?;
    let hole = format!("{prefix}.roothash={ROOTHASH_HOLE}");
    let word = format!("{prefix}.roothash={root_hash}");
    let mut out = String::with_capacity(text.len() + 64);
    let mut rest = text;
    while let Some(at) = rest.find(&hole) {
        let before = rest[..at].chars().next_back();
        let after = rest[at + hole.len()..].chars().next();
        out.push_str(&rest[..at]);
        let whole = before.is_none_or(char::is_whitespace) && after.is_none_or(char::is_whitespace);
        out.push_str(if whole { &word } else { &hole });
        rest = &rest[at + hole.len()..];
    }
    out.push_str(rest);
    Ok(out.into_bytes())
}

const CHUNK: usize = 1 << 20;

/// Reads `rootfs` whole once and holds it to the table: its length, the erofs superblock over the
/// data, the tree after the data against the tree of the data, the root hash. Returns the image's
/// SHA-256.
fn hash_image<R: Read>(rootfs: &mut R, table: &verity::Table) -> Result<[u8; 32], String> {
    let mut sha = Sha256::new();
    let mut hasher = verity::Hasher::new(table.salt_bytes());
    let mut head = Vec::with_capacity(2048);
    let mut tail = Vec::new();
    let mut len = 0u64;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = rootfs
            .read(&mut buf)
            .map_err(|e| format!("the root image: {e}"))?;
        if n == 0 {
            break;
        }
        if head.len() < 2048 {
            head.extend_from_slice(&buf[..n.min(2048 - head.len())]);
        }
        sha.update(&buf[..n]);
        let in_data = usize::try_from(table.hash_offset.saturating_sub(len))
            .unwrap_or(usize::MAX)
            .min(n);
        hasher.update(&buf[..in_data]);
        if (tail.len() + n - in_data) as u64 <= table.tree_bytes {
            tail.extend_from_slice(&buf[in_data..n]);
        }
        len += n as u64;
    }
    if len != table.image_bytes {
        return Err(format!(
            "the root image is {len} bytes, and the verity table's image_bytes is {}: give the image with its hash tree appended, the table's own",
            table.image_bytes
        ));
    }
    check_erofs(&head, table.hash_offset)?;
    let tree = hasher
        .finish()
        .map_err(|e| format!("the root image: {e}"))?;
    if hex(&tree.root_hash) != table.root_hash {
        return Err(format!(
            "the root image's data hashes to root hash {}, and the verity table says {}",
            hex(&tree.root_hash),
            table.root_hash
        ));
    }
    if tree.tree != tail {
        return Err(
            "the hash tree after the root image's data is not the tree of that data".into(),
        );
    }
    Ok(sha.finalize().into())
}

/// Makes a bundle: checks everything the installer checks before it writes (and the names of the
/// slot and the builder's text), computes the hash tree, fills the command line's root hash, and
/// builds the manifest and its signature. `payloads` are the five boot files in role order.
///
/// # Errors
///
/// The sentence naming the first thing that is wrong; nothing has been written.
pub fn build<R: Read>(
    b: &Build<'_>,
    payloads: [Vec<u8>; 5],
    rootfs: &mut R,
) -> Result<Built, String> {
    let rules = b.rules;
    if !valid_id(b.id) {
        return Err(format!(
            "--slot {:?} isn't a slot id: lowercase letters, digits, '.', '_' and '-', not starting with '.', at most 64",
            b.id
        ));
    }
    if !valid_name(b.product) {
        return Err(format!(
            "--product {:?} isn't lowercase [a-z0-9-]",
            b.product
        ));
    }
    if b.version.is_empty() {
        return Err("--version is empty".into());
    }
    if !valid_name(&rules.board) {
        return Err(format!(
            "the board file's [board] id {:?} isn't lowercase [a-z0-9-], which a slot's board must be",
            rules.board
        ));
    }
    let family = rules.family;
    check_kernel(family, &payloads[0])?;
    let refs: [&[u8]; 5] = std::array::from_fn(|i| payloads[i].as_slice());
    check_bays(&rules.layout, b.id, &refs)?;
    b.table.check()?;
    let image_sha = hash_image(rootfs, b.table)?;
    let root_hash = b.table.root_hash.clone();
    let mut payloads = payloads;
    payloads[3] = fill_roothash(&payloads[3], &rules.prefix, &root_hash)?;
    let line = std::str::from_utf8(&payloads[3]).map_err(|_| "boot/cmdline.txt is not text")?;
    check_cmdline(
        &rules.prefix,
        b.id,
        &root_hash,
        &rules.boot_sources,
        cmdline_line(line),
    )?;
    check_loader_line(family, &payloads[3])?;
    let refs: [&[u8]; 5] = std::array::from_fn(|i| payloads[i].as_slice());
    check_bays(&rules.layout, b.id, &refs)?;
    let boot_files: Vec<(String, String)> = ROLES
        .iter()
        .zip(&payloads)
        .map(|(role, bytes)| (role.path.to_string(), hex(&Sha256::digest(bytes))))
        .collect();
    let manifest = Manifest {
        id: b.id.to_string(),
        product: b.product.to_string(),
        version: b.version.to_string(),
        variant: b.variant.to_string(),
        board: rules.board.clone(),
        kernel: b.kernel.to_string(),
        display_name: b.display_name.to_string(),
        image_size: b.table.image_bytes,
        image_sha256: hex(&image_sha),
        verity: Verity {
            root_hash,
            salt: b.table.salt.clone(),
            data_blocks: b.table.data_blocks,
            hash_offset: b.table.hash_offset,
        },
        boot_files,
        overlay: rules.overlay.clone(),
    };
    let manifest_bytes = manifest.to_json().into_bytes();
    // What is written is what is read: the manifest parses back to itself, or nothing is written.
    let text = std::str::from_utf8(&manifest_bytes).map_err(|_| "slot.json isn't text")?;
    if Manifest::parse(text).as_ref() != Ok(&manifest) {
        return Err(format!(
            "the manifest does not read back as it was made: {}",
            Manifest::parse(text).err().unwrap_or_default()
        ));
    }
    let signature = b.signer.as_ref().map(|signer| {
        let message = signing::slot_bundle_message(&manifest_bytes);
        signing::record_bytes(&signer.key_id, &(signer.sign)(&message))
    });
    Ok(Built {
        manifest,
        manifest_bytes,
        signature,
        payloads,
        image_len: b.table.image_bytes,
        data_len: b.table.hash_offset,
    })
}

/// Writes the bundle: `slot.json`, `slot.sig` when signed, `rootfs.erofs` (the image as it is read
/// again from `rootfs`), the `boot/` directory and the five boot files in role order.
/// The same bytes for the same inputs.
///
/// # Errors
///
/// A failed write, or a root image that is not what [`build`] read.
pub fn write_to<R: Read + Seek, W: Write>(
    built: &Built,
    rootfs: &mut R,
    out: &mut W,
) -> io::Result<()> {
    let member = |out: &mut W, name: &str, bytes: &[u8]| -> io::Result<()> {
        out.write_all(&write::header(name, b'0', bytes.len() as u64))?;
        out.write_all(bytes)?;
        out.write_all(&vec![0u8; write::padding(bytes.len() as u64)])
    };
    member(out, MANIFEST, &built.manifest_bytes)?;
    if let Some(signature) = &built.signature {
        member(out, SIGNATURE, signature)?;
    }
    let total = built.image_len;
    out.write_all(&write::header(ROOTFS, b'0', total))?;
    rootfs.seek(SeekFrom::Start(0))?;
    let mut sha = Sha256::new();
    let mut left = total;
    let mut buf = vec![0u8; CHUNK];
    while left > 0 {
        let want = usize::try_from(left).unwrap_or(CHUNK).min(CHUNK);
        rootfs.read_exact(&mut buf[..want]).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("the root image changed while it was written: {e}"),
            )
        })?;
        sha.update(&buf[..want]);
        out.write_all(&buf[..want])?;
        left -= want as u64;
    }
    if hex(&sha.finalize()) != built.manifest.image_sha256 {
        return Err(io::Error::other(
            "the root image changed while the bundle was made: its SHA-256 is not the one in slot.json",
        ));
    }
    out.write_all(&vec![0u8; write::padding(total)])?;
    out.write_all(&write::header("boot/", b'5', 0))?;
    for (role, bytes) in ROLES.iter().zip(&built.payloads) {
        member(out, &format!("boot/{}", role.path), bytes)?;
    }
    out.write_all(&write::END)
}

/// One result of [`verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub state: State,
    /// The check's name, a colon and what was found.
    pub text: String,
}

/// How a check came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// It holds.
    Ok,
    /// It does not: the bundle is refused.
    Bad,
    /// It was not made, for the reason the text gives.
    Skipped,
}

/// A bundle taken as the installer takes it, held for the checks: nothing is written anywhere.
#[derive(Default)]
pub struct Reader {
    entries: Vec<String>,
    manifest: Vec<u8>,
    signature: Vec<u8>,
    parsed: Option<Manifest>,
    current: Option<Current>,
    root: Option<Root>,
    boot: Vec<(String, Vec<u8>)>,
    hashes: Vec<(String, String)>,
    /// The check that failed while the bundle came, and why.
    pub failure: Option<(&'static str, String)>,
}

struct Current {
    what: Member,
    name: String,
    sha: Sha256,
    data: Vec<u8>,
}

struct Root {
    size: u64,
    hasher: Option<verity::Hasher>,
    hash_offset: u64,
    seen: u64,
    tail: Vec<u8>,
    tail_cap: u64,
}

impl Reader {
    fn fail<T>(&mut self, check: &'static str, why: String) -> Result<T, String> {
        let text = why.clone();
        self.failure.get_or_insert((check, why));
        Err(text)
    }
}

impl Sink for Reader {
    fn begin(&mut self, name: &str, kind: Kind, size: u64) -> Result<(), String> {
        let what = match check_order(&self.entries, name, kind) {
            Ok(what) => what,
            Err(why) => return self.fail("members", why),
        };
        if what == Member::Boot
            && kind == Kind::File
            && let Some(manifest) = &self.parsed
            && let Err(why) = check_listed(manifest, name.strip_prefix("boot/").unwrap_or(name))
        {
            return self.fail("members", why);
        }
        self.entries.push(name.to_string());
        if kind == Kind::Dir {
            if what != Member::Boot {
                return self.fail("members", format!("{name} is a directory in the bundle"));
            }
            return Ok(());
        }
        if let Err(why) = check_member_size(what, size) {
            return self.fail(
                if what == Member::Manifest {
                    "manifest"
                } else {
                    "signature"
                },
                why,
            );
        }
        if what == Member::Root {
            self.root = Some(match &self.parsed {
                Some(m) => {
                    let salt = unhex::<{ verity::SALT_LEN }>(&m.verity.salt);
                    Root {
                        size,
                        hasher: salt.map(verity::Hasher::new),
                        hash_offset: m.verity.hash_offset,
                        seen: 0,
                        tail: Vec::new(),
                        tail_cap: m.image_size.saturating_sub(m.verity.hash_offset),
                    }
                }
                None => Root {
                    size,
                    hasher: None,
                    hash_offset: 0,
                    seen: 0,
                    tail: Vec::new(),
                    tail_cap: 0,
                },
            });
        }
        self.current = Some(Current {
            what,
            name: name.to_string(),
            sha: Sha256::new(),
            data: Vec::new(),
        });
        Ok(())
    }

    fn data(&mut self, bytes: &[u8]) -> Result<(), String> {
        let Some(current) = &mut self.current else {
            return Ok(());
        };
        match current.what {
            Member::Manifest => self.manifest.extend_from_slice(bytes),
            Member::Signature => self.signature.extend_from_slice(bytes),
            Member::Root => {
                current.sha.update(bytes);
                if let Some(root) = &mut self.root {
                    let data_left = root.hash_offset.saturating_sub(root.seen);
                    let in_data = usize::try_from(data_left)
                        .unwrap_or(usize::MAX)
                        .min(bytes.len());
                    if let Some(hasher) = &mut root.hasher {
                        hasher.update(&bytes[..in_data]);
                    }
                    let rest = &bytes[in_data..];
                    if (root.tail.len() + rest.len()) as u64 <= root.tail_cap {
                        root.tail.extend_from_slice(rest);
                    }
                    root.seen += bytes.len() as u64;
                }
            }
            Member::Boot => {
                current.sha.update(bytes);
                current.data.extend_from_slice(bytes);
            }
        }
        Ok(())
    }

    fn end(&mut self) -> Result<(), String> {
        let Some(current) = self.current.take() else {
            return Ok(());
        };
        match current.what {
            Member::Manifest => {
                let parsed = match core::str::from_utf8(&self.manifest) {
                    Ok(text) => Manifest::parse(text),
                    Err(_) => Err("slot.json isn't text".into()),
                };
                let parsed = match parsed {
                    Ok(p) => p,
                    Err(why) => return self.fail("manifest", why),
                };
                if let Err(why) = check_slot_id(&parsed.id) {
                    return self.fail("manifest", why);
                }
                self.parsed = Some(parsed);
            }
            Member::Signature => {}
            Member::Root => {
                self.hashes
                    .push((current.name, hex(&current.sha.finalize())));
            }
            Member::Boot => {
                let sha = hex(&current.sha.finalize());
                self.hashes.push((current.name.clone(), sha));
                self.boot.push((current.name, current.data));
            }
        }
        Ok(())
    }
}

/// Checks `slot.sig` against the manifest's bytes: how it came out and the line to show.
pub type SignatureCheck<'a> = dyn Fn(&[u8], &[u8]) -> (State, String) + 'a;

/// What [`verify`] is given beside the bundle.
pub struct Context<'a> {
    /// The board's rules, for the board's own checks; without them those are skipped.
    pub rules: Option<&'a Rules>,
    /// Checks `slot.sig` (its bytes, empty when the bundle has none) against the manifest's bytes:
    /// how it came out, and the line to show after `signature: `.
    pub signature: &'a SignatureCheck<'a>,
}

fn found(state: State, check: &str, text: impl std::fmt::Display) -> Finding {
    Finding {
        state,
        text: format!("{check}: {text}"),
    }
}

/// Takes the bundle `bundle` as the installer does and reports each check, writing nothing. The
/// checks are the installer's own functions of this module: framing and member order, the manifest,
/// the signature, the image and each boot file against the manifest, then, beyond what the installer
/// looks at, that the hash tree after the image recomputes to the manifest's root hash and that
/// `boot.files` is exactly the five role files; with the board's rules, the board, the kernel's form,
/// the bays, the command line and the overlay marker.
///
/// A bundle that does not frame or whose manifest does not parse stops at that check, since nothing
/// after it can be read; every later check reports whether or not an earlier one failed.
pub fn verify<R: Read>(bundle: &mut R, ctx: &Context<'_>) -> Vec<Finding> {
    let mut reader = Reader::default();
    let mut tar = Tar::default();
    let mut buf = vec![0u8; CHUNK];
    let mut framing: Result<(), String> = Ok(());
    loop {
        match bundle.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Err(why) = tar.feed(&buf[..n], &mut reader) {
                    framing = Err(why);
                    break;
                }
            }
            Err(e) => {
                framing = Err(format!("the bundle can't be read: {e}"));
                break;
            }
        }
    }
    let mut out = Vec::new();
    if let Some((check, why)) = reader.failure.take() {
        out.push(found(State::Bad, check, why));
        return out;
    }
    if let Err(why) = framing {
        out.push(found(State::Bad, "members", why));
        return out;
    }
    if !tar.finished() {
        out.push(found(
            State::Bad,
            "members",
            "the bundle ends in the middle",
        ));
        return out;
    }
    let Some(manifest) = reader.parsed.clone() else {
        out.push(found(
            State::Bad,
            "members",
            "not a slot bundle: no slot.json",
        ));
        return out;
    };
    let names: Vec<&str> = reader
        .entries
        .iter()
        .map(|n| if n == BOOT_DIR { "boot/" } else { n.as_str() })
        .filter(|n| !n.starts_with("boot/") || *n == "boot/")
        .collect();
    out.push(found(
        State::Ok,
        "members",
        format!(
            "{} in the enforced order, and {} boot files",
            names.join(", "),
            reader.boot.len()
        ),
    ));
    out.push(found(
        State::Ok,
        "manifest",
        format!(
            "slot {}, product {}, version {}, board {}, schema 3",
            manifest.id, manifest.product, manifest.version, manifest.board
        ),
    ));
    let (state, text) = (ctx.signature)(&reader.manifest, &reader.signature);
    out.push(found(state, "signature", text));
    let root_size = reader.root.as_ref().map(|r| r.size);
    match check_root(&manifest, root_size, &reader.hashes) {
        Ok(()) => out.push(found(
            State::Ok,
            "image",
            format!(
                "rootfs.erofs is {} bytes and its SHA-256 is slot.json's",
                manifest.image_size
            ),
        )),
        Err(why) => out.push(found(State::Bad, "image", why)),
    }
    out.push(verity_finding(&manifest, reader.root.take()));
    match check_boot(&manifest, &reader.hashes) {
        Ok(()) => out.push(found(
            State::Ok,
            "boot files",
            format!(
                "{} listed, each in the bundle with its SHA-256",
                manifest.boot_files.len()
            ),
        )),
        Err(why) => out.push(found(State::Bad, "boot files", why)),
    }
    match check_roles(&manifest) {
        Ok(()) => out.push(found(
            State::Ok,
            "roles",
            "boot.files is exactly the five boot files",
        )),
        Err(why) => out.push(found(State::Bad, "roles", why)),
    }
    match ctx.rules {
        None => out.push(found(
            State::Skipped,
            "board",
            "not checked: no --layout-of BOARD (the board id, kernel form, bays, command line and overlay are the board's)",
        )),
        Some(rules) => board_findings(&mut out, &manifest, &reader, rules),
    }
    out
}

fn verity_finding(manifest: &Manifest, root: Option<Root>) -> Finding {
    let Some(root) = root else {
        return found(
            State::Skipped,
            "verity",
            "not checked: the bundle has no rootfs.erofs",
        );
    };
    if root.seen != manifest.image_size {
        return found(
            State::Skipped,
            "verity",
            "not checked: the image is not the size slot.json says",
        );
    }
    let Some(hasher) = root.hasher else {
        return found(
            State::Bad,
            "verity",
            "slot.json's verity.salt isn't 64 hex digits",
        );
    };
    let tree = match hasher.finish() {
        Ok(tree) => tree,
        Err(why) => return found(State::Bad, "verity", why),
    };
    if tree.data_blocks != manifest.verity.data_blocks
        || tree_bytes(tree.data_blocks) != root.tail.len() as u64
    {
        return found(
            State::Bad,
            "verity",
            "the hash tree has not the shape slot.json says",
        );
    }
    if hex(&tree.root_hash) != manifest.verity.root_hash {
        return found(
            State::Bad,
            "verity",
            format!(
                "the image's root hash is {}, and slot.json says {}",
                hex(&tree.root_hash),
                manifest.verity.root_hash
            ),
        );
    }
    if tree.tree != root.tail {
        return found(
            State::Bad,
            "verity",
            "the hash tree after the image is not the tree of the image",
        );
    }
    found(
        State::Ok,
        "verity",
        format!(
            "the {} data blocks and the tree after them recompute to root hash {}",
            tree.data_blocks, manifest.verity.root_hash
        ),
    )
}

fn board_findings(out: &mut Vec<Finding>, manifest: &Manifest, reader: &Reader, rules: &Rules) {
    if manifest.board == rules.board {
        out.push(found(
            State::Ok,
            "board",
            format!("slot.json is for board {}", rules.board),
        ));
    } else {
        out.push(found(
            State::Bad,
            "board",
            format!(
                "this slot is built for board {}, and this is board {}",
                manifest.board, rules.board
            ),
        ));
    }
    let file = |path: &str| {
        reader
            .boot
            .iter()
            .find(|(name, _)| name.strip_prefix("boot/") == Some(path))
            .map(|(_, bytes)| bytes.as_slice())
    };
    let roles = role_files();
    let payloads: Vec<Option<&[u8]>> = roles.iter().map(|p| file(p)).collect();
    let missing = |check: &str, what: &str| {
        found(
            State::Skipped,
            check,
            format!("not checked: the bundle has no boot/{what}"),
        )
    };
    match payloads[0] {
        None => out.push(missing("kernel", roles[0])),
        Some(kernel) => match check_kernel(rules.family, kernel) {
            Ok(()) => out.push(found(
                State::Ok,
                "kernel",
                match rules.family {
                    Family::Tryboot => "stored as shipped on the Pi firmware's form",
                    Family::UbootBlock => "raw, as the U-Boot form takes it",
                },
            )),
            Err(why) => out.push(found(State::Bad, "kernel", why)),
        },
    }
    if payloads.iter().all(Option::is_some) {
        let refs: [&[u8]; 5] = std::array::from_fn(|i| payloads[i].unwrap_or_default());
        match check_bays(&rules.layout, &manifest.id, &refs) {
            Ok(()) => out.push(found(
                State::Ok,
                "bays",
                "each boot file fits the board's bay for it",
            )),
            Err(why) => out.push(found(State::Bad, "bays", why)),
        }
    } else {
        out.push(found(
            State::Skipped,
            "bays",
            "not checked: a boot file is missing",
        ));
    }
    match payloads[3] {
        None => out.push(missing("cmdline", roles[3])),
        Some(bytes) => {
            let checked = std::str::from_utf8(bytes)
                .map_err(|_| "boot/cmdline.txt is not text".to_string())
                .and_then(|text| {
                    check_cmdline(
                        &rules.prefix,
                        &manifest.id,
                        &manifest.verity.root_hash,
                        &rules.boot_sources,
                        cmdline_line(text),
                    )
                })
                .and_then(|()| check_loader_line(rules.family, bytes));
            match checked {
                Ok(()) => out.push(found(
                    State::Ok,
                    "cmdline",
                    format!(
                        "names {}.slot={} and {}.roothash={}, and the board's {} boot source(s)",
                        rules.prefix,
                        manifest.id,
                        rules.prefix,
                        manifest.verity.root_hash,
                        rules.boot_sources.len()
                    ),
                )),
                Err(why) => out.push(found(State::Bad, "cmdline", why)),
            }
        }
    }
    if manifest.overlay == rules.overlay {
        out.push(found(
            State::Ok,
            "overlay",
            match &rules.overlay {
                Some(o) => format!("boot.overlay is the board's marker ({})", o.sha256),
                None => "neither the board nor slot.json has an overlay marker".to_string(),
            },
        ));
    } else {
        out.push(found(
            State::Bad,
            "overlay",
            "boot.overlay is not the board's overlay marker: build the bundle with this board file",
        ));
    }
}

#[cfg(test)]
#[path = "bundle_tests.rs"]
mod tests;
