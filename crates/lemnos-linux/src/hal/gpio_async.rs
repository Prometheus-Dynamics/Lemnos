//! `embedded_hal_async::digital::Wait` for a GPIO line, driven by edge
//! events through Tokio's reactor.

use super::{EdgeEvent, EdgeKind, GpioLine, IoError};
use embedded_hal::digital::ErrorType;
use std::io;
use std::os::fd::AsRawFd;
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
    inner: Waiter<GpioLine>,
}

impl AsyncGpioLine {
    pub fn new(line: GpioLine) -> io::Result<Self> {
        Ok(Self {
            inner: Waiter::new(line)?,
        })
    }

    pub fn line(&self) -> &GpioLine {
        self.inner.fd.get_ref()
    }

    pub fn into_inner(self) -> GpioLine {
        self.inner.fd.into_inner()
    }

    /// The logical value now.
    pub fn get(&self) -> io::Result<bool> {
        self.inner.fd.get_ref().get()
    }

    /// The next edge event.
    pub async fn next_event(&mut self) -> io::Result<EdgeEvent> {
        self.inner.next_event().await
    }
}

/// What the waiter needs from a line: its level and a non-blocking event
/// read. A fake implements it in the tests.
trait EdgeLine: AsRawFd {
    fn level(&self) -> io::Result<bool>;
    fn try_event(&mut self) -> io::Result<Option<EdgeEvent>>;
}

impl EdgeLine for GpioLine {
    fn level(&self) -> io::Result<bool> {
        self.get()
    }

    fn try_event(&mut self) -> io::Result<Option<EdgeEvent>> {
        self.wait_event(Some(Duration::ZERO))
    }
}

#[derive(Debug)]
struct Waiter<L: AsRawFd> {
    fd: AsyncFd<L>,
}

impl<L: EdgeLine> Waiter<L> {
    fn new(line: L) -> io::Result<Self> {
        Ok(Self {
            fd: AsyncFd::with_interest(line, Interest::READABLE)?,
        })
    }

    async fn next_event(&mut self) -> io::Result<EdgeEvent> {
        loop {
            let mut guard = self.fd.readable_mut().await?;
            match guard.get_inner_mut().try_event() {
                Ok(Some(event)) => return Ok(event),
                Ok(None) => guard.clear_ready(),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => guard.clear_ready(),
                Err(e) => return Err(e),
            }
        }
    }

    async fn wait_level(&mut self, high: bool) -> Result<(), IoError> {
        loop {
            if self.fd.get_ref().level()? == high {
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
        self.inner.wait_level(true).await
    }

    async fn wait_for_low(&mut self) -> Result<(), IoError> {
        self.inner.wait_level(false).await
    }

    async fn wait_for_rising_edge(&mut self) -> Result<(), IoError> {
        self.inner.wait_edge(Some(EdgeKind::Rising)).await
    }

    async fn wait_for_falling_edge(&mut self) -> Result<(), IoError> {
        self.inner.wait_edge(Some(EdgeKind::Falling)).await
    }

    async fn wait_for_any_edge(&mut self) -> Result<(), IoError> {
        self.inner.wait_edge(None).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::os::fd::RawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct State {
        level: bool,
        pending: VecDeque<EdgeKind>,
        fail: bool,
        seqno: u32,
    }

    /// A line whose descriptor is one end of a socket pair: each queued
    /// edge writes one byte, so the reactor sees it as readable exactly
    /// like a line request with an event pending. Reading an edge applies
    /// it to the level, as the kernel's value read would show.
    struct FakeLine {
        rx: UnixStream,
        state: Arc<Mutex<State>>,
    }

    struct Driver {
        tx: UnixStream,
        state: Arc<Mutex<State>>,
    }

    impl Driver {
        fn edge(&mut self, kind: EdgeKind) {
            self.state.lock().unwrap().pending.push_back(kind);
            self.tx.write_all(&[1]).unwrap();
        }

        fn fail(&mut self) {
            self.state.lock().unwrap().fail = true;
            self.tx.write_all(&[1]).unwrap();
        }
    }

    fn fake(level: bool) -> (FakeLine, Driver) {
        let (rx, tx) = UnixStream::pair().unwrap();
        rx.set_nonblocking(true).unwrap();
        let state = Arc::new(Mutex::new(State {
            level,
            ..State::default()
        }));
        (
            FakeLine {
                rx,
                state: state.clone(),
            },
            Driver { tx, state },
        )
    }

    impl AsRawFd for FakeLine {
        fn as_raw_fd(&self) -> RawFd {
            self.rx.as_raw_fd()
        }
    }

    impl EdgeLine for FakeLine {
        fn level(&self) -> io::Result<bool> {
            Ok(self.state.lock().unwrap().level)
        }

        fn try_event(&mut self) -> io::Result<Option<EdgeEvent>> {
            let mut byte = [0u8];
            self.rx.read_exact(&mut byte)?;
            let mut state = self.state.lock().unwrap();
            if state.fail {
                return Err(io::Error::from_raw_os_error(ENODEV));
            }
            let kind = state.pending.pop_front().expect("one edge per byte");
            state.level = kind == EdgeKind::Rising;
            state.seqno += 1;
            Ok(Some(EdgeEvent {
                offset: 0,
                kind,
                timestamp_ns: 0,
                seqno: state.seqno,
                line_seqno: state.seqno,
            }))
        }
    }

    const ENODEV: i32 = 19;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap()
    }

    fn later(mut driver: Driver, edges: &'static [EdgeKind]) -> std::thread::JoinHandle<Driver> {
        std::thread::spawn(move || {
            for &kind in edges {
                std::thread::sleep(Duration::from_millis(20));
                driver.edge(kind);
            }
            driver
        })
    }

    #[test]
    fn edge_waits_skip_the_other_edge() {
        runtime().block_on(async {
            let (line, mut driver) = fake(true);
            let mut waiter = Waiter::new(line).unwrap();
            driver.edge(EdgeKind::Falling);
            driver.edge(EdgeKind::Rising);
            waiter.wait_edge(Some(EdgeKind::Rising)).await.unwrap();
            assert_eq!(waiter.fd.get_ref().state.lock().unwrap().seqno, 2);
            assert!(waiter.fd.get_ref().level().unwrap());

            driver.edge(EdgeKind::Rising);
            driver.edge(EdgeKind::Falling);
            waiter.wait_edge(Some(EdgeKind::Falling)).await.unwrap();
            assert_eq!(waiter.fd.get_ref().state.lock().unwrap().seqno, 4);
        });
    }

    #[test]
    fn any_edge_takes_the_first() {
        runtime().block_on(async {
            let (line, mut driver) = fake(false);
            let mut waiter = Waiter::new(line).unwrap();
            driver.edge(EdgeKind::Falling);
            driver.edge(EdgeKind::Rising);
            waiter.wait_edge(None).await.unwrap();
            assert_eq!(waiter.fd.get_ref().state.lock().unwrap().seqno, 1);
        });
    }

    #[test]
    fn level_waits_return_at_once_when_already_there() {
        runtime().block_on(async {
            let (line, _driver) = fake(true);
            let mut waiter = Waiter::new(line).unwrap();
            waiter.wait_level(true).await.unwrap();
            assert_eq!(waiter.fd.get_ref().state.lock().unwrap().seqno, 0);
        });
    }

    #[test]
    fn level_waits_await_edges_from_another_thread() {
        runtime().block_on(async {
            let (line, driver) = fake(false);
            let mut waiter = Waiter::new(line).unwrap();
            let feeder = later(
                driver,
                &[EdgeKind::Rising, EdgeKind::Falling, EdgeKind::Rising],
            );
            waiter.wait_level(true).await.unwrap();
            assert_eq!(waiter.fd.get_ref().state.lock().unwrap().seqno, 1);
            waiter.wait_level(false).await.unwrap();
            assert_eq!(waiter.fd.get_ref().state.lock().unwrap().seqno, 2);
            let event = waiter.next_event().await.unwrap();
            assert_eq!((event.kind, event.seqno), (EdgeKind::Rising, 3));
            feeder.join().unwrap();
        });
    }

    #[test]
    fn a_read_error_ends_the_wait() {
        runtime().block_on(async {
            let (line, mut driver) = fake(false);
            let mut waiter = Waiter::new(line).unwrap();
            driver.fail();
            let error = waiter.wait_level(true).await.unwrap_err();
            assert_eq!(io::Error::from(error).raw_os_error(), Some(ENODEV));
        });
    }
}
