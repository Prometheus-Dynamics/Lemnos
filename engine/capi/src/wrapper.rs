//! A Rust client that goes through the C surface, so a program can share one
//! installed library instead of compiling the client in.

use std::ffi::CString;
use std::time::Duration;

use lemnos_abi::{Handle, MessageSink, Status};

use crate::ffi;
use crate::surface;

/// One value from a subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Value {
    pub subscription: u64,
    pub stamp_ns: u64,
    pub len: usize,
}

/// A connection to the service, through the C surface.
#[derive(Debug)]
pub struct Client {
    handle: Handle,
}

fn c_string(text: &str) -> Result<CString, Status> {
    CString::new(text).map_err(|_| Status::INVALID_ARGUMENT)
}

const fn checked(status: Status) -> Result<u32, Status> {
    if status.0 < 0 {
        Err(status)
    } else {
        Ok(status.0 as u32)
    }
}

impl Client {
    pub fn connect(socket: &str, name: &str, timeout: Duration) -> Result<Self, Status> {
        let version = surface::VERSION;
        // SAFETY: a valid version struct, read only during the call.
        checked(unsafe { ffi::lemnos_check_version(&version) })?;
        let (socket, name) = (c_string(socket)?, c_string(name)?);
        let mut handle = Handle::NULL;
        let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: both strings are NUL-terminated and outlive the call, and
        // `handle` is valid for a write.
        checked(unsafe {
            ffi::lemnos_client_open(
                socket.as_ptr(),
                name.as_ptr(),
                timeout_ms,
                &mut handle,
                MessageSink::NONE,
            )
        })?;
        Ok(Self { handle })
    }

    /// Refreshes the list and says how many devices it has.
    pub fn device_count(&self) -> Result<u32, Status> {
        checked(ffi::lemnos_device_count(self.handle))
    }

    pub fn device_name(&self, device: u32) -> Result<String, Status> {
        let mut bytes = [0u8; 256];
        let mut len = 0;
        // SAFETY: the buffer is valid for its length and `len` for a write.
        checked(unsafe {
            ffi::lemnos_device_text(
                self.handle,
                device,
                0,
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                &mut len,
            )
        })?;
        String::from_utf8(bytes[..len].to_vec()).map_err(|_| Status::IO)
    }

    /// Opens a contract, returning the number the other calls take.
    pub fn open(&self, device: &str, tag: &str) -> Result<u32, Status> {
        let (device, tag) = (c_string(device)?, c_string(tag)?);
        let mut opened = 0;
        // SAFETY: both strings are NUL-terminated and outlive the call, and
        // `opened` is valid for a write.
        checked(unsafe {
            ffi::lemnos_open(
                self.handle,
                device.as_ptr(),
                tag.as_ptr(),
                &mut opened,
                MessageSink::NONE,
            )
        })?;
        Ok(opened)
    }

    pub fn call(
        &self,
        opened: u32,
        method: u16,
        args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize, Status> {
        // SAFETY: both slices are valid for their lengths during the call.
        let n = checked(unsafe {
            ffi::lemnos_call(
                self.handle,
                opened,
                method,
                args.as_ptr(),
                args.len(),
                reply.as_mut_ptr(),
                reply.len(),
                MessageSink::NONE,
            )
        })?;
        Ok(n as usize)
    }

    /// Returns the subscription's id.
    pub fn subscribe(&self, opened: u32, method: u16, period_us: u64) -> Result<u64, Status> {
        let mut out = surface::Subscription {
            struct_size: size_of::<surface::Subscription>() as u32,
            ..Default::default()
        };
        // SAFETY: `out` is valid for its declared size during the call.
        checked(unsafe {
            ffi::lemnos_subscribe(
                self.handle,
                opened,
                method,
                period_us,
                0,
                &mut out,
                MessageSink::NONE,
            )
        })?;
        Ok(out.id)
    }

    pub fn next_value(&self, into: &mut [u8], timeout: Duration) -> Result<Option<Value>, Status> {
        let mut out = surface::Value {
            struct_size: size_of::<surface::Value>() as u32,
            ..Default::default()
        };
        let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: `out` is valid for its declared size and `into` for its
        // length during the call.
        let got = checked(unsafe {
            ffi::lemnos_next_value(
                self.handle,
                timeout_ms,
                &mut out,
                into.as_mut_ptr(),
                into.len(),
                MessageSink::NONE,
            )
        })?;
        Ok((got == 1).then_some(Value {
            subscription: out.subscription,
            stamp_ns: out.stamp_ns,
            len: out.len as usize,
        }))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = ffi::lemnos_client_close(self.handle);
    }
}
