//! `embedded_hal_async::digital::Wait` for a GPIO line, driven by edge
//! events through Tokio's reactor.

use super::{EdgeEvent, EdgeKind, GpioLine, IoError};
use embedded_hal::digital::ErrorType;
use std::io;
use std::time::Duration;
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;

/// A GPIO line that awaits its edges: `embedded_hal_async::digital::Wait`.
///
/// Request the line as an input with edge detection on the edges you wait
/// for (`LineSettings::input().with_edge(LineEdge::Both)`); the line's
/// descriptor is registered with the current Tokio reactor, so create it
/// inside a runtime with IO enabled.
#[derive(Debug)]
pub struct AsyncGpioLine {
    fd: AsyncFd<GpioLine>,
}

impl AsyncGpioLine {
    pub fn new(line: GpioLine) -> io::Result<Self> {
        Ok(Self {
            fd: AsyncFd::with_interest(line, Interest::READABLE)?,
        })
    }

    pub fn line(&self) -> &GpioLine {
        self.fd.get_ref()
    }

    pub fn into_inner(self) -> GpioLine {
        self.fd.into_inner()
    }

    /// The logical value now.
    pub fn get(&self) -> io::Result<bool> {
        self.fd.get_ref().get()
    }

    /// The next edge event.
    pub async fn next_event(&mut self) -> io::Result<EdgeEvent> {
        loop {
            let mut guard = self.fd.readable_mut().await?;
            match guard.get_inner_mut().wait_event(Some(Duration::ZERO)) {
                Ok(Some(event)) => return Ok(event),
                Ok(None) => guard.clear_ready(),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => guard.clear_ready(),
                Err(e) => return Err(e),
            }
        }
    }

    async fn wait_level(&mut self, high: bool) -> Result<(), IoError> {
        loop {
            if self.get()? == high {
                return Ok(());
            }
            self.next_event().await?;
        }
    }

    async fn wait_edge(&mut self, kind: Option<EdgeKind>) -> Result<(), IoError> {
        loop {
            let event = self.next_event().await?;
            if kind.is_none_or(|k| k == event.kind) {
                return Ok(());
            }
        }
    }
}

impl ErrorType for AsyncGpioLine {
    type Error = IoError;
}

impl embedded_hal_async::digital::Wait for AsyncGpioLine {
    async fn wait_for_high(&mut self) -> Result<(), IoError> {
        self.wait_level(true).await
    }

    async fn wait_for_low(&mut self) -> Result<(), IoError> {
        self.wait_level(false).await
    }

    async fn wait_for_rising_edge(&mut self) -> Result<(), IoError> {
        self.wait_edge(Some(EdgeKind::Rising)).await
    }

    async fn wait_for_falling_edge(&mut self) -> Result<(), IoError> {
        self.wait_edge(Some(EdgeKind::Falling)).await
    }

    async fn wait_for_any_edge(&mut self) -> Result<(), IoError> {
        self.wait_edge(None).await
    }
}
