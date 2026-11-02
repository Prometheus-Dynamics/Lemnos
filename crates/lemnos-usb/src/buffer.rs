//! Transfer memory. A transfer moves its bytes through a buffer the USB layer
//! owns while the transfer is in flight.
//!
//! On Linux a buffer is either a heap vector or a usbdevfs mapping
//! (`USBDEVFS_CAP_MMAP`): page-aligned memory the kernel allocates for the
//! device file, which the host controller uses directly. Where the kernel
//! offers no mapping, or a mapping fails, the vector is the fallback. Other
//! platforms only have vectors (nusb copies them into its own buffers).

use crate::transfer::TransferBuffer;

/// The storage behind a [`TransferBuffer`].
#[cfg(target_os = "linux")]
pub(crate) use lemnos_linux_sys::usbfs::UrbBuffer as Storage;

/// The storage behind a [`TransferBuffer`]: heap memory only.
#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
pub(crate) struct Storage(Vec<u8>);

/// Heap storage over `bytes`.
#[cfg(not(target_os = "linux"))]
pub(crate) fn heap(bytes: Vec<u8>) -> Storage {
    Storage(bytes)
}

#[cfg(not(target_os = "linux"))]
impl Storage {
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub(crate) fn capacity(&self) -> usize {
        self.0.len()
    }
    pub(crate) fn is_mapped(&self) -> bool {
        false
    }
    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.0
    }
    pub(crate) fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.0
    }
    pub(crate) fn set_len(&mut self, len: usize) {
        self.0.resize(len, 0);
    }
    pub(crate) fn into_vec(self) -> Vec<u8> {
        self.0
    }
}

/// Heap storage over `bytes`.
#[cfg(target_os = "linux")]
pub(crate) fn heap(bytes: Vec<u8>) -> Storage {
    Storage::Heap(bytes)
}

/// Wraps storage as the public buffer.
pub(crate) fn wrap(storage: Storage) -> TransferBuffer {
    TransferBuffer { storage }
}

/// Opens the public buffer's storage (Linux submits it to usbdevfs).
#[cfg(target_os = "linux")]
pub(crate) fn unwrap(buffer: TransferBuffer) -> Storage {
    buffer.storage
}
