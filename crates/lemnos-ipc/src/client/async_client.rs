//! A [`DeviceClient`] on Tokio's reactor (feature `tokio`): [`AsyncDeviceClient::next`] waits
//! on the client's socket through [`AsyncFd`] instead of a thread.

use super::poll::Polled;
use super::{ClientError, DeviceClient};
use crate::wire::RawReading;
use std::os::fd::{AsRawFd, RawFd};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;

/// A [`DeviceClient`] registered with the current Tokio reactor.
///
/// Requests go through [`get_mut`](Self::get_mut) ([`DeviceClient::send_read`] and the
/// others, which do not wait); [`next`](Self::next) waits for what comes back. A
/// reconnection's new socket is registered again inside `next`.
pub struct AsyncDeviceClient {
    /// Registered while connected; `None` only between `next`'s steps.
    fd: Option<AsyncFd<Registered>>,
    /// The client while it has no socket to register.
    idle: Option<DeviceClient>,
}

/// The client, with the descriptor it was registered under (deregistered under the
/// same number after a reconnection has replaced it).
struct Registered {
    client: DeviceClient,
    fd: RawFd,
}

impl std::fmt::Debug for AsyncDeviceClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncDeviceClient")
            .field("fd", &self.registered_fd())
            .finish_non_exhaustive()
    }
}

impl AsRawFd for Registered {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl AsyncDeviceClient {
    /// Puts `client` in polled use and registers it with the current reactor. Fails
    /// outside a Tokio runtime with IO enabled.
    pub fn new(mut client: DeviceClient) -> Result<Self, ClientError> {
        client.set_nonblocking(true)?;
        let mut this = Self {
            fd: None,
            idle: Some(client),
        };
        this.register()?;
        Ok(this)
    }

    /// The client: for requests, and for the blocking calls (a subscription), which
    /// hand the socket back non-blocking.
    pub fn get_mut(&mut self) -> &mut DeviceClient {
        match (&mut self.fd, &mut self.idle) {
            (Some(fd), _) => &mut fd.get_mut().client,
            (None, Some(client)) => client,
            (None, None) => unreachable!("the client is always held"),
        }
    }

    /// The client back, out of the reactor (still non-blocking; see
    /// [`DeviceClient::set_nonblocking`]).
    pub fn into_inner(mut self) -> DeviceClient {
        self.take()
    }

    /// Waits for what [`DeviceClient::poll`] finds next. Readings are decoded into
    /// `reading`. Cancel-safe: a dropped future loses nothing, since nothing is taken
    /// from the client while waiting.
    pub async fn next(&mut self, reading: &mut RawReading) -> Result<Polled, ClientError> {
        loop {
            if self.registered_fd() != self.get_mut().fd().map(|fd| fd.as_raw_fd()) {
                self.register()?;
            }
            if let Some(polled) = self.get_mut().poll(reading)? {
                if matches!(
                    polled,
                    Polled::Connected { .. } | Polled::Disconnected { .. }
                ) {
                    self.register()?;
                }
                return Ok(polled);
            }
            match self.fd.as_mut() {
                Some(fd) => {
                    let interest = if fd.get_ref().client.wants_write() {
                        Interest::READABLE | Interest::WRITABLE
                    } else {
                        Interest::READABLE
                    };
                    let mut guard = fd.ready_mut(interest).await?;
                    match guard.get_inner_mut().client.poll(reading)? {
                        Some(polled) => {
                            if matches!(
                                polled,
                                Polled::Connected { .. } | Polled::Disconnected { .. }
                            ) {
                                drop(guard);
                                self.register()?;
                            }
                            return Ok(polled);
                        }
                        None => guard.clear_ready(),
                    }
                }
                None => match self.get_mut().retry_at() {
                    Some(at) => tokio::time::sleep_until(at.into()).await,
                    None => return Err(ClientError::Closed),
                },
            }
        }
    }

    fn registered_fd(&self) -> Option<RawFd> {
        self.fd.as_ref().map(|fd| fd.get_ref().fd)
    }

    fn take(&mut self) -> DeviceClient {
        match (self.fd.take(), self.idle.take()) {
            (Some(fd), _) => fd.into_inner().client,
            (None, Some(client)) => client,
            (None, None) => unreachable!("the client is always held"),
        }
    }

    /// Registers the client's current socket (none while disconnected).
    fn register(&mut self) -> Result<(), ClientError> {
        let client = self.take();
        match client.fd().map(|fd| fd.as_raw_fd()) {
            Some(fd) => match AsyncFd::try_new(Registered { client, fd }) {
                Ok(registered) => self.fd = Some(registered),
                Err(e) => {
                    let (registered, error) = e.into_parts();
                    self.idle = Some(registered.client);
                    return Err(ClientError::Io(error));
                }
            },
            None => self.idle = Some(client),
        }
        Ok(())
    }
}
