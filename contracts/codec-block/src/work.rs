//! A job's work as bytes: a [`WorkHeader`], then `refs` [`BufferRef`]s, then `items` items,
//! each an [`ItemHeader`] and its bytes. Everything is little-endian and packed, as this
//! crate's codec encodes it. [`Builder`] writes one and [`Work`] reads one back, checking
//! every count and length against the bytes there are, so that nothing after it has to.

use lemnos_core::{Decode, Encode, Reader, Writer};

use crate::{
    BUFFER_REF_BYTES, BufferRef, ITEM_HEADER_BYTES, ItemHeader, WORK_HEADER_BYTES, WorkHeader,
};

/// Writes a job's work into a caller's buffer: every reference first, then the items.
#[derive(Debug)]
pub struct Builder<'a> {
    out: &'a mut [u8],
    len: usize,
    refs: u16,
    items: u16,
}

impl<'a> Builder<'a> {
    /// Starts work in `out`. `None` when `out` can't hold a header.
    pub fn new(out: &'a mut [u8]) -> Option<Self> {
        (out.len() >= WORK_HEADER_BYTES).then_some(Self {
            out,
            len: WORK_HEADER_BYTES,
            refs: 0,
            items: 0,
        })
    }

    fn put(&mut self, value: &impl Encode, bytes: usize) -> Option<()> {
        let end = self.len.checked_add(bytes)?;
        let mut writer = Writer::new(self.out.get_mut(self.len..end)?);
        value.encode(&mut writer).ok()?;
        self.len = end;
        Some(())
    }

    /// Adds a buffer reference. `None`, and nothing added, when it doesn't fit or an item
    /// has been added already.
    pub fn reference(&mut self, reference: BufferRef) -> Option<()> {
        if self.items != 0 || self.refs == u16::MAX {
            return None;
        }
        self.put(&reference, BUFFER_REF_BYTES)?;
        self.refs += 1;
        Some(())
    }

    /// Adds an item. `None`, and nothing added, when it doesn't fit.
    pub fn item(&mut self, tag: u32, bytes: &[u8]) -> Option<()> {
        let size = u32::try_from(bytes.len()).ok()?;
        let end = self
            .len
            .checked_add(ITEM_HEADER_BYTES)?
            .checked_add(bytes.len())?;
        if end > self.out.len() || self.items == u16::MAX {
            return None;
        }
        self.put(&ItemHeader { tag, bytes: size }, ITEM_HEADER_BYTES)?;
        self.out.get_mut(self.len..end)?.copy_from_slice(bytes);
        self.len = end;
        self.items += 1;
        Some(())
    }

    /// Writes the header and returns how many bytes the work is.
    pub fn finish(self) -> usize {
        let header = WorkHeader {
            refs: self.refs,
            items: self.items,
        };
        let mut writer = Writer::new(&mut self.out[..WORK_HEADER_BYTES]);
        // The header's two fields fit the room checked in `new`.
        let _ = header.encode(&mut writer);
        self.len
    }
}

/// A job's work, checked: its references and items all lie inside the bytes, with nothing
/// left over.
#[derive(Debug, Clone, Copy)]
pub struct Work<'a> {
    refs: &'a [u8],
    items: &'a [u8],
    item_count: u16,
}

impl<'a> Work<'a> {
    /// Reads work from `bytes`, which must be exactly the work. `None` for bytes that are not
    /// work: a count or a length that runs past them, or bytes after the last item.
    pub fn parse(bytes: &'a [u8]) -> Option<Self> {
        let header = WorkHeader::decode(&mut Reader::new(bytes.get(..WORK_HEADER_BYTES)?)).ok()?;
        let refs_end = WORK_HEADER_BYTES + usize::from(header.refs) * BUFFER_REF_BYTES;
        let refs = bytes.get(WORK_HEADER_BYTES..refs_end)?;
        let items = bytes.get(refs_end..)?;
        // Every item is walked once here, so the iterator can't run past the bytes.
        let mut rest = items;
        for _ in 0..header.items {
            let item = ItemHeader::decode(&mut Reader::new(rest.get(..ITEM_HEADER_BYTES)?)).ok()?;
            let end = ITEM_HEADER_BYTES.checked_add(usize::try_from(item.bytes).ok()?)?;
            rest = rest.get(end..)?;
        }
        rest.is_empty().then_some(Self {
            refs,
            items,
            item_count: header.items,
        })
    }

    /// How many buffer references it has.
    #[must_use]
    pub fn ref_count(&self) -> usize {
        self.refs.len() / BUFFER_REF_BYTES
    }

    /// How many items it has.
    #[must_use]
    pub fn item_count(&self) -> usize {
        usize::from(self.item_count)
    }

    /// The buffer references, in order.
    pub fn refs(&self) -> impl Iterator<Item = BufferRef> + 'a {
        self.refs
            .as_chunks::<BUFFER_REF_BYTES>()
            .0
            .iter()
            .filter_map(|chunk| BufferRef::decode(&mut Reader::new(chunk)).ok())
    }

    /// The items, in order: each tag with its bytes.
    pub fn items(&self) -> impl Iterator<Item = (u32, &'a [u8])> + 'a {
        let mut rest = self.items;
        (0..self.item_count).filter_map(move |_| {
            let item = ItemHeader::decode(&mut Reader::new(rest.get(..ITEM_HEADER_BYTES)?)).ok()?;
            let end = ITEM_HEADER_BYTES + item.bytes as usize;
            let bytes = rest.get(ITEM_HEADER_BYTES..end)?;
            rest = rest.get(end..)?;
            Some((item.tag, bytes))
        })
    }
}
