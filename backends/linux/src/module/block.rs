//! A block bound to the kernel module, and the board-file names of what it has.
//!
//! Every name is a prefix and the block's device name as the module lists it (the platform device
//! bound to it), the same way `dma:<block>` names its DMA memory:
//!
//! | Name | What it is |
//! |---|---|
//! | `regs:<block>` or `regs:<block>/<n>` | Register region `n` (0 when left out), mapped |
//! | `irq:<block>` or `irq:<block>/<n>` | Interrupt `n` (0 when left out) |
//! | `clocks:<block>` or `clocks:<block>/<a>,<b>` | Clocks by name, index 0 first; none named is the block's only clock |
//! | `resets:<block>` or `resets:<block>/<a>,<b>` | Reset lines by name, the same way |
//!
//! Each is held under a module claim, so two holders of one of them can't exist, and each is
//! refused with no fallback: `NOT_FOUND` for a name that isn't one, `NOT_SUPPORTED` without the
//! module, `NO_DEVICE` for a block the module doesn't have bound.

use std::io;

use lemnos_hal::ErrorKind;
use lemnos_linux_sys::module::{self as sys, BlockInfo, POWER_NAME};

use super::Node;
use crate::kind_of;

/// `regs:`.
pub(crate) const REGS: &str = "regs:";
/// `irq:`.
pub(crate) const IRQ: &str = "irq:";
/// `clocks:`.
pub(crate) const CLOCKS: &str = "clocks:";
/// `resets:`.
pub(crate) const RESETS: &str = "resets:";

/// Clocks or reset lines one name can list: the module keeps at most this many per block.
pub(crate) const MAX_POWER: usize = 12;

/// Whether `block` can be a block's device name: not empty, shorter than the module's names, and
/// no NUL or `/`.
pub(crate) fn valid(block: &str) -> bool {
    !block.is_empty() && block.len() < 64 && !block.contains(['\0', '/'])
}

/// The block a name with `prefix` means, and what follows its `/`, if anything.
fn split<'a>(need: &'a str, prefix: &str) -> Option<(&'a str, Option<&'a str>)> {
    let rest = need.strip_prefix(prefix)?;
    let (block, after) = match rest.split_once('/') {
        Some((block, after)) => (block, Some(after)),
        None => (rest, None),
    };
    valid(block).then_some((block, after))
}

/// A `regs:` or `irq:` name: the block and the index, 0 when left out.
pub(crate) fn indexed<'a>(need: &'a str, prefix: &str) -> Option<(&'a str, u32)> {
    let (block, after) = split(need, prefix)?;
    let index = match after {
        None => 0,
        // Plain decimal only, so one index has one name.
        Some(text) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => {
            if text.len() > 1 && text.starts_with('0') {
                return None;
            }
            text.parse().ok()?
        }
        Some(_) => return None,
    };
    Some((block, index))
}

/// A `clocks:` or `resets:` name: the block and the names, in index order. No names is the
/// block's only one, which the module asks for with an empty name.
pub(crate) fn named<'a>(need: &'a str, prefix: &str) -> Option<(&'a str, Vec<&'a str>)> {
    let (block, after) = split(need, prefix)?;
    let Some(list) = after else {
        return Some((block, vec![""]));
    };
    let names: Vec<&str> = list.split(',').collect();
    let fits = |name: &&str| !name.is_empty() && name.len() < POWER_NAME && !name.contains('\0');
    (names.len() <= MAX_POWER && names.iter().all(fits)).then_some((block, names))
}

/// Whether `need` is any of the names above.
pub(crate) fn names_a_block(need: &str) -> Option<&str> {
    indexed(need, REGS)
        .or_else(|| indexed(need, IRQ))
        .map(|(block, _)| block)
        .or_else(|| named(need, CLOCKS).map(|(block, _)| block))
        .or_else(|| named(need, RESETS).map(|(block, _)| block))
}

/// The number and description of the block bound to the module as `name`: `Unavailable` when no
/// block of that name is bound, `Unsupported` when the module speaks another interface.
pub(crate) fn find(node: &Node, name: &str) -> Result<(u32, BlockInfo), ErrorKind> {
    let block = number(sys::find_block(node.fd(), name))?;
    let info = sys::block_info(node.fd(), block).map_err(|e| kind_of(&e))?;
    if info.abi != sys::ABI {
        return Err(ErrorKind::Unsupported);
    }
    Ok((block, info))
}

/// What looking a block up came to: its number, `Unavailable` when the module has no block of
/// that name bound (it doesn't own it, or it isn't bound now), or the failure.
fn number(found: io::Result<Option<u32>>) -> Result<u32, ErrorKind> {
    found
        .map_err(|e| kind_of(&e))?
        .ok_or(ErrorKind::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_names_are_a_block_and_a_number() {
        assert_eq!(
            indexed("regs:lemnos-test-block", REGS),
            Some(("lemnos-test-block", 0))
        );
        assert_eq!(
            indexed("regs:fdee0000.npu/2", REGS),
            Some(("fdee0000.npu", 2))
        );
        assert_eq!(indexed("irq:a/10", IRQ), Some(("a", 10)));
        for bad in [
            "regs:",
            "regs:/1",
            "regs:a/",
            "regs:a/x",
            "regs:a/-1",
            "regs:a/+1",
            "regs:a/01",
            "regs:a/1/2",
            "regs:a/99999999999",
            "regs:a\0b",
            "irq:a",
        ] {
            assert_eq!(indexed(bad, REGS), None, "{bad:?}");
        }
        assert_eq!(indexed(&format!("regs:{}", "x".repeat(64)), REGS), None);
    }

    #[test]
    fn named_names_are_a_block_and_its_names_in_order() {
        assert_eq!(named("clocks:blk", CLOCKS), Some(("blk", vec![""])));
        assert_eq!(
            named("clocks:blk/core,axi", CLOCKS),
            Some(("blk", vec!["core", "axi"]))
        );
        assert_eq!(named("resets:blk/rst", RESETS), Some(("blk", vec!["rst"])));
        let long = "x".repeat(POWER_NAME);
        let many = vec!["c"; MAX_POWER + 1].join(",");
        for bad in [
            "clocks:".to_string(),
            "clocks:blk/".to_string(),
            "clocks:blk/a,,b".to_string(),
            "clocks:blk/a,".to_string(),
            format!("clocks:blk/{long}"),
            format!("clocks:blk/{many}"),
            "resets:blk/a".to_string(),
        ] {
            assert_eq!(named(&bad, CLOCKS), None, "{bad:?}");
        }
        assert!(named(&format!("clocks:blk/{}", "x".repeat(31)), CLOCKS).is_some());
    }

    #[test]
    fn only_the_four_prefixes_name_a_block() {
        assert_eq!(names_a_block("regs:a"), Some("a"));
        assert_eq!(names_a_block("irq:a/1"), Some("a"));
        assert_eq!(names_a_block("clocks:a/core"), Some("a"));
        assert_eq!(names_a_block("resets:a"), Some("a"));
        assert_eq!(names_a_block("dma:a"), None);
        assert_eq!(names_a_block("i2c-1"), None);
        assert_eq!(names_a_block("regs:a/b"), None);
    }

    #[test]
    fn a_block_the_module_doesnt_have_is_no_device() {
        assert_eq!(number(Ok(Some(3))), Ok(3));
        assert_eq!(number(Ok(None)), Err(ErrorKind::Unavailable));
        assert_eq!(
            lemnos_core::status_of(ErrorKind::Unavailable),
            lemnos_core::Status::NO_DEVICE
        );
        let not_the_module = io::Error::from_raw_os_error(libc::ENOTTY);
        assert_eq!(number(Err(not_the_module)), Err(ErrorKind::Unsupported));
    }
}
