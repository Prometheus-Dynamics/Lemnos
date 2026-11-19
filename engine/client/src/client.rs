use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lemnos_core::{Bound, Caller, Contract, JobId, Status};

use crate::DeviceDescription;
use crate::error::Error;
use crate::session::{
    DeviceEntry, Event, Handle, JobEvent, PackageList, Session, Subscription, SubscriptionState,
    UsbEndpoints, UsbEndpointsEnd, ValueInfo,
};
use crate::stream::{FrameStream, StreamPlatform};

const ARGS: usize = 256;

/// A connection that any number of threads and typed handles can share. Calls
/// run one at a time; open a second client for work that must not wait on the
/// first.
#[derive(Debug, Clone)]
pub struct Client {
    session: Arc<Mutex<Session>>,
    dropped_events: Arc<AtomicU64>,
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

    /// Connects, waiting up to `bound` for the service to appear. Retries on the same backoff as
    /// [`Client::connecting`], capped at 8 s. Fails with `Error::TimedOut` once the bound passes;
    /// a refusal by the service fails at once. A zero bound is [`Client::connect`].
    pub fn connect_within(
        path: impl AsRef<Path>,
        name: &str,
        bound: Duration,
    ) -> Result<Self, Error> {
        Ok(Self::from(Session::connect_within(path, name, bound)?))
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
    /// The device's description is read once, here, and checked against `C` (see
    /// [`bind`](crate::bind)): a device whose base methods differ, or that lacks a method or
    /// field `C` uses, is refused, and nothing is called on it.
    pub fn open<C: Contract + ?Sized>(&self, name: &str) -> Result<Bound<C, RemoteCaller>, Error> {
        let (handle, description) = {
            let mut session = self.lock();
            let handle = session.open(name, C::TAG.as_str())?;
            let description = session.describe(handle.device())?;
            (handle, description)
        };
        crate::bind::verify::<C>(&description)?;
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

    /// Asks for a subscription and returns its id at once. See [`Session::subscribe_pending`].
    pub fn subscribe_pending(
        &self,
        handle: &mut Handle,
        method: u16,
        period_us: u64,
        only_changes: bool,
    ) -> Result<u64, Error> {
        self.lock()
            .subscribe_pending(handle, method, period_us, only_changes)
    }

    /// See [`Session::subscription_state`].
    pub fn subscription_state(&self, id: u64) -> SubscriptionState {
        self.lock().subscription_state(id)
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

    /// See [`Session::usb_endpoints_end`].
    pub fn usb_endpoints_end(
        &self,
        device: u32,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<UsbEndpointsEnd, Error> {
        self.lock().usb_endpoints_end(device, platform)
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

    /// See [`Session::open_stream_held`].
    pub fn open_stream_held(
        &self,
        device: u32,
        name: &str,
        hold: u32,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<FrameStream, Error> {
        self.lock().open_stream_held(device, name, hold, platform)
    }

    /// See [`Session::open_stream_writable_held`].
    pub fn open_stream_writable_held(
        &self,
        device: u32,
        name: &str,
        hold: u32,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<FrameStream, Error> {
        self.lock()
            .open_stream_writable_held(device, name, hold, platform)
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

    /// See [`Session::open_stream_unmapped`].
    pub fn open_stream_unmapped(
        &self,
        device: u32,
        name: &str,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<FrameStream, Error> {
        self.lock().open_stream_unmapped(device, name, platform)
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

    /// See [`Session::wait_event`]. Holds the connection while it waits: other threads' calls
    /// wait too.
    pub fn wait_event(&self, timeout: Duration) -> Option<Event> {
        self.lock().wait_event(timeout)
    }

    /// How many device events this client has dropped since it was made. The queue keeps 64;
    /// past that the oldest event goes and is counted here, whether or not it was read. The count
    /// never goes down. Clones share it, and a reconnect does not reset it. It does not wait for a
    /// call in progress, so any thread may read it at any time. A consumer that sees it rise
    /// should list the devices again, since the changes it missed are not replayed.
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
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
            dropped_events: session.dropped_events_counter(),
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
