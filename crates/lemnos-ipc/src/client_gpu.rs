//! [`GpuClient`]: the advisory GPU lease (see [`crate::wire::GpuAcquire`]).

use super::{ClientError, ClientEvent, ClientOptions, Connection};
use crate::wire::{
    FEATURE_GPU_LEASE, GpuAcquire, GpuInfo, GpuLeaseState, Message, Refusal, Request,
};
use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};

/// Asks `lemnosd` for GPU leases. Advisory: the lease grants nothing the
/// kernel enforces (the GPU's driver opens the render node itself); it only
/// orders the clients that ask. Leases belong to this connection and end with
/// it, as a lock on an open file does. A `GpuClient` never reconnects: a lost
/// connection has lost its leases.
pub struct GpuClient {
    pub(super) conn: Connection,
    notices: VecDeque<GpuNotice>,
}

/// A notice to a lease's holder: [`GpuLeaseState::YieldAsked`] or
/// [`GpuLeaseState::Overdue`] (or the grant of a lease whose wait ended).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuNotice {
    pub lease: u32,
    pub state: GpuLeaseState,
}

impl GpuClient {
    /// The service at `path`, as client `name`.
    pub fn connect(path: impl AsRef<Path>, name: impl Into<String>) -> Result<Self, ClientError> {
        ClientOptions::new(path, name).gpu()
    }

    pub(super) fn new(conn: Connection) -> Self {
        Self {
            conn,
            notices: VecDeque::new(),
        }
    }

    /// Whether the service has the lease requests ([`FEATURE_GPU_LEASE`]).
    /// Every other call fails with [`Refusal::Unsupported`] when it has not.
    pub fn supported(&self) -> bool {
        self.conn.features & FEATURE_GPU_LEASE != 0
    }

    fn check(&self) -> Result<(), ClientError> {
        if self.supported() {
            Ok(())
        } else {
            Err(ClientError::Refused(Refusal::Unsupported))
        }
    }

    /// Takes a lease and returns its number. `wait: None` takes it only if it
    /// is free now (else [`Refusal::Claimed`]); `Some(w)` queues behind the
    /// holders for up to `w`, then gives up the place ([`ClientError::Timeout`]).
    /// `request.wait` is set from `wait`. An unknown GPU is
    /// [`Refusal::UnknownDevice`].
    pub fn acquire(
        &mut self,
        mut request: GpuAcquire,
        wait: Option<Duration>,
    ) -> Result<u32, ClientError> {
        self.check()?;
        request.wait = wait.is_some();
        let id = self.conn.next_id();
        let start = Instant::now();
        self.conn.send(&Request::GpuAcquire {
            id,
            acquire: request,
        })?;
        let mut deadline = start + self.conn.options.timeout;
        let mut queued = None;
        loop {
            let message = match self.conn.receive(deadline) {
                Ok(m) => m,
                Err(ClientError::Timeout) => {
                    if let Some(lease) = queued {
                        // Give the place up; a grant racing this is released too.
                        let _ = self.release(lease);
                    }
                    return Err(ClientError::Timeout);
                }
                Err(e) => return Err(e),
            };
            match message {
                Message::GpuLease {
                    id: got,
                    lease,
                    state,
                } if got == id => match state {
                    GpuLeaseState::Granted => return Ok(lease),
                    GpuLeaseState::Queued { .. } => {
                        queued = Some(lease);
                        deadline = start + wait.unwrap_or_default();
                    }
                    GpuLeaseState::Refused(r) => return Err(ClientError::Refused(r)),
                    other => self.notices.push_back(GpuNotice {
                        lease,
                        state: other,
                    }),
                },
                Message::GpuLease { lease, state, .. } => {
                    self.notices.push_back(GpuNotice { lease, state });
                }
                other => self.conn.queue.push_back(ClientEvent::Data(other)),
            }
        }
    }

    /// Ends a lease, held or queued. Not this connection's, or already ended:
    /// [`Refusal::UnknownHandle`].
    pub fn release(&mut self, lease: u32) -> Result<(), ClientError> {
        self.check()?;
        let id = self.conn.next_id();
        self.conn
            .request(&Request::GpuRelease { id, lease }, super::reply(id))?
            .map(|_| ())
            .map_err(ClientError::Refused)
    }

    /// Reports that work under `lease` hung (`detail`: what was seen, such as
    /// a fence that never signalled). Shown in the GPU's state.
    pub fn report_hang(&mut self, lease: u32, detail: &str) -> Result<(), ClientError> {
        self.check()?;
        let id = self.conn.next_id();
        let request = Request::GpuHang {
            id,
            lease,
            detail: detail.to_string(),
        };
        self.conn
            .request(&request, super::reply(id))?
            .map(|_| ())
            .map_err(ClientError::Refused)
    }

    /// The GPUs the service found and their leases.
    pub fn status(&mut self) -> Result<Vec<GpuInfo>, ClientError> {
        self.check()?;
        let id = self.conn.next_id();
        self.conn.request(&Request::GpuStatus { id }, |m| match m {
            Message::GpuState { id: got, gpus } if *got == id => Some(gpus.clone()),
            _ => None,
        })
    }

    /// The next notice to one of this client's leases, waiting up to `wait`.
    pub fn next_notice(&mut self, wait: Duration) -> Result<Option<GpuNotice>, ClientError> {
        let deadline = Instant::now() + wait;
        loop {
            if let Some(n) = self.notices.pop_front() {
                return Ok(Some(n));
            }
            match self.conn.queue.pop_front() {
                Some(ClientEvent::Data(Message::GpuLease { lease, state, .. })) => {
                    return Ok(Some(GpuNotice { lease, state }));
                }
                Some(ClientEvent::Disconnected { error }) => return Err(error),
                Some(_) => continue,
                None => {}
            }
            match self.conn.receive(deadline) {
                Ok(Message::GpuLease { lease, state, .. }) => {
                    return Ok(Some(GpuNotice { lease, state }));
                }
                Ok(_) => {}
                Err(ClientError::Timeout) => return Ok(None),
                Err(e) => return Err(e),
            }
        }
    }

    pub fn is_connected(&self) -> bool {
        self.conn.stream.is_some()
    }
}
