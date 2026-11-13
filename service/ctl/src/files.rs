//! `lemnos-ctl files`: the named files a node serves (`lemnos.files/1`, `docs/daemon.md`). With a
//! device only, its index. With a name, that file: the snapshot is taken once and read back in
//! pieces, so the bytes are what the node held when the transfer began. The file goes to stdout, or
//! to a new file the tool creates (it never overwrites one).

use std::fs::OpenOptions;
use std::io::Write;

use super::{Outcome, REPLY_MAX, Tool};

/// The contract this command speaks, and its method numbers (`lemnos-contract-named-files`).
const TAG: &str = "lemnos.files/1";
const INDEX: u16 = 1;
const OPEN: u16 = 2;
const SIZE: u16 = 3;
const READ: u16 = 4;
const CLOSE: u16 = 5;
/// How much one read asks for. The reply's bound is the frame limit, well above this.
const PIECE: usize = 32 << 10;

impl Tool<'_> {
    /// One call of the device's `lemnos.files/1`, its reply written into `reply`.
    fn files_call(
        &self,
        device: &str,
        method: u16,
        args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize, String> {
        let mut handle = self.client.open_tag(device, TAG).map_err(super::text)?;
        self.client
            .call(&mut handle, method, args, reply)
            .map_err(super::text)
    }

    /// The device's index, as its text.
    pub(super) fn files_index(&mut self, device: &str) -> Outcome {
        let mut reply = vec![0; REPLY_MAX];
        // An out-buffer method's last argument is the buffer's capacity.
        let capacity = (REPLY_MAX as u32).to_le_bytes();
        let n = self.files_call(device, INDEX, &capacity, &mut reply)?;
        self.out
            .write_all(&reply[..n])
            .map_err(|error| error.to_string())
    }

    /// One named file, to stdout or to `path`, which must not exist yet.
    pub(super) fn files_fetch(&mut self, device: &str, name: &str, path: Option<&str>) -> Outcome {
        let mut args = Vec::with_capacity(4 + name.len());
        args.extend_from_slice(&(name.len() as u32).to_le_bytes());
        args.extend_from_slice(name.as_bytes());
        let mut reply = [0u8; 8];
        let n = self.files_call(device, OPEN, &args, &mut reply)?;
        let handle = u64_of(&reply[..n])?;
        // The snapshot is closed whatever happened, and the first failure is the one reported.
        let bytes = self.files_pieces(device, handle);
        let closed = self.files_call(device, CLOSE, &handle.to_le_bytes(), &mut []);
        let bytes = bytes?;
        closed?;
        match path {
            Some(path) => {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .map_err(|error| format!("{path}: {error}"))?;
                file.write_all(&bytes)
                    .map_err(|error| format!("{path}: {error}"))
            }
            None => self
                .out
                .write_all(&bytes)
                .map_err(|error| error.to_string()),
        }
    }

    /// The snapshot `handle`, read in pieces to its size. A read that stops short is an error.
    fn files_pieces(&self, device: &str, handle: u64) -> Result<Vec<u8>, String> {
        let mut size = [0u8; 8];
        let n = self.files_call(device, SIZE, &handle.to_le_bytes(), &mut size)?;
        let size = u64_of(&size[..n])?;
        let mut bytes = Vec::with_capacity(size as usize);
        while (bytes.len() as u64) < size {
            // handle, from, then the out buffer's capacity.
            let mut args = [0u8; 20];
            args[..8].copy_from_slice(&handle.to_le_bytes());
            args[8..16].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
            args[16..].copy_from_slice(&(PIECE as u32).to_le_bytes());
            let mut piece = vec![0; PIECE];
            let n = self.files_call(device, READ, &args, &mut piece)?;
            if n == 0 {
                return Err(format!(
                    "the node stopped at {} of {size} bytes",
                    bytes.len()
                ));
            }
            bytes.extend_from_slice(&piece[..n]);
        }
        Ok(bytes)
    }
}

/// A reply of one little-endian `u64`.
fn u64_of(bytes: &[u8]) -> Result<u64, String> {
    let array: [u8; 8] = bytes
        .try_into()
        .map_err(|_| "the node sent a reply of the wrong size".to_string())?;
    Ok(u64::from_le_bytes(array))
}
