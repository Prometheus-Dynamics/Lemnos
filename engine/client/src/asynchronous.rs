use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use lemnos_wire::Frame;
use tokio::io::Interest;
use tokio::net::UnixStream as TokioStream;
use tokio::time::{Instant, timeout_at};

use crate::DeviceDescription;
use crate::error::Error;
use crate::session::{DeviceEntry, Handle, Mode, PackageList, Session, Subscription, ValueInfo};
use crate::stream::{FrameStream, StreamPlatform};

/// [`Session`] for a tokio program. It waits on the socket's readiness and
/// otherwise runs the same core.
#[derive(Debug)]
pub struct AsyncClient {
    session: Session,
    registered: Option<(u64, TokioStream)>,
}

impl AsyncClient {
    pub async fn connect(path: impl AsRef<Path>, name: &str) -> Result<Self, Error> {
        let mut client = Self {
            session: Session::new(path.as_ref(), name, Mode::Polled),
            registered: None,
        };
        client.settle().await?;
        Ok(client)
    }

    fn refresh_registration(&mut self) {
        let generation = self.session.generation();
        if self
            .registered
            .as_ref()
            .is_some_and(|(at, _)| *at == generation)
        {
            return;
        }
        self.registered = None;
        let Some(stream) = self.session.stream() else {
            return;
        };
        if let Ok(fd) = stream.try_clone().and_then(TokioStream::from_std) {
            self.registered = Some((generation, fd));
        }
    }

    /// Runs `step` until it finishes, waiting on the socket between tries.
    async fn until<R>(
        &mut self,
        timeout: Duration,
        mut step: impl FnMut(&mut Session) -> Option<R>,
    ) -> Result<R, Error> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(out) = step(&mut self.session) {
                return Ok(out);
            }
            self.refresh_registration();
            let interest = if self.session.has_unsent() {
                Interest::READABLE | Interest::WRITABLE
            } else {
                Interest::READABLE
            };
            if let Some((_, fd)) = &self.registered {
                let ready = timeout_at(deadline, fd.ready(interest))
                    .await
                    .map_err(|_| Error::TimedOut)?
                    .map_err(|_| Error::Disconnected)?;
                // The core does the reading, on the same socket. This tells
                // tokio that readiness was used.
                for wanted in [Interest::READABLE, Interest::WRITABLE] {
                    if (wanted == Interest::READABLE && ready.is_readable())
                        || (wanted == Interest::WRITABLE && ready.is_writable())
                    {
                        let _ = fd.try_io(wanted, || Err::<(), _>(ErrorKind::WouldBlock.into()));
                    }
                }
            } else {
                let pause = self.session.until_next_try().max(Duration::from_millis(1));
                timeout_at(deadline, tokio::time::sleep(pause))
                    .await
                    .map_err(|_| Error::TimedOut)?;
            }
        }
    }

    async fn settle(&mut self) -> Result<(), Error> {
        let timeout = self.session.timeout();
        self.until(timeout, |s| s.try_settle(None)).await?
    }

    async fn wait<R>(
        &mut self,
        id: u32,
        mut pick: impl FnMut(Frame<'_>) -> Option<Result<R, Error>>,
    ) -> Result<R, Error> {
        let generation = self.session.generation();
        let timeout = self.session.timeout();
        self.until(timeout, |s| s.try_answer(None, generation, id, &mut pick))
            .await?
    }

    pub async fn list(&mut self) -> Result<Vec<DeviceEntry>, Error> {
        self.settle().await?;
        let (id, pick) = self.session.op_list()?;
        self.wait(id, pick).await
    }

    /// What became of the service's driver packages: which were loaded, which refused and why.
    pub async fn packages(&mut self) -> Result<PackageList, Error> {
        self.settle().await?;
        let (id, pick) = self.session.op_packages()?;
        self.wait(id, pick).await
    }

    pub async fn open(&mut self, name: &str, tag: &str) -> Result<Handle, Error> {
        self.settle().await?;
        let (id, pick) = self.session.op_open(name, tag)?;
        self.wait(id, pick).await
    }

    pub async fn describe(&mut self, device: u32) -> Result<DeviceDescription, Error> {
        self.settle().await?;
        let (id, pick) = self.session.op_describe(device)?;
        self.wait(id, pick).await
    }

    /// A stale handle gets [`Error::Stale`]; open it again.
    pub async fn call(
        &mut self,
        handle: &Handle,
        method: u16,
        args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize, Error> {
        self.settle().await?;
        let (id, pick) = self.session.op_call(handle, method, args, reply)?;
        self.wait(id, pick).await
    }

    pub async fn subscribe(
        &mut self,
        handle: &Handle,
        method: u16,
        period_us: u64,
        only_changes: bool,
    ) -> Result<Subscription, Error> {
        self.settle().await?;
        let (id, local, pick) =
            self.session
                .op_subscribe(handle, method, period_us, only_changes)?;
        let result = self.wait(id, pick).await;
        if result.is_err() {
            self.session.forget(local);
        }
        result
    }

    /// See [`Session::open_stream`]. The frames then come off the ring as they do anywhere else.
    pub async fn open_stream(
        &mut self,
        device: u32,
        name: &str,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<FrameStream, Error> {
        self.open_stream_as(device, name, false, platform).await
    }

    /// See [`Session::open_stream_writable`].
    pub async fn open_stream_writable(
        &mut self,
        device: u32,
        name: &str,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<FrameStream, Error> {
        self.open_stream_as(device, name, true, platform).await
    }

    async fn open_stream_as(
        &mut self,
        device: u32,
        name: &str,
        writable: bool,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<FrameStream, Error> {
        self.settle().await?;
        let (id, pick) = self.session.op_open_stream(device, name, &platform)?;
        let ready = self.wait(id, pick).await;
        self.session
            .finish_open_stream(ready, device, name, writable, platform)
    }

    /// See [`Session::close_stream`].
    pub async fn close_stream(&mut self, stream: FrameStream) -> Result<(), Error> {
        self.settle().await?;
        let (id, pick) = self.session.op_close_stream(&stream)?;
        self.wait(id, pick).await
    }

    /// The next value from any subscription, or `None` after `timeout`.
    pub async fn next_value(
        &mut self,
        into: &mut [u8],
        timeout: Duration,
    ) -> Result<Option<ValueInfo>, Error> {
        match self.until(timeout, |s| s.try_value(None, into)).await {
            Ok(value) => value.map(Some),
            Err(Error::TimedOut) => Ok(None),
            Err(error) => Err(error),
        }
    }
}
