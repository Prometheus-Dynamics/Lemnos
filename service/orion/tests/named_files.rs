//! The bridge's named-file snapshots over a fake node: what Orion would be sent.

use std::cell::RefCell;
use std::io::{self, Read, Seek, SeekFrom};
use std::rc::Rc;

use lemnos_orion::named::{Node, Snapshot};

/// A fake node: named files, its open snapshots, and how many were closed.
#[derive(Default)]
struct State {
    files: Vec<(String, Vec<u8>)>,
    open: Vec<(u64, Vec<u8>)>,
    next: u64,
    closed: u32,
    /// A read gives this many bytes at most (0 is a node that stopped).
    read_cap: Option<usize>,
    /// Whether `size` refuses.
    size_refused: bool,
}

#[derive(Clone, Default)]
struct Fake(Rc<RefCell<State>>);

impl Node for Fake {
    fn open(&mut self, name: &str) -> Result<u64, String> {
        let mut s = self.0.borrow_mut();
        let bytes = s
            .files
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.clone())
            .ok_or_else(|| "NOT_FOUND".to_string())?;
        s.next += 1;
        let handle = s.next;
        s.open.push((handle, bytes));
        Ok(handle)
    }

    fn size(&mut self, handle: u64) -> Result<u64, String> {
        let s = self.0.borrow();
        if s.size_refused {
            return Err("refused".into());
        }
        s.open
            .iter()
            .find(|(h, _)| *h == handle)
            .map(|(_, b)| b.len() as u64)
            .ok_or_else(|| "NOT_FOUND".to_string())
    }

    fn read(&mut self, handle: u64, from: u64, into: &mut [u8]) -> Result<usize, String> {
        let s = self.0.borrow();
        let bytes = &s
            .open
            .iter()
            .find(|(h, _)| *h == handle)
            .ok_or_else(|| "NOT_FOUND".to_string())?
            .1;
        if from as usize >= bytes.len() {
            return Ok(0);
        }
        let mut n = into.len().min(bytes.len() - from as usize);
        if let Some(cap) = s.read_cap {
            n = n.min(cap);
        }
        into[..n].copy_from_slice(&bytes[from as usize..from as usize + n]);
        Ok(n)
    }

    fn close(&mut self, handle: u64) -> Result<(), String> {
        let mut s = self.0.borrow_mut();
        s.open.retain(|(h, _)| *h != handle);
        s.closed += 1;
        Ok(())
    }
}

fn node_with(name: &str, bytes: &[u8]) -> Fake {
    let fake = Fake::default();
    fake.0
        .borrow_mut()
        .files
        .push((name.to_string(), bytes.to_vec()));
    fake
}

#[test]
fn a_name_without_the_prefix_is_refused_and_nothing_is_opened() {
    let fake = node_with("lemnos/x", b"a");
    let error = Snapshot::open(Box::new(fake.clone()), "system/kernel-log")
        .err()
        .unwrap();
    assert!(error.contains("not a `lemnos/` name"), "{error}");
    assert!(fake.0.borrow().open.is_empty());
}

#[test]
fn a_name_the_node_refuses_is_an_error_with_its_reason() {
    let fake = node_with("lemnos/x", b"a");
    let error = Snapshot::open(Box::new(fake), "lemnos/missing")
        .err()
        .unwrap();
    assert_eq!(error, "NOT_FOUND");
}

#[test]
fn the_whole_file_reads_back_exactly_and_the_snapshot_closes_on_drop() {
    let content: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let fake = node_with("lemnos/tree/overlay.dts", &content);
    let mut snapshot = Snapshot::open(Box::new(fake.clone()), "lemnos/tree/overlay.dts").unwrap();
    assert_eq!(snapshot.size(), content.len() as u64);
    let mut out = Vec::new();
    snapshot.read_to_end(&mut out).unwrap();
    assert_eq!(out, content);
    drop(snapshot);
    assert_eq!(fake.0.borrow().closed, 1, "closed once, when dropped");
    assert!(fake.0.borrow().open.is_empty());
}

#[test]
fn the_bytes_are_those_the_node_held_when_the_file_was_opened() {
    let fake = node_with("lemnos/trial-reason", b"first");
    let mut snapshot = Snapshot::open(Box::new(fake.clone()), "lemnos/trial-reason").unwrap();
    // The node's file changes after the snapshot: the transfer still reads what it began with.
    fake.0.borrow_mut().files[0].1 = b"later reason".to_vec();
    let mut out = Vec::new();
    snapshot.read_to_end(&mut out).unwrap();
    assert_eq!(out, b"first");
}

#[test]
fn a_transfer_resumes_from_its_offset() {
    let fake = node_with("lemnos/boots", b"0123456789");
    let snapshot = Snapshot::open(Box::new(fake), "lemnos/boots").unwrap();
    let mut resumed = snapshot.at(4).unwrap();
    let mut out = Vec::new();
    resumed.read_to_end(&mut out).unwrap();
    assert_eq!(out, b"456789");
}

#[test]
fn seeking_past_the_end_reads_nothing_and_a_negative_seek_is_invalid() {
    let fake = node_with("lemnos/boots", b"abc");
    let mut snapshot = Snapshot::open(Box::new(fake), "lemnos/boots").unwrap();
    assert_eq!(snapshot.seek(SeekFrom::Start(99)).unwrap(), 99);
    let mut buf = [0u8; 4];
    assert_eq!(snapshot.read(&mut buf).unwrap(), 0);
    let error = snapshot.seek(SeekFrom::End(-10)).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn a_node_that_stops_before_the_size_is_an_error_not_an_end() {
    let fake = node_with("lemnos/boots", b"abcdef");
    fake.0.borrow_mut().read_cap = Some(0);
    let mut snapshot = Snapshot::open(Box::new(fake), "lemnos/boots").unwrap();
    let mut out = Vec::new();
    let error = snapshot.read_to_end(&mut out).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn a_size_the_node_refuses_closes_the_snapshot_it_took() {
    let fake = node_with("lemnos/boots", b"abc");
    fake.0.borrow_mut().size_refused = true;
    let error = Snapshot::open(Box::new(fake.clone()), "lemnos/boots")
        .err()
        .unwrap();
    assert_eq!(error, "refused");
    assert_eq!(
        fake.0.borrow().closed,
        1,
        "the snapshot is closed, not left open"
    );
    assert!(fake.0.borrow().open.is_empty());
}
