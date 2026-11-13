//! The named files a node serves to a reader (`lemnos.files/1`, `docs/daemon.md`): the `lemnos/`
//! prefix, Lemnos's own knowledge of the board. Each file is a snapshot taken when a reader opens
//! it, read from any offset, and closed when the transfer ends. Nothing is collected in the
//! background, and nothing is kept across a restart: a reader asks again.
//!
//! Settings, from the device's `config`:
//!
//! - `data`: the data partition, `/data` by default (the boot history).
//! - `boot`: the boot partition, `/boot/firmware` by default (the trial reason).
//! - `sys_block`: the block devices, `/sys/block` by default (the storage life estimates).
//! - `module`: the lemnos module's directory, `/sys/module/lemnos` by default (`lemnos/kernel`).
//! - `tree_overlay`, `tree_boot`, `tree_modprobe`: the image's generated files, each a path. Unset,
//!   the name is refused (never an empty file).
//!
//! Which names a node serves is decided by what is there now: [`NamedFilesDriver`]'s index lists
//! them. Every name is capped at its bound; a file over it is refused, never cut short.

mod index;
pub mod producers;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use lemnos_contract_named_files::{MAX_OPEN, NamedFiles};
use lemnos_core::{Config, Lifecycle, Result, Status, driver};

pub use index::{INDEX, Line, NAMES, Name, parse as parse_index, render as render_index};
pub use producers::Paths;

/// A transfer's snapshot: its handle and the bytes fixed when it was opened.
struct Snapshot {
    handle: u64,
    bytes: Vec<u8>,
}

#[driver("named-files", provides = [NamedFiles])]
pub struct NamedFilesDriver {
    paths: Paths,
    open: Vec<Snapshot>,
    next: u64,
}

impl Lifecycle for NamedFilesDriver {
    type Needs = NamedFilesDriverNeeds;

    fn start(_needs: NamedFilesDriverNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            paths: paths(cfg),
            open: Vec::new(),
            next: 1,
        })
    }
}

fn paths(cfg: &Config<'_>) -> Paths {
    let optional = |key| {
        cfg.str(key)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
    };
    Paths {
        data: PathBuf::from(cfg.str("data").unwrap_or("/data")),
        boot: PathBuf::from(cfg.str("boot").unwrap_or("/boot/firmware")),
        sys_block: PathBuf::from(cfg.str("sys_block").unwrap_or("/sys/block")),
        module: PathBuf::from(cfg.str("module").unwrap_or("/sys/module/lemnos")),
        tree_overlay: optional("tree_overlay"),
        tree_boot: optional("tree_boot"),
        tree_modprobe: optional("tree_modprobe"),
    }
}

impl NamedFilesDriver {
    /// The names served now, in the index's order.
    fn served(&self) -> Vec<Name> {
        NAMES
            .into_iter()
            .filter(|name| producers::produce(*name, &self.paths).is_ok())
            .collect()
    }

    fn index_text(&self) -> Vec<u8> {
        render_index(&self.served()).into_bytes()
    }
}

impl NamedFiles for NamedFilesDriver {
    fn index(&mut self, into: &mut [u8]) -> Result<usize> {
        let text = self.index_text();
        let out = into.get_mut(..text.len()).ok_or(Status::BUFFER_TOO_SMALL)?;
        out.copy_from_slice(&text);
        Ok(text.len())
    }

    fn open(&mut self, name: &str) -> Result<u64> {
        if self.open.len() >= MAX_OPEN {
            return Err(Status::BUSY);
        }
        let bytes = if name == INDEX {
            self.index_text()
        } else {
            let name = Name::named(name).ok_or(Status::NOT_FOUND)?;
            producers::produce(name, &self.paths)?
        };
        let handle = self.next;
        self.next += 1;
        self.open.push(Snapshot { handle, bytes });
        Ok(handle)
    }

    fn size(&mut self, handle: u64) -> Result<u64> {
        let open = self.find(handle)?;
        Ok(open.bytes.len() as u64)
    }

    fn read(&mut self, handle: u64, from: u64, into: &mut [u8]) -> Result<usize> {
        let open = self.find(handle)?;
        let len = open.bytes.len() as u64;
        if from >= len {
            return Ok(0);
        }
        let start = from as usize;
        let n = into.len().min(open.bytes.len() - start);
        into[..n].copy_from_slice(&open.bytes[start..start + n]);
        Ok(n)
    }

    fn close(&mut self, handle: u64) -> Result<()> {
        let at = self
            .open
            .iter()
            .position(|open| open.handle == handle)
            .ok_or(Status::NOT_FOUND)?;
        self.open.remove(at);
        Ok(())
    }
}

impl NamedFilesDriver {
    fn find(&mut self, handle: u64) -> Result<&Snapshot> {
        self.open
            .iter()
            .find(|open| open.handle == handle)
            .ok_or(Status::NOT_FOUND)
    }
}
