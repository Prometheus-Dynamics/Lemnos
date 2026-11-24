//! A bound gadget's FunctionFS files, kept in the service manager's file descriptor store so that a
//! restart of the daemon leaves the gadget on its controller.
//!
//! # Why
//!
//! FunctionFS takes the gadget off its controller when the last open file of its instance closes:
//! `ffs_data_closed` → `ffs_data_reset` → `ffs_closed` → `unregister_gadget_item` (f_fs.c, the same
//! in 6.12 and 7.x). The `no_disconnect` mount option only puts that off: the next open of `ep0`
//! finds the instance deactivated and resets it the same way, and the instance's superblock goes
//! with the daemon's mount namespace (`ffs_fs_kill_sb` resets it too). So a daemon that closes its
//! files, by stopping or by dying, takes every function of the gadget off the bus: the host sees the
//! device go, and the network function's link drops with it.
//!
//! The service manager keeps descriptors across a restart, and across a crash of the main process,
//! for as long as the unit is active or restarting (`FileDescriptorStoreMax`, the default
//! `FileDescriptorStorePreserve=restart`). While it holds a copy of `ep0` the instance is never
//! closed: the gadget stays bound, the host keeps its configuration, and the network function (a
//! kernel function, whose network device lives as long as its configfs directory) carries traffic
//! all through. The copies also keep the instance's mount and superblock alive after the daemon's
//! namespace has gone, so the daemon that starts next takes the gadget up by these descriptors,
//! never by a path: it mounts nothing, writes no descriptors and does not touch `UDC`.
//!
//! # What is kept
//!
//! For each bound gadget, under names `lemnos-ffs.CONTROLLER.PART`:
//!
//! - `record`: a memfd with the descriptor set the session was enabled with, what the setup said
//!   (serial, network refused), the endpoints and the lent pair, and the host's state as the daemon
//!   last saw it (configured, suspended, unbound), rewritten in place at each change;
//! - `ep0`, and `epN` for each endpoint file.
//!
//! A gadget the daemon takes down on purpose (its device let go, a set that changed) is taken out of
//! the store first, and the next enable on that controller waits until the store has closed its
//! copies (a barrier the store answers once it has acted on everything sent before), so a fresh
//! instance is never described while the old one is still open somewhere.
//!
//! # What can be lost
//!
//! Everything, on a stop: the service manager empties the store, the copies close, and the gadget
//! goes off the bus as it would without them. A stop is for shutdown only. A store too small for a
//! gadget's files drops the ones past its bound without telling the daemon; the next daemon finds
//! the set incomplete, lets the rest go and sets the gadget up again.
//!
//! The host's state is written after the daemon reads the event that changed it. A daemon killed
//! between the two leaves the record one change behind: the next daemon takes the session up as
//! the record says, and the host's next event puts it right. A request the host made of the
//! function while no daemon ran is still in `ep0`'s queue and is read by the next one; a data stage
//! that was under way when the daemon died is ended by the host's own timeout.

use std::fs::File;
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::fs::FileExt;

/// A service manager's file descriptor store, as the daemon reaches it (`sd_notify`'s `FDSTORE=1`,
/// `FDSTOREREMOVE=1` and `BARRIER=1`).
pub trait FdStore: Send + Sync {
    /// Gives a copy of `fd` to the store under `name`. `Ok(false)` when there is no store.
    ///
    /// # Errors
    ///
    /// The store is there and did not take it.
    fn keep(&self, name: &str, fd: BorrowedFd<'_>) -> io::Result<bool>;
    /// Asks the store to close every copy it keeps under `name`.
    fn let_go(&self, name: &str);
    /// A descriptor that reads end-of-file (polls as hung up) once the store has acted on
    /// everything sent to it before. `None` when there is no store, or it can't be asked.
    fn barrier(&self) -> Option<OwnedFd>;
}

/// The start of every name a gadget's files are kept under.
pub const NAME_PREFIX: &str = "lemnos-ffs.";

/// Whether a stored descriptor's name is one of a gadget's.
#[must_use]
pub fn is_kept_name(name: &str) -> bool {
    name.starts_with(NAME_PREFIX)
}

pub(super) fn record_name(udc: &str) -> String {
    format!("{NAME_PREFIX}{udc}.record")
}

/// `epN`'s name: `0` is `ep0`, `N` the plan's endpoint `N - 1`.
pub(super) fn file_name(udc: &str, ep: usize) -> String {
    format!("{NAME_PREFIX}{udc}.ep{ep}")
}

/// Every name a gadget with `endpoints` endpoint files keeps its files under.
pub(super) fn names(udc: &str, endpoints: usize) -> impl Iterator<Item = String> + '_ {
    std::iter::once(record_name(udc)).chain((0..=endpoints).map(move |ep| file_name(udc, ep)))
}

const MAGIC: &[u8; 8] = b"LMNFFS02";
const AT_STATE: u64 = 8;
const HEADER: usize = 29;
/// Where the request the host waits on an answer for is written: a flag, then its eight bytes.
const AT_REQUEST: u64 = 20;
const NO_LEND: u8 = 0xff;
const NO_ISO: u32 = u32::MAX;
/// The most endpoints a function has: 15 each way.
const MAX_ENDPOINTS: usize = 30;
/// As many endpoint names as a gadget can have kept, to let go of all of them.
pub(super) const MAX_KEPT_ENDPOINTS: usize = MAX_ENDPOINTS;

/// What the record says of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Recorded {
    pub(super) configured: bool,
    pub(super) suspended: bool,
    pub(super) released: bool,
    pub(super) serial: Option<bool>,
    pub(super) network_refused: bool,
    pub(super) endpoints: Vec<(u8, Option<u32>)>,
    pub(super) lend: Option<[usize; 2]>,
    pub(super) set: Vec<u8>,
    /// The control request the daemon had read and the host was still waiting on an answer
    /// for: its setup packet as it came (`bRequestType`, `bRequest`, then `wValue`, `wIndex` and
    /// `wLength` little-endian).
    pub(super) request: Option<[u8; 8]>,
}

/// A session's record, in a memfd kept with its files.
#[derive(Debug)]
pub(super) struct Record {
    file: File,
}

impl Record {
    /// A new record of `recorded`.
    pub(super) fn new(recorded: &Recorded) -> io::Result<Self> {
        let fd = lemnos_linux_sys::descriptor::sealable_memfd(c"lemnos-ffs-record", 0)?;
        let file = File::from(fd);
        let mut bytes =
            Vec::with_capacity(HEADER + recorded.endpoints.len() * 5 + recorded.set.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&state_bytes(
            recorded.configured,
            recorded.suspended,
            recorded.released,
        ));
        bytes.push(match recorded.serial {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        });
        bytes.push(u8::from(recorded.network_refused));
        let count = u8::try_from(recorded.endpoints.len())
            .ok()
            .filter(|&n| usize::from(n) <= MAX_ENDPOINTS)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        bytes.push(count);
        match recorded.lend {
            Some([out, input]) => {
                bytes.push(u8::try_from(out).unwrap_or(NO_LEND));
                bytes.push(u8::try_from(input).unwrap_or(NO_LEND));
            }
            None => bytes.extend_from_slice(&[NO_LEND, NO_LEND]),
        }
        let set_len = u32::try_from(recorded.set.len())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        bytes.extend_from_slice(&set_len.to_le_bytes());
        bytes.extend_from_slice(&request_bytes(recorded.request));
        for (address, iso) in &recorded.endpoints {
            bytes.push(*address);
            bytes.extend_from_slice(&iso.unwrap_or(NO_ISO).to_le_bytes());
        }
        bytes.extend_from_slice(&recorded.set);
        file.write_all_at(&bytes, 0)?;
        Ok(Self { file })
    }

    /// The record kept in `fd`, read back.
    ///
    /// # Errors
    ///
    /// `InvalidData` for a record this module did not write, or one cut short.
    pub(super) fn read(fd: OwnedFd) -> io::Result<(Self, Recorded)> {
        let file = File::from(fd);
        let bad = || io::Error::from(io::ErrorKind::InvalidData);
        let mut header = [0u8; HEADER];
        file.read_exact_at(&mut header, 0).map_err(|_| bad())?;
        if &header[..8] != MAGIC || header[8] > 1 || header[9] > 1 || header[10] > 1 {
            return Err(bad());
        }
        let serial = match header[11] {
            0 => None,
            1 => Some(false),
            2 => Some(true),
            _ => return Err(bad()),
        };
        let count = usize::from(header[13]);
        if count > MAX_ENDPOINTS {
            return Err(bad());
        }
        let lend = match (header[14], header[15]) {
            (NO_LEND, NO_LEND) => None,
            (out, input) if usize::from(out) < count && usize::from(input) < count => {
                Some([usize::from(out), usize::from(input)])
            }
            _ => return Err(bad()),
        };
        let set_len = u32::from_le_bytes([header[16], header[17], header[18], header[19]]) as usize;
        if set_len > 64 * 1024 {
            return Err(bad());
        }
        let request = match header[20] {
            0 => None,
            1 => {
                let mut setup = [0u8; 8];
                setup.copy_from_slice(&header[21..29]);
                Some(setup)
            }
            _ => return Err(bad()),
        };
        let mut rest = vec![0u8; count * 5 + set_len];
        file.read_exact_at(&mut rest, HEADER as u64)
            .map_err(|_| bad())?;
        let endpoints = rest[..count * 5]
            .as_chunks::<5>()
            .0
            .iter()
            .map(|e| {
                let iso = u32::from_le_bytes([e[1], e[2], e[3], e[4]]);
                (e[0], (iso != NO_ISO).then_some(iso))
            })
            .collect();
        let recorded = Recorded {
            configured: header[8] == 1,
            suspended: header[9] == 1,
            released: header[10] == 1,
            serial,
            network_refused: header[12] == 1,
            endpoints,
            lend,
            set: rest[count * 5..].to_vec(),
            request,
        };
        Ok((Self { file }, recorded))
    }

    /// The host's state now. One write of three bytes in place; a failure leaves the record as it
    /// was, which the next change writes over.
    pub(super) fn note(&self, configured: bool, suspended: bool, released: bool) {
        let _ = self
            .file
            .write_all_at(&state_bytes(configured, suspended, released), AT_STATE);
    }

    /// The request the host waits on an answer for now, or none. One write of nine bytes in
    /// place, as [`Record::note`].
    pub(super) fn note_request(&self, request: Option<[u8; 8]>) {
        let _ = self.file.write_all_at(&request_bytes(request), AT_REQUEST);
    }

    pub(super) fn fd(&self) -> BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(&self.file)
    }
}

fn request_bytes(request: Option<[u8; 8]>) -> [u8; 9] {
    let mut bytes = [0u8; 9];
    if let Some(setup) = request {
        bytes[0] = 1;
        bytes[1..].copy_from_slice(&setup);
    }
    bytes
}

fn state_bytes(configured: bool, suspended: bool, released: bool) -> [u8; 3] {
    [
        u8::from(configured),
        u8::from(suspended),
        u8::from(released),
    ]
}

/// One gadget's files as the store gave them back.
#[derive(Default)]
pub(super) struct Given {
    pub(super) udc: String,
    pub(super) record: Option<OwnedFd>,
    /// `ep0` at 0, `epN` at N.
    pub(super) files: Vec<(usize, OwnedFd)>,
    /// Names given that are none of the above, or came twice.
    pub(super) strays: Vec<String>,
}

/// The gadgets' files among `fds`, each gadget's together, by the names they were kept under.
pub(super) fn sort(fds: Vec<(String, OwnedFd)>) -> Vec<Given> {
    let mut given: Vec<Given> = Vec::new();
    for (name, fd) in fds {
        let Some(rest) = name.strip_prefix(NAME_PREFIX) else {
            continue;
        };
        let Some((udc, part)) = rest.rsplit_once('.') else {
            continue;
        };
        let at = match given.iter().position(|g| g.udc == udc) {
            Some(at) => at,
            None => {
                given.push(Given {
                    udc: udc.to_string(),
                    ..Given::default()
                });
                given.len() - 1
            }
        };
        let entry = &mut given[at];
        if part == "record" && entry.record.is_none() {
            entry.record = Some(fd);
        } else if let Some(n) = part
            .strip_prefix("ep")
            .and_then(|n| n.parse::<usize>().ok())
            && n <= MAX_ENDPOINTS
            && !entry.files.iter().any(|(k, _)| *k == n)
        {
            entry.files.push((n, fd));
        } else {
            entry.strays.push(name);
        }
    }
    given
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorded() -> Recorded {
        Recorded {
            configured: true,
            suspended: false,
            released: false,
            serial: Some(true),
            network_refused: false,
            endpoints: vec![(0x01, None), (0x81, None), (0x82, Some(1024))],
            lend: Some([0, 1]),
            set: vec![1, 2, 3, 4, 5],
            request: None,
        }
    }

    #[test]
    fn a_record_reads_back_as_written_and_its_state_changes_in_place() {
        let record = Record::new(&recorded()).unwrap();
        record.note(false, true, true);
        let setup = [0xc1, 1, 0x34, 0x12, 0, 0, 64, 0];
        record.note_request(Some(setup));
        let copy = OwnedFd::from(record.file.try_clone().unwrap());
        let (_, back) = Record::read(copy).unwrap();
        let mut want = recorded();
        want.configured = false;
        want.suspended = true;
        want.released = true;
        want.request = Some(setup);
        assert_eq!(back, want);
        // Answered: gone from the record.
        record.note_request(None);
        let copy = OwnedFd::from(record.file.try_clone().unwrap());
        want.request = None;
        assert_eq!(Record::read(copy).unwrap().1, want);
    }

    #[test]
    fn a_record_this_module_did_not_write_is_refused() {
        let fd = lemnos_linux_sys::descriptor::sealable_memfd(c"other", 0).unwrap();
        let file = File::from(fd);
        file.write_all_at(b"not a record at all, just bytes", 0)
            .unwrap();
        let error = Record::read(OwnedFd::from(file)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        // Cut short after a good header.
        let record = Record::new(&recorded()).unwrap();
        record.file.set_len(HEADER as u64 + 3).unwrap();
        let error = Record::read(OwnedFd::from(record.file)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn stored_files_are_sorted_by_gadget_and_part() {
        let fd = || OwnedFd::from(File::open("/dev/null").unwrap());
        let given = sort(vec![
            ("lemnos-watchdog".into(), fd()),
            (record_name("dummy_udc.0"), fd()),
            (file_name("dummy_udc.0", 0), fd()),
            (file_name("dummy_udc.0", 2), fd()),
            (file_name("fe980000.usb", 1), fd()),
            (file_name("dummy_udc.0", 2), fd()),
            ("lemnos-ffs.dummy_udc.0.junk".into(), fd()),
        ]);
        assert_eq!(given.len(), 2);
        let first = &given[0];
        assert_eq!(first.udc, "dummy_udc.0");
        assert!(first.record.is_some());
        let mut eps: Vec<usize> = first.files.iter().map(|(n, _)| *n).collect();
        eps.sort_unstable();
        assert_eq!(eps, [0, 2]);
        assert_eq!(
            first.strays,
            [
                file_name("dummy_udc.0", 2),
                "lemnos-ffs.dummy_udc.0.junk".to_string()
            ]
        );
        assert_eq!(given[1].udc, "fe980000.usb");
        assert!(given[1].record.is_none());
        assert_eq!(
            names("u", 2).collect::<Vec<_>>(),
            [
                "lemnos-ffs.u.record",
                "lemnos-ffs.u.ep0",
                "lemnos-ffs.u.ep1",
                "lemnos-ffs.u.ep2"
            ]
        );
    }
}
