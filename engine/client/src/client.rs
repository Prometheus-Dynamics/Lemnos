use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lemnos_core::{Bound, Caller, Contract, Status};

use crate::DeviceDescription;
use crate::error::Error;
use crate::session::{DeviceEntry, Event, Handle, Session, Subscription, ValueInfo};
use crate::stream::{FrameStream, StreamPlatform};

const ARGS: usize = 256;

/// A connection that any number of threads and typed handles can share. Calls
/// run one at a time; open a second client for work that must not wait on the
/// first.
#[derive(Debug, Clone)]
pub struct Client {
    session: Arc<Mutex<Session>>,
}

impl Client {
    /// Connects, failing if no service is there.
    pub fn connect(path: impl AsRef<Path>, name: &str) -> Result<Self, Error> {
        Ok(Self::from(Session::connect(path, name)?))
    }

    /// Connects whenever the service appears.
    pub fn connecting(path: impl AsRef<Path>, name: &str) -> Self {
        Self::from(Session::connecting(path, name))
    }

    fn lock(&self) -> MutexGuard<'_, Session> {
        self.session.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn list(&self) -> Result<Vec<DeviceEntry>, Error> {
        self.lock().list()
    }

    /// The first device providing `tag`.
    pub fn find(&self, tag: &str) -> Result<Option<DeviceEntry>, Error> {
        self.lock().find(tag)
    }

    /// A typed client for contract `C` on the device called `name`, or on any
    /// device providing it when `name` is empty.
    pub fn open<C: Contract + ?Sized>(&self, name: &str) -> Result<Bound<C, RemoteCaller>, Error> {
        let handle = self.lock().open(name, C::TAG.as_str())?;
        Ok(Bound::new(RemoteCaller {
            session: Arc::clone(&self.session),
            handle,
            args: [0; ARGS],
        }))
    }

    /// An untyped handle, for tools that learn contracts from descriptions.
    pub fn open_tag(&self, name: &str, tag: &str) -> Result<Handle, Error> {
        self.lock().open(name, tag)
    }

    pub fn describe(&self, device: u32) -> Result<DeviceDescription, Error> {
        self.lock().describe(device)
    }

    pub fn call(
        &self,
        handle: &mut Handle,
        method: u16,
        args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize, Error> {
        self.lock().call(handle, method, args, reply)
    }

    pub fn subscribe(
        &self,
        handle: &mut Handle,
        method: u16,
        period_us: u64,
        only_changes: bool,
    ) -> Result<Subscription, Error> {
        self.lock()
            .subscribe(handle, method, period_us, only_changes)
    }

    pub fn unsubscribe(&self, subscription: u64) -> Result<(), Error> {
        self.lock().unsubscribe(subscription)
    }

    pub fn set_power(&self, device: u32, on: bool) -> Result<(), Error> {
        self.lock().set_power(device, on)
    }

    /// See [`Session::open_stream`].
    pub fn open_stream(
        &self,
        device: u32,
        name: &str,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<FrameStream, Error> {
        self.lock().open_stream(device, name, platform)
    }

    /// See [`Session::close_stream`].
    pub fn close_stream(&self, stream: FrameStream) -> Result<(), Error> {
        self.lock().close_stream(stream)
    }

    pub fn next_value(
        &self,
        into: &mut [u8],
        timeout: Duration,
    ) -> Result<Option<ValueInfo>, Error> {
        self.lock().next_value(into, timeout)
    }

    pub fn next_event(&self) -> Option<Event> {
        self.lock().next_event()
    }

    pub fn is_connected(&self) -> bool {
        self.lock().is_connected()
    }

    pub fn set_timeout(&self, timeout: Duration) {
        self.lock().set_timeout(timeout);
    }
}

impl From<Session> for Client {
    fn from(session: Session) -> Self {
        Self {
            session: Arc::new(Mutex::new(session)),
        }
    }
}

/// Carries a typed client's calls over the socket.
#[derive(Debug)]
pub struct RemoteCaller {
    session: Arc<Mutex<Session>>,
    handle: Handle,
    args: [u8; ARGS],
}

impl Caller for RemoteCaller {
    fn args(&mut self) -> &mut [u8] {
        &mut self.args
    }

    fn call(&mut self, method: u16, args_len: usize, reply: &mut [u8]) -> Result<usize, Status> {
        let args = self.args.get(..args_len).ok_or(Status::INVALID_ARGUMENT)?;
        self.session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .call(&mut self.handle, method, args, reply)
            .map_err(Status::from)
    }
}
