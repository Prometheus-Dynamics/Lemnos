//! A slot's manifest, `slot.json`, schema 3: what the bundle must hold and how its image is
//! opened through dm-verity.
//!
//! Keys:
//!
//! - `schema`: 3. Earlier schemas are refused.
//! - `id`: the slot id, `[A-Za-z0-9._-]`.
//! - `product`: the operating system's product name, lowercase `[a-z0-9-]`.
//! - `version`: the operating system's version, text.
//! - `variant`: optional text.
//! - `board`: the board the slot was built for, lowercase `[a-z0-9-]`: the board file's
//!   `[board] id` of the board that may run it.
//! - `kernel`, `display_name`: optional text, shown by `list`.
//! - `image.size`: `rootfs.erofs`'s size in bytes, its hash tree included.
//! - `image.sha256`: `rootfs.erofs`'s SHA-256, 64 lowercase hex digits.
//! - `verity.root_hash`: 64 lowercase hex digits.
//! - `verity.salt`: exactly 64 lowercase hex digits (32 bytes), unique per image. It may be derived
//!   from the image's content (a reproducible build then gives the same salt and root hash). It need
//!   not be secret or unpredictable: the image is public, the salt goes into the device-mapper
//!   table and every hash, and the root hash is what the signed manifest binds. An empty, missing
//!   or other-length salt is refused.
//! - `verity.data_blocks`: how many 4096-byte blocks of data the hash tree covers.
//! - `verity.hash_offset`: the byte offset in `rootfs.erofs` where the hash tree starts.
//! - `boot.files`: each boot file's path under `boot/` to its SHA-256.
//! - `boot.overlay` (optional): `{"board": ..., "sha256": ...}`, the board overlay's marker (see `Overlay`).
//!
//! Fixed by convention, so not in the manifest: dm-verity format version 1, SHA-256, 4096-byte
//! data and hash blocks, no verity superblock. The hash tree is appended to the data:
//! `hash_offset` is `data_blocks * 4096`, and `image.size` is `hash_offset` plus the tree's size
//! ([`tree_bytes`]). A manifest whose values don't fit these is refused.
//!
//! The tree's shape is what `veritysetup format --no-superblock` writes and what the kernel's
//! dm-verity reads: 128 SHA-256 digests to a 4096-byte block; the levels are `ceil(n / 128)`,
//! repeated while the count being hashed is more than 1, so a single data block has no tree at
//! all and the root hash is that block's own hash. In the hash area the levels are stored top
//! level first, down to the level over the data, which is the order the kernel computes for
//! each level's start from the table's hash start block (`hash_offset / 4096`).

use crate::json::{self, Json};

/// The schema this driver reads.
pub const SCHEMA: u64 = 3;
/// The data and hash block size.
pub const BLOCK: u64 = 4096;
/// SHA-256 digests in one hash block.
const PER_BLOCK: u64 = BLOCK / 32;

/// How dm-verity opens a slot's image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verity {
    pub root_hash: String,
    pub salt: String,
    pub data_blocks: u64,
    pub hash_offset: u64,
}

impl Verity {
    /// The device-mapper table line for the image on `device` (a loop device's `major:minor` or
    /// path), data and tree both on it.
    pub fn table(&self, device: &str) -> String {
        format!(
            "0 {} verity 1 {device} {device} {BLOCK} {BLOCK} {} {} sha256 {} {}",
            self.data_blocks * (BLOCK / 512),
            self.data_blocks,
            self.hash_offset / BLOCK,
            self.root_hash,
            self.salt,
        )
    }
}

/// What a schema 3 manifest says.
/// The board overlay a slot was built with (`boot.overlay`): the marker the live device tree carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    /// The board id the overlay names.
    pub board: String,
    /// SHA-256 of the overlay's text before its marker, 64 lowercase hex digits.
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub product: String,
    pub version: String,
    pub variant: String,
    pub board: String,
    pub kernel: String,
    pub display_name: String,
    pub image_size: u64,
    pub image_sha256: String,
    pub verity: Verity,
    /// Path under `boot/` to SHA-256.
    pub boot_files: Vec<(String, String)>,
    /// The board overlay the boot files carry, for a board whose firmware loads one (`boot.overlay`):
    /// the board id and the digest the overlay's live marker must equal. `None` when the slot has none.
    pub overlay: Option<Overlay>,
}

/// The size of the hash tree over `data_blocks` blocks: a level of `ceil(n / 128)` blocks while
/// the count being hashed is more than 1. One data block has no tree.
pub fn tree_bytes(data_blocks: u64) -> u64 {
    let (mut count, mut blocks) = (data_blocks, 0u64);
    while count > 1 {
        count = count.div_ceil(PER_BLOCK);
        blocks += count;
    }
    blocks * BLOCK
}

/// Lowercase hex of exactly `len` digits.
pub fn is_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A product or board name: lowercase `[a-z0-9-]`, not empty, at most 64.
pub fn valid_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl Manifest {
    /// The manifest's bytes: what the bundle holds as `slot.json` and what its signature covers, so
    /// they are fixed by this function alone. Compact JSON with no whitespace and no trailing
    /// newline; the keys in the order of the module's list (`schema`, `id`, `product`, `version`,
    /// `variant`, `board`, `kernel`, `display_name`, `image`, `verity`, `boot`), each of `variant`,
    /// `kernel` and `display_name` left out when empty, `boot` holding `overlay` (when there is one)
    /// and then `files` in the order given; numbers as plain decimal digits; text with only the
    /// escapes [`json::quoted`] writes. [`Manifest::parse`] reads it back to an equal manifest.
    pub fn to_json(&self) -> String {
        let mut out = String::from("{\"schema\":");
        out.push_str(&SCHEMA.to_string());
        let mut text = |key: &str, value: &str| {
            out.push_str(",\"");
            out.push_str(key);
            out.push_str("\":");
            json::quoted(value, &mut out);
        };
        text("id", &self.id);
        text("product", &self.product);
        text("version", &self.version);
        if !self.variant.is_empty() {
            text("variant", &self.variant);
        }
        text("board", &self.board);
        if !self.kernel.is_empty() {
            text("kernel", &self.kernel);
        }
        if !self.display_name.is_empty() {
            text("display_name", &self.display_name);
        }
        out.push_str(&format!(
            ",\"image\":{{\"size\":{},\"sha256\":\"{}\"}}",
            self.image_size, self.image_sha256
        ));
        out.push_str(&format!(
            ",\"verity\":{{\"root_hash\":\"{}\",\"salt\":\"{}\",\"data_blocks\":{},\"hash_offset\":{}}}",
            self.verity.root_hash, self.verity.salt, self.verity.data_blocks, self.verity.hash_offset
        ));
        out.push_str(",\"boot\":{");
        if let Some(overlay) = &self.overlay {
            out.push_str("\"overlay\":{\"board\":");
            json::quoted(&overlay.board, &mut out);
            out.push_str(&format!(",\"sha256\":\"{}\"}},", overlay.sha256));
        }
        out.push_str("\"files\":{");
        for (at, (name, sha)) in self.boot_files.iter().enumerate() {
            if at > 0 {
                out.push(',');
            }
            json::quoted(name, &mut out);
            out.push_str(&format!(":\"{sha}\""));
        }
        out.push_str("}}}");
        out
    }

    /// Reads and checks a manifest. The error is a sentence for the board's state.
    ///
    /// # Errors
    ///
    /// When a key is missing, isn't what it must be, or the image's numbers don't fit together.
    pub fn parse(text: &str) -> Result<Self, String> {
        let json = Json::parse(text).ok_or("slot.json isn't JSON")?;
        let schema = json.get("schema").and_then(Json::u64);
        if schema != Some(SCHEMA) {
            return Err(match schema {
                Some(n) => format!(
                    "slot.json is schema {n}; this board takes schema {SCHEMA} only: build the bundle again"
                ),
                None => format!("slot.json names no schema; this board takes schema {SCHEMA}"),
            });
        }
        let text_of = |path: &str| -> Result<String, String> {
            json.path(path)
                .and_then(Json::text)
                .map(str::to_string)
                .ok_or_else(|| format!("slot.json has no {path}"))
        };
        let optional = |path: &str| -> Result<String, String> {
            match json.path(path) {
                None => Ok(String::new()),
                Some(value) => value
                    .text()
                    .map(str::to_string)
                    .ok_or_else(|| format!("slot.json's {path} isn't text")),
            }
        };
        let number = |path: &str| -> Result<u64, String> {
            json.path(path)
                .ok_or_else(|| format!("slot.json has no {path}"))?
                .u64()
                .ok_or_else(|| format!("slot.json's {path} isn't a number"))
        };
        if json.get("family").is_some() {
            return Err("slot.json has a family key, which schema 3 doesn't have: it names product and board".into());
        }
        let id = text_of("id")?;
        let product = text_of("product")?;
        if !valid_name(&product) {
            return Err("slot.json's product isn't lowercase [a-z0-9-]".into());
        }
        let version = text_of("version")?;
        if version.is_empty() {
            return Err("slot.json's version is empty".into());
        }
        let board = text_of("board")?;
        if !valid_name(&board) {
            return Err("slot.json's board isn't lowercase [a-z0-9-]".into());
        }
        let image_size = number("image.size")?;
        let image_sha256 = text_of("image.sha256")?;
        if !is_hex(&image_sha256, 64) {
            return Err("slot.json's image.sha256 isn't 64 hex digits".into());
        }
        let root_hash = text_of("verity.root_hash")?;
        if !is_hex(&root_hash, 64) {
            return Err("slot.json's verity.root_hash isn't 64 hex digits".into());
        }
        let salt = text_of("verity.salt")?;
        if !is_hex(&salt, 64) {
            return Err("slot.json's verity.salt isn't 64 hex digits".into());
        }
        let data_blocks = number("verity.data_blocks")?;
        let hash_offset = number("verity.hash_offset")?;
        if data_blocks == 0 || data_blocks.checked_mul(BLOCK) != Some(hash_offset) {
            return Err(format!(
                "slot.json's verity.hash_offset must be verity.data_blocks times {BLOCK}: the tree follows the data"
            ));
        }
        if hash_offset.checked_add(tree_bytes(data_blocks)) != Some(image_size) {
            return Err(format!(
                "slot.json's image.size isn't the data and its hash tree: {} bytes",
                hash_offset.saturating_add(tree_bytes(data_blocks))
            ));
        }
        let boot_files = json
            .path("boot.files")
            .ok_or("slot.json has no boot.files")?
            .members()
            .iter()
            .map(|(name, sha)| match sha.text() {
                Some(sha) if is_hex(sha, 64) => Ok((name.clone(), sha.to_string())),
                _ => Err(format!(
                    "slot.json's hash of boot file {name} isn't 64 hex digits"
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        if boot_files.is_empty() {
            return Err("slot.json lists no boot files".into());
        }
        let overlay = match json.path("boot.overlay") {
            None => None,
            Some(o) => {
                let board = o
                    .get("board")
                    .and_then(|v| v.text())
                    .filter(|b| !b.is_empty())
                    .ok_or("slot.json's boot.overlay has no board")?;
                let sha = o
                    .get("sha256")
                    .and_then(|v| v.text())
                    .filter(|h| is_hex(h, 64))
                    .ok_or("slot.json's boot.overlay sha256 isn't 64 hex digits")?;
                Some(Overlay {
                    board: board.to_string(),
                    sha256: sha.to_string(),
                })
            }
        };
        Ok(Self {
            id,
            product,
            version,
            variant: optional("variant")?,
            board,
            kernel: optional("kernel")?,
            display_name: optional("display_name")?,
            image_size,
            image_sha256,
            verity: Verity {
                root_hash,
                salt,
                data_blocks,
                hash_offset,
            },
            boot_files,
            overlay,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn manifest(extra: &str, data_blocks: u64) -> String {
        let offset = data_blocks * BLOCK;
        format!(
            "{{\"schema\":3,\"id\":\"os-2\",\"product\":\"an-os\",\"version\":\"2.0\",\"board\":\"board-a\",{extra}\
             \"image\":{{\"size\":{},\"sha256\":\"{HASH}\"}},\
             \"verity\":{{\"root_hash\":\"{HASH}\",\"salt\":\"{HASH}\",\"data_blocks\":{data_blocks},\"hash_offset\":{offset}}},\
             \"boot\":{{\"files\":{{\"cmdline.txt\":\"{HASH}\"}}}}}}",
            offset + tree_bytes(data_blocks)
        )
    }

    fn whole() -> Manifest {
        Manifest {
            id: "os-2".into(),
            product: "an-os".into(),
            version: "2.0 \"rc\"".into(),
            variant: "lite".into(),
            board: "board-a".into(),
            kernel: "6.12".into(),
            display_name: "An OS".into(),
            image_size: 300 * BLOCK + tree_bytes(300),
            image_sha256: HASH.into(),
            verity: Verity {
                root_hash: HASH.into(),
                salt: HASH.into(),
                data_blocks: 300,
                hash_offset: 300 * BLOCK,
            },
            boot_files: vec![
                ("kernel8.img".into(), HASH.into()),
                ("overlays/lemnos-board.dtbo".into(), HASH.into()),
            ],
            overlay: Some(Overlay {
                board: "board-a".into(),
                sha256: HASH.into(),
            }),
        }
    }

    /// The manifest the writer makes reads back to the same manifest, with and without its optional
    /// keys, and an escaped text survives.
    #[test]
    fn a_serialized_manifest_parses_back_equal() {
        let full = whole();
        assert_eq!(Manifest::parse(&full.to_json()).unwrap(), full);
        let mut bare = whole();
        bare.variant.clear();
        bare.kernel.clear();
        bare.display_name.clear();
        bare.overlay = None;
        let text = bare.to_json();
        assert!(
            !text.contains("\"variant\"") && !text.contains("\"overlay\""),
            "{text}"
        );
        assert_eq!(Manifest::parse(&text).unwrap(), bare);
    }

    /// The bytes are what is signed: the key order, the number format and the lack of whitespace are
    /// fixed. A change of this text is a change of every signature over a manifest, so it is pinned
    /// literally.
    #[test]
    fn the_serialized_bytes_are_fixed() {
        let want = format!(
            "{{\"schema\":3,\"id\":\"os-2\",\"product\":\"an-os\",\"version\":\"2.0 \\\"rc\\\"\",\"variant\":\"lite\",\"board\":\"board-a\",\"kernel\":\"6.12\",\"display_name\":\"An OS\",\"image\":{{\"size\":{},\"sha256\":\"{HASH}\"}},\"verity\":{{\"root_hash\":\"{HASH}\",\"salt\":\"{HASH}\",\"data_blocks\":300,\"hash_offset\":1228800}},\"boot\":{{\"overlay\":{{\"board\":\"board-a\",\"sha256\":\"{HASH}\"}},\"files\":{{\"kernel8.img\":\"{HASH}\",\"overlays/lemnos-board.dtbo\":\"{HASH}\"}}}}}}",
            300 * BLOCK + tree_bytes(300)
        );
        assert_eq!(whole().to_json(), want);
        assert_eq!(whole().to_json(), whole().to_json());
    }

    #[test]
    fn a_board_overlay_is_read_when_it_is_listed() {
        let text = manifest("", 3).replace(
            "\"boot\":{\"files\"",
            &format!(
                "\"boot\":{{\"overlay\":{{\"board\":\"board-a\",\"sha256\":\"{HASH}\"}},\"files\""
            ),
        );
        let m = Manifest::parse(&text).unwrap();
        assert_eq!(
            m.overlay,
            Some(Overlay {
                board: "board-a".into(),
                sha256: HASH.into()
            })
        );
        assert_eq!(Manifest::parse(&manifest("", 3)).unwrap().overlay, None);
    }

    #[test]
    fn a_board_overlay_digest_must_be_64_hex_digits() {
        let text = manifest("", 3).replace(
            "\"boot\":{\"files\"",
            "\"boot\":{\"overlay\":{\"board\":\"board-a\",\"sha256\":\"xyz\"},\"files\"",
        );
        let why = Manifest::parse(&text).unwrap_err();
        assert!(why.contains("boot.overlay sha256"), "{why}");
    }

    #[test]
    fn the_tree_is_what_veritysetup_writes() {
        // `veritysetup format --no-superblock --data-block-size=4096 --hash-block-size=4096
        // --hash=sha256 --salt=<64 hex> --data-blocks=N` (2.8.7): the hash file's length.
        for (data_blocks, hash_file) in [
            (1, 0),
            (2, 4096),
            (128, 4096),
            (129, 12288),
            (16_384, 528_384),
            (16_385, 540_672),
            (38_400, 1_245_184),
        ] {
            assert_eq!(
                tree_bytes(data_blocks),
                hash_file,
                "{data_blocks} data blocks"
            );
        }
        // 1 GiB: 262144 blocks, 2048 + 16 + 1 hash blocks.
        assert_eq!(tree_bytes(262_144), 2065 * BLOCK);
    }

    #[test]
    fn a_schema_3_manifest_reads_whole() {
        let m = Manifest::parse(&manifest("\"variant\":\"lite\",", 300)).unwrap();
        assert_eq!(
            (
                m.product.as_str(),
                m.version.as_str(),
                m.board.as_str(),
                m.variant.as_str()
            ),
            ("an-os", "2.0", "board-a", "lite")
        );
        assert_eq!(m.verity.data_blocks, 300);
        assert_eq!(
            m.verity.table("7:0"),
            format!("0 2400 verity 1 7:0 7:0 4096 4096 300 300 sha256 {HASH} {HASH}")
        );
    }

    #[test]
    fn other_schemas_and_the_old_family_key_are_refused() {
        let two = manifest("", 3).replace("\"schema\":3", "\"schema\":2");
        assert!(Manifest::parse(&two).unwrap_err().contains("schema 2"));
        let none = manifest("", 3).replace("\"schema\":3,", "");
        assert!(Manifest::parse(&none).unwrap_err().contains("no schema"));
        let family = manifest("\"family\":\"x\",", 3);
        assert!(Manifest::parse(&family).unwrap_err().contains("family"));
    }

    #[test]
    fn product_and_board_are_lowercase_names_and_board_is_required() {
        let missing = manifest("", 3).replace("\"board\":\"board-a\",", "");
        assert_eq!(
            Manifest::parse(&missing).unwrap_err(),
            "slot.json has no board"
        );
        let upper = manifest("", 3).replace("board-a", "Board-A");
        assert!(Manifest::parse(&upper).unwrap_err().contains("board"));
        let product = manifest("", 3).replace("an-os", "an_os");
        assert!(Manifest::parse(&product).unwrap_err().contains("product"));
    }

    #[test]
    fn verity_numbers_that_dont_fit_are_refused() {
        let good = manifest("", 300);
        let offset = good.replace("\"hash_offset\":1228800", "\"hash_offset\":1232896");
        assert!(
            Manifest::parse(&offset)
                .unwrap_err()
                .contains("hash_offset")
        );
        let size = good.replace(
            &format!("\"size\":{}", 300 * BLOCK + tree_bytes(300)),
            "\"size\":5",
        );
        assert!(Manifest::parse(&size).unwrap_err().contains("image.size"));
        let salt = good.replace(&format!("\"salt\":\"{HASH}\""), "\"salt\":\"abc\"");
        assert!(Manifest::parse(&salt).unwrap_err().contains("salt"));
        let unsalted = good.replace(&format!("\"salt\":\"{HASH}\""), "\"salt\":\"\"");
        assert!(Manifest::parse(&unsalted).unwrap_err().contains("salt"));
        let short = good.replace(
            &format!("\"salt\":\"{HASH}\""),
            &format!("\"salt\":\"{}\"", &HASH[..32]),
        );
        assert!(Manifest::parse(&short).unwrap_err().contains("salt"));
        let missing = good.replace(&format!("\"salt\":\"{HASH}\","), "");
        assert!(Manifest::parse(&missing).unwrap_err().contains("salt"));
        let one = manifest("", 1);
        assert_eq!(
            Manifest::parse(&one).unwrap().image_size,
            BLOCK,
            "one block, no tree"
        );
        let zero = manifest("", 0);
        assert!(Manifest::parse(&zero).is_err());
    }
}
