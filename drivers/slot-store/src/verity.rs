//! The dm-verity hash tree of a slot's image, computed here so the builder never supplies a root
//! hash it could get wrong. The shape is the one `manifest` documents: format version 1, SHA-256,
//! 4096-byte data and hash blocks, no superblock, the tree stored after the data with its top level
//! first. A version 1 digest is `SHA-256(salt || block)`, which is what the kernel computes and
//! `veritysetup format --no-superblock` writes.
//!
//! [`Hasher`] takes the data in pieces of any size (the image is never held whole: only its 32-byte
//! digest per block is), and [`Hasher::finish`] gives the root hash and the tree's bytes.

use crate::manifest::{BLOCK, tree_bytes};
use sha2::{Digest, Sha256};

/// The salt's length: 32 bytes, 64 hex digits in the manifest.
pub const SALT_LEN: usize = 32;
/// SHA-256 digests in one hash block.
const PER_BLOCK: usize = BLOCK as usize / 32;

/// What the tree over an image is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    /// The root hash: the digest of the top hash block, or of the one data block when there is no tree.
    pub root_hash: [u8; 32],
    /// How many data blocks the tree covers.
    pub data_blocks: u64,
    /// The tree's bytes, top level first; empty for a single data block.
    pub tree: Vec<u8>,
}

fn digest(salt: &[u8; SALT_LEN], block: &[u8]) -> [u8; 32] {
    Sha256::new()
        .chain_update(salt)
        .chain_update(block)
        .finalize()
        .into()
}

/// Computes a tree from the data as it comes.
pub struct Hasher {
    salt: [u8; SALT_LEN],
    block: Vec<u8>,
    digests: Vec<[u8; 32]>,
}

impl Hasher {
    /// A hasher with `salt`.
    pub fn new(salt: [u8; SALT_LEN]) -> Self {
        Self {
            salt,
            block: Vec::with_capacity(BLOCK as usize),
            digests: Vec::new(),
        }
    }

    /// The next bytes of the data.
    pub fn update(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let take = (BLOCK as usize - self.block.len()).min(bytes.len());
            self.block.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.block.len() == BLOCK as usize {
                self.digests.push(digest(&self.salt, &self.block));
                self.block.clear();
            }
        }
    }

    /// The tree over everything given.
    ///
    /// # Errors
    ///
    /// When there is no data, or its length is not a whole number of 4096-byte blocks.
    pub fn finish(self) -> Result<Tree, String> {
        if !self.block.is_empty() {
            return Err(format!(
                "the image is not a whole number of {BLOCK}-byte blocks: {} bytes over",
                self.block.len()
            ));
        }
        let data_blocks = self.digests.len() as u64;
        if data_blocks == 0 {
            return Err("the image is empty".into());
        }
        let mut level = self.digests;
        if level.len() == 1 {
            return Ok(Tree {
                root_hash: level[0],
                data_blocks,
                tree: Vec::new(),
            });
        }
        // Each level is the digests of the one below, packed 128 to a block, the last block padded
        // with zeros; the levels stop at one block, whose digest is the root.
        let mut levels: Vec<Vec<u8>> = Vec::new();
        let root_hash = loop {
            let mut packed: Vec<u8> = level.iter().flatten().copied().collect();
            packed.resize(level.len().div_ceil(PER_BLOCK) * BLOCK as usize, 0);
            let top = packed.len() == BLOCK as usize;
            level = packed
                .chunks(BLOCK as usize)
                .map(|b| digest(&self.salt, b))
                .collect();
            levels.push(packed);
            if top {
                break level[0];
            }
        };
        let tree: Vec<u8> = levels.into_iter().rev().flatten().collect();
        debug_assert_eq!(tree.len() as u64, tree_bytes(data_blocks));
        Ok(Tree {
            root_hash,
            data_blocks,
            tree,
        })
    }
}

/// The tree over `data`, a whole number of blocks.
///
/// # Errors
///
/// As [`Hasher::finish`].
pub fn tree_of(salt: [u8; SALT_LEN], data: &[u8]) -> Result<Tree, String> {
    let mut hasher = Hasher::new(salt);
    hasher.update(data);
    hasher.finish()
}

/// The facts a builder's verity step writes, as `key value` lines (the `veritysetup` outputs): what
/// the bundle tool reads in place of a root hash it could get wrong. Required, each once:
/// `hash_type` (1), `algorithm` (sha256), `root_hash`, `salt` (64 hex), `data_blocks`,
/// `data_block_size` and `hash_block_size` (4096), `hash_offset`, `tree_bytes`, `image_bytes` and
/// `hash_start_block`. Other keys (`uuid`, `table`, ...) are ignored. [`Table::check`] holds the
/// numbers to each other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub root_hash: String,
    pub salt: String,
    pub data_blocks: u64,
    pub hash_offset: u64,
    pub tree_bytes: u64,
    pub image_bytes: u64,
}

impl Table {
    /// Reads the table's text.
    ///
    /// # Errors
    ///
    /// The sentence naming the key that is missing, given twice, malformed or not what the slot's
    /// dm-verity is (format 1, SHA-256, 4096-byte blocks), or a number that contradicts another.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut seen: Vec<(&str, &str)> = Vec::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let (key, value) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            if seen.iter().any(|(k, _)| *k == key) {
                return Err(format!("verity table: {key} is given twice"));
            }
            seen.push((key, value.trim()));
        }
        let get = |key: &str| -> Result<&str, String> {
            seen.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| *v)
                .ok_or_else(|| format!("verity table: {key} is missing"))
        };
        let number = |key: &str| -> Result<u64, String> {
            get(key)?
                .parse()
                .map_err(|_| format!("verity table: {key} is not a number"))
        };
        let fixed = |key: &str, want: u64| -> Result<(), String> {
            let n = number(key)?;
            if n == want {
                Ok(())
            } else {
                Err(format!(
                    "verity table: {key} is {n}, and the slot's dm-verity takes {want}"
                ))
            }
        };
        fixed("hash_type", 1)?;
        let algorithm = get("algorithm")?;
        if algorithm != "sha256" {
            return Err(format!(
                "verity table: algorithm is {algorithm}, and the slot's dm-verity takes sha256"
            ));
        }
        fixed("data_block_size", BLOCK)?;
        fixed("hash_block_size", BLOCK)?;
        let hex_of = |key: &str| -> Result<String, String> {
            let v = get(key)?;
            if crate::manifest::is_hex(v, 64) {
                Ok(v.to_string())
            } else {
                Err(format!(
                    "verity table: {key} is not 64 lowercase hex digits"
                ))
            }
        };
        let table = Self {
            root_hash: hex_of("root_hash")?,
            salt: hex_of("salt")?,
            data_blocks: number("data_blocks")?,
            hash_offset: number("hash_offset")?,
            tree_bytes: number("tree_bytes")?,
            image_bytes: number("image_bytes")?,
        };
        let start = number("hash_start_block")?;
        table.check()?;
        if table.hash_offset / BLOCK != start {
            return Err(format!(
                "verity table: hash_start_block is {start}, and hash_offset {} is block {}",
                table.hash_offset,
                table.hash_offset / BLOCK
            ));
        }
        Ok(table)
    }

    /// The numbers against each other: the tree follows the data, its size is what the manifest's
    /// own computation gives, and the image is both.
    ///
    /// # Errors
    ///
    /// The sentence naming what differs.
    pub fn check(&self) -> Result<(), String> {
        if self.data_blocks == 0 || self.data_blocks.checked_mul(BLOCK) != Some(self.hash_offset) {
            return Err(format!(
                "verity table: hash_offset {} is not data_blocks {} times {BLOCK}",
                self.hash_offset, self.data_blocks
            ));
        }
        if self.tree_bytes != tree_bytes(self.data_blocks) {
            return Err(format!(
                "verity table: tree_bytes is {}, and {} data blocks have a tree of {} bytes",
                self.tree_bytes,
                self.data_blocks,
                tree_bytes(self.data_blocks)
            ));
        }
        if self.hash_offset.checked_add(self.tree_bytes) != Some(self.image_bytes) {
            return Err(format!(
                "verity table: image_bytes is {}, and hash_offset plus tree_bytes is {}",
                self.image_bytes,
                self.hash_offset.saturating_add(self.tree_bytes)
            ));
        }
        Ok(())
    }

    /// The salt as bytes.
    pub fn salt_bytes(&self) -> [u8; SALT_LEN] {
        let mut out = [0u8; SALT_LEN];
        for (at, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&self.salt[at * 2..at * 2 + 2], 16).unwrap_or(0);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The documented data: block i is filled with the byte i mod 251.
    fn data(blocks: usize) -> Vec<u8> {
        (0..blocks)
            .flat_map(|i| vec![(i % 251) as u8; BLOCK as usize])
            .collect()
    }

    fn salt() -> [u8; SALT_LEN] {
        std::array::from_fn(|i| i as u8)
    }

    /// The vectors of `docs/slots.md`, which come from a real `veritysetup` 2.8.7 run
    /// (`format --no-superblock --data-block-size=4096 --hash-block-size=4096 --hash=sha256
    /// --salt=000102...1f --data-blocks=N`): the hash file's length and the root hash.
    const VECTORS: [(usize, usize, &str); 5] = [
        (
            1,
            0,
            "4ce3ecf32c133bf6321901b6092219474b6ac91a19d0304621d629e6bb9987dc",
        ),
        (
            2,
            4096,
            "bd780675f50010ed023fd1f07e98f029a6aaf25a8f6ee33ec94b4b4e847e4b2e",
        ),
        (
            128,
            4096,
            "acb37719c99f3ac554ecbbccb8910616915a08e5a67bd3b9778694c60e673da4",
        ),
        (
            129,
            12288,
            "6093a2333523050b628581510028976d3d3e9c62458642a83727e6df641397a4",
        ),
        (
            16384,
            528_384,
            "234a6801a6840c2defdf3a7c08060cf9874f5773c6360f4172b08ae891a611d5",
        ),
    ];

    #[test]
    fn the_tree_is_what_veritysetup_writes() {
        for (blocks, tree_len, root) in VECTORS {
            let tree = tree_of(salt(), &data(blocks)).unwrap();
            assert_eq!(tree.data_blocks, blocks as u64);
            assert_eq!(tree.tree.len(), tree_len, "{blocks}");
            assert_eq!(tree.tree.len() as u64, tree_bytes(blocks as u64));
            assert_eq!(hex(&tree.root_hash), root, "{blocks} blocks");
        }
    }

    #[test]
    fn pieces_of_any_size_give_the_same_tree() {
        let whole = data(300);
        let want = tree_of([1; SALT_LEN], &whole).unwrap();
        for piece in [1, 7, 4095, 4096, 4097, 100_000] {
            let mut hasher = Hasher::new([1; SALT_LEN]);
            for chunk in whole.chunks(piece) {
                hasher.update(chunk);
            }
            assert_eq!(hasher.finish().unwrap(), want, "piece {piece}");
        }
    }

    #[test]
    fn the_salt_and_every_data_byte_change_the_root_hash() {
        let base = tree_of([0; SALT_LEN], &data(5)).unwrap();
        assert_ne!(
            base.root_hash,
            tree_of([1; SALT_LEN], &data(5)).unwrap().root_hash
        );
        let mut flipped = data(5);
        flipped[3 * BLOCK as usize + 17] ^= 1;
        assert_ne!(
            base.root_hash,
            tree_of([0; SALT_LEN], &flipped).unwrap().root_hash
        );
    }

    #[test]
    fn a_partial_block_and_no_data_are_refused() {
        let why = tree_of([0; SALT_LEN], &[0; 4097]).unwrap_err();
        assert!(why.contains("whole number"), "{why}");
        assert_eq!(
            tree_of([0; SALT_LEN], &[]).unwrap_err(),
            "the image is empty"
        );
    }
}

#[cfg(test)]
mod table_tests {
    use super::*;

    const H: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// What a builder's verity step writes for 300 data blocks.
    fn text() -> String {
        format!(
            "hash_type 1\nalgorithm sha256\nroot_hash {H}\nsalt {H}\nuuid 8b3c0e1a-0000-4000-8000-000000000000\n\
             data_blocks 300\ndata_block_size 4096\nhash_block_size 4096\nhash_offset 1228800\ntree_bytes 16384\n\
             image_bytes 1245184\nhash_start_block 300\ntable 0 2400 verity 1 /dev/x /dev/x 4096 4096 300 300 sha256 {H} {H}\n"
        )
    }

    #[test]
    fn the_builders_table_reads_and_each_contradiction_is_refused_by_name() {
        let t = Table::parse(&text()).unwrap();
        assert_eq!(
            (t.data_blocks, t.hash_offset, t.tree_bytes, t.image_bytes),
            (300, 1_228_800, 16_384, 1_245_184)
        );
        let refused = |from: &str, to: &str| Table::parse(&text().replace(from, to)).unwrap_err();
        assert_eq!(
            refused("hash_type 1", "hash_type 0"),
            "verity table: hash_type is 0, and the slot's dm-verity takes 1"
        );
        assert_eq!(
            refused("algorithm sha256", "algorithm sha1"),
            "verity table: algorithm is sha1, and the slot's dm-verity takes sha256"
        );
        assert!(
            refused("data_block_size 4096", "data_block_size 512")
                .contains("data_block_size is 512")
        );
        assert!(
            refused("hash_block_size 4096", "hash_block_size 1024")
                .contains("hash_block_size is 1024")
        );
        assert!(
            refused("hash_offset 1228800", "hash_offset 1228801")
                .contains("is not data_blocks 300 times 4096")
        );
        assert!(
            refused("tree_bytes 16384", "tree_bytes 12288")
                .contains("tree_bytes is 12288, and 300 data blocks have a tree of 16384")
        );
        assert!(
            refused("image_bytes 1245184", "image_bytes 1245185")
                .contains("image_bytes is 1245185, and hash_offset plus tree_bytes is 1245184")
        );
        assert!(
            refused("hash_start_block 300", "hash_start_block 301")
                .contains("hash_start_block is 301")
        );
        assert!(refused(&format!("salt {H}"), "salt abc").contains("salt is not 64 lowercase hex"));
        assert_eq!(
            refused("data_blocks 300\n", ""),
            "verity table: data_blocks is missing"
        );
        assert_eq!(
            refused("data_blocks 300", "data_blocks x"),
            "verity table: data_blocks is not a number"
        );
        assert_eq!(
            Table::parse(&format!("{}hash_type 1\n", text())).unwrap_err(),
            "verity table: hash_type is given twice"
        );
    }
}
