use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lemnos_core::{Bound, Caller, Contract, JobId, Status};

use crate::DeviceDescription;
use crate::error::Error;
use crate::session::{
    DeviceEntry, Event, Handle, JobEvent, PackageList, Session, Subscription, UsbEndpoints,
    ValueInfo,
};
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

    /// What became of the service's driver packages: which were loaded, which refused and why.
    pub fn packages(&self) -> Result<PackageList, Error> {
        self.lock().packages()
    }

    /// The first device providing `tag`.
    pub fn find(&self, tag: &str) -> Result<Option<DeviceEntry>, Error> {
        self.lock().find(tag)
    }

    /// A typed client for contract `C` on the device called `name`, or on any
    /// device providing it when `name` is empty.
    ///
    /// The device's description is read once, here, and its hash for this contract is compared
    /// with `C::HASH`: a device whose argument or result types were laid out differently is
    /// refused, and nothing is called on it.
    pub fn open<C: Contract + ?Sized>(&self, name: &str) -> Result<Bound<C, RemoteCaller>, Error> {
        let (handle, description) = {
            let mut session = self.lock();
            let handle = session.open(name, C::TAG.as_str())?;
            let description = session.describe(handle.device())?;
            (handle, description)
        };
        if let Some(theirs) = description
            .contracts
            .iter()
            .find(|contract| contract.tag == C::TAG.as_str())
            .map(|contract| contract.hash)
            .filter(|theirs| *theirs != C::HASH)
        {
            return Err(Error::ContractMismatch {
                contract: C::TAG.as_str().to_string(),
                ours: C::HASH,
                theirs,
            });
        }
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

    /// See [`Session::start_job`].
    pub fn start_job(&self, handle: &mut Handle, method: u16, args: &[u8]) -> Result<u64, Error> {
        self.lock().start_job(handle, method, args)
    }

    /// See [`Session::cancel_job`].
    pub fn cancel_job(&self, job: u64) -> Result<(), Error> {
        self.lock().cancel_job(job)
    }

    /// See [`Session::wait_job`]. Holds the connection while it waits: other threads' calls
    /// wait too.
    pub fn wait_job(
        &self,
        job: u64,
        timeout: Duration,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Option<(Status, Vec<u8>)> {
        self.lock().wait_job(job, timeout, progress)
    }

    pub fn next_job_event(&self) -> Option<JobEvent> {
        self.lock().next_job_event()
    }

    /// See [`Session::wait_job_event`]. Holds the connection while it waits: other threads'
    /// calls wait too.
    pub fn wait_job_event(&self, timeout: Duration) -> Option<JobEvent> {
        self.lock().wait_job_event(timeout)
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

    /// See [`Session::usb_endpoints`].
    pub fn usb_endpoints(
        &self,
        device: u32,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<UsbEndpoints, Error> {
        self.lock().usb_endpoints(device, platform)
    }

    /// See [`Session::usb_endpoints_release`].
    pub fn usb_endpoints_release(&self, device: u32) -> Result<(), Error> {
        self.lock().usb_endpoints_release(device)
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

    /// See [`Session::open_stream_writable`].
    pub fn open_stream_writable(
        &self,
        device: u32,
        name: &str,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<FrameStream, Error> {
        self.lock().open_stream_writable(device, name, platform)
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

    fn start_job(&mut self, method: u16, args_len: usize) -> Result<JobId, Status> {
        let args = self.args.get(..args_len).ok_or(Status::INVALID_ARGUMENT)?;
        self.session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .start_job(&mut self.handle, method, args)
            .map(JobId)
            .map_err(Status::from)
    }
}
