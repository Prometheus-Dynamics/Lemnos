//! The Windows and macOS backend: nusb (WinUSB and IOKit).
//!
//! nusb has no isochronous transfers, so those are Linux-only. Synchronous
//! transfers block on nusb's transfer calls. Asynchronous ones run on a worker
//! thread per endpoint: the worker owns the endpoint, queues the transfers in
//! submission order and waits for their completions (nusb completes an
//! endpoint's transfers in that order).

use crate::config::ConfigurationInfo;
use crate::config::configuration_from_bytes;
use crate::error::{Error, ErrorKind, Result};
use crate::transfer::{Canceller, Shared, Transfer, TransferOutcome};
use crate::types::{ControlKind, ControlRecipient, ControlSetup, Direction, TransferType};
use crate::wait::ThreadWaker;
use nusb::transfer::{
    Buffer, Bulk, Completion, ControlIn, ControlOut, ControlType, In, Interrupt, Out, Recipient,
};
use nusb::{Endpoint, Interface, MaybeFuture};
use std::collections::{BTreeMap, VecDeque};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::thread::{self, Thread};
use std::time::Duration;

/// What a timeout of zero means here: nusb waits for a duration, not forever.
const NO_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// One endpoint, opened with its type and direction.
enum Ep {
    BulkIn(Endpoint<Bulk, In>),
    BulkOut(Endpoint<Bulk, Out>),
    InterruptIn(Endpoint<Interrupt, In>),
    InterruptOut(Endpoint<Interrupt, Out>),
}

impl Ep {
    fn max_packet_size(&self) -> usize {
        match self {
            Self::BulkIn(ep) => ep.max_packet_size(),
            Self::BulkOut(ep) => ep.max_packet_size(),
            Self::InterruptIn(ep) => ep.max_packet_size(),
            Self::InterruptOut(ep) => ep.max_packet_size(),
        }
    }

    fn transfer_in(&mut self, requested: usize, timeout: Duration) -> Completion {
        let buffer = Buffer::new(requested);
        match self {
            Self::BulkIn(ep) => ep.transfer_blocking(buffer, timeout),
            Self::InterruptIn(ep) => ep.transfer_blocking(buffer, timeout),
            Self::BulkOut(_) | Self::InterruptOut(_) => {
                unreachable!("direction is checked at open")
            }
        }
    }

    fn transfer_out(&mut self, data: &[u8], timeout: Duration) -> Completion {
        let buffer = Buffer::from(data);
        match self {
            Self::BulkOut(ep) => ep.transfer_blocking(buffer, timeout),
            Self::InterruptOut(ep) => ep.transfer_blocking(buffer, timeout),
            Self::BulkIn(_) | Self::InterruptIn(_) => unreachable!("direction is checked at open"),
        }
    }

    fn submit(&mut self, buffer: Buffer) {
        match self {
            Self::BulkIn(ep) => ep.submit(buffer),
            Self::BulkOut(ep) => ep.submit(buffer),
            Self::InterruptIn(ep) => ep.submit(buffer),
            Self::InterruptOut(ep) => ep.submit(buffer),
        }
    }

    fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Completion> {
        match self {
            Self::BulkIn(ep) => ep.poll_next_complete(cx),
            Self::BulkOut(ep) => ep.poll_next_complete(cx),
            Self::InterruptIn(ep) => ep.poll_next_complete(cx),
            Self::InterruptOut(ep) => ep.poll_next_complete(cx),
        }
    }

    fn cancel_all(&mut self) {
        match self {
            Self::BulkIn(ep) => ep.cancel_all(),
            Self::BulkOut(ep) => ep.cancel_all(),
            Self::InterruptIn(ep) => ep.cancel_all(),
            Self::InterruptOut(ep) => ep.cancel_all(),
        }
    }
}

/// A command to an endpoint worker.
enum Command {
    Submit {
        buffer: Buffer,
        shared: Arc<Shared>,
        /// For IN transfers, the bytes asked for (the buffer's length is the
        /// bus-sized request).
        wanted: Option<usize>,
    },
    Cancel,
}

/// The worker for one endpoint: its command channel and its thread, to wake.
struct Worker {
    interface: u8,
    commands: Sender<Command>,
    thread: Thread,
    max_packet_size: usize,
}

pub(crate) struct Native {
    device: nusb::Device,
    claimed: Mutex<BTreeMap<u8, Arc<Interface>>>,
    workers: Mutex<BTreeMap<u8, Worker>>,
}

impl Native {
    pub(crate) fn open(info: &nusb::DeviceInfo) -> Result<Self> {
        let device = info
            .open()
            .wait()
            .map_err(|error| Error::from_nusb("opening the USB device", &error))?;
        Ok(Self {
            device,
            claimed: Mutex::new(BTreeMap::new()),
            workers: Mutex::new(BTreeMap::new()),
        })
    }

    pub(crate) fn configurations(&self) -> Result<Vec<ConfigurationInfo>> {
        self.device
            .configurations()
            .map(|config| configuration_from_bytes(config.as_bytes()))
            .collect()
    }

    pub(crate) fn active_configuration(&self) -> Result<u8> {
        self.device
            .active_configuration()
            .map(|config| config.configuration_value())
            .map_err(|error| {
                Error::new(
                    ErrorKind::Other,
                    format!("reading the active configuration: {error}"),
                )
            })
    }

    pub(crate) fn set_configuration(&self, value: u8) -> Result<()> {
        self.device
            .set_configuration(value)
            .wait()
            .map_err(|error| Error::from_nusb("selecting the configuration", &error))
    }

    pub(crate) fn reset(&self) -> Result<()> {
        self.device
            .reset()
            .wait()
            .map_err(|error| Error::from_nusb("resetting the device", &error))
    }

    pub(crate) fn claim_interface(&self, number: u8) -> Result<()> {
        let mut claimed = self.lock_claimed();
        if claimed.contains_key(&number) {
            return Err(Error::new(
                ErrorKind::Busy,
                format!("interface {number} is already claimed"),
            ));
        }
        let interface = self
            .device
            .claim_interface(number)
            .wait()
            .map_err(|error| Error::from_nusb("claiming the interface", &error))?;
        claimed.insert(number, Arc::new(interface));
        Ok(())
    }

    pub(crate) fn release_interface(&self, number: u8) {
        // Endpoint workers hold their endpoints, and so the claim, until they
        // finish: stop the ones on this interface first.
        self.lock_workers()
            .retain(|_, worker| worker.interface != number);
        self.lock_claimed().remove(&number);
    }

    pub(crate) fn set_alternate(&self, number: u8, alternate: u8) -> Result<()> {
        let claimed = self.lock_claimed();
        let interface = claimed.get(&number).ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                format!("interface {number} is not claimed"),
            )
        })?;
        interface
            .set_alt_setting(alternate)
            .wait()
            .map_err(|error| Error::from_nusb("selecting the alternate setting", &error))
    }

    pub(crate) fn control_in(
        &self,
        setup: &ControlSetup,
        length: u16,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let request = ControlIn {
            control_type: control_type(setup.kind),
            recipient: recipient(setup.recipient),
            request: setup.request,
            value: setup.value,
            index: setup.index,
            length,
        };
        let result = self.control_in_native(request, timeout);
        result.map_err(|error| Error::from_transfer("control read", error))
    }

    pub(crate) fn control_out(
        &self,
        setup: &ControlSetup,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize> {
        let request = ControlOut {
            control_type: control_type(setup.kind),
            recipient: recipient(setup.recipient),
            request: setup.request,
            value: setup.value,
            index: setup.index,
            data,
        };
        self.control_out_native(request, timeout)
            .map(|()| data.len())
            .map_err(|error| Error::from_transfer("control write", error))
    }

    #[cfg(not(windows))]
    fn control_in_native(
        &self,
        request: ControlIn,
        timeout: Duration,
    ) -> std::result::Result<Vec<u8>, nusb::transfer::TransferError> {
        self.device
            .control_in(request, timeout_or_forever(timeout))
            .wait()
    }

    #[cfg(not(windows))]
    fn control_out_native(
        &self,
        request: ControlOut<'_>,
        timeout: Duration,
    ) -> std::result::Result<(), nusb::transfer::TransferError> {
        self.device
            .control_out(request, timeout_or_forever(timeout))
            .wait()
    }

    /// Windows routes control transfers through a claimed interface (WinUSB).
    #[cfg(windows)]
    fn control_in_native(
        &self,
        request: ControlIn,
        timeout: Duration,
    ) -> std::result::Result<Vec<u8>, nusb::transfer::TransferError> {
        let interface = self.first_claimed()?;
        interface
            .control_in(request, timeout_or_forever(timeout))
            .wait()
    }

    #[cfg(windows)]
    fn control_out_native(
        &self,
        request: ControlOut<'_>,
        timeout: Duration,
    ) -> std::result::Result<(), nusb::transfer::TransferError> {
        let interface = self.first_claimed()?;
        interface
            .control_out(request, timeout_or_forever(timeout))
            .wait()
    }

    #[cfg(windows)]
    fn first_claimed(&self) -> std::result::Result<Arc<Interface>, nusb::transfer::TransferError> {
        self.lock_claimed()
            .values()
            .next()
            .cloned()
            .ok_or(nusb::transfer::TransferError::InvalidArgument)
    }

    pub(crate) fn transfer_in(
        &self,
        address: u8,
        transfer: TransferType,
        len: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let (mut ep, _) = self.open_endpoint(address, transfer, Direction::In)?;
        let mps = ep.max_packet_size().max(1);
        let requested = len.div_ceil(mps).max(1) * mps;
        let completion = ep.transfer_in(requested, timeout_or_forever(timeout));
        let actual = completion.actual_len;
        let mut data = completion.buffer.into_vec();
        check_sync_status(completion.status, "USB read", timeout)?;
        data.truncate(actual.min(len));
        Ok(data)
    }

    pub(crate) fn transfer_out(
        &self,
        address: u8,
        transfer: TransferType,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize> {
        let (mut ep, _) = self.open_endpoint(address, transfer, Direction::Out)?;
        let completion = ep.transfer_out(data, timeout_or_forever(timeout));
        check_sync_status(completion.status, "USB write", timeout)?;
        Ok(completion.actual_len)
    }

    /// Submits an asynchronous transfer on an endpoint (bulk or interrupt).
    pub(crate) fn submit(
        &self,
        address: u8,
        transfer: TransferType,
        direction: Direction,
        buffer: Vec<u8>,
    ) -> Result<Transfer> {
        let (commands, thread, max_packet_size) = self.worker_for(address, transfer, direction)?;
        let shared = Shared::new();
        let command = match direction {
            Direction::Out => Command::Submit {
                buffer: Buffer::from(buffer),
                shared: Arc::clone(&shared),
                wanted: None,
            },
            Direction::In => {
                let mps = max_packet_size.max(1);
                let requested = buffer.len().div_ceil(mps).max(1) * mps;
                Command::Submit {
                    buffer: Buffer::new(requested),
                    shared: Arc::clone(&shared),
                    wanted: Some(buffer.len()),
                }
            }
        };
        commands.send(command).map_err(|_| {
            Error::new(ErrorKind::Disconnected, "the endpoint's worker has stopped")
        })?;
        thread.unpark();
        Ok(Transfer::new(
            shared,
            Some(Box::new(EndpointCancel { commands, thread })),
        ))
    }

    /// The endpoint's worker, started on first use.
    fn worker_for(
        &self,
        address: u8,
        transfer: TransferType,
        direction: Direction,
    ) -> Result<(Sender<Command>, Thread, usize)> {
        let mut workers = self.lock_workers();
        if let Some(worker) = workers.get(&address) {
            return Ok((
                worker.commands.clone(),
                worker.thread.clone(),
                worker.max_packet_size,
            ));
        }
        let (ep, interface) = self.open_endpoint(address, transfer, direction)?;
        let max_packet_size = ep.max_packet_size();
        let (sender, receiver) = mpsc::channel();
        let handle = thread::Builder::new()
            .name(format!("lemnos-usb-ep-{address:02x}"))
            .spawn(move || run_worker(ep, receiver))
            .map_err(|error| {
                Error::new(
                    ErrorKind::Other,
                    format!("starting an endpoint worker: {error}"),
                )
            })?;
        let thread = handle.thread().clone();
        workers.insert(
            address,
            Worker {
                interface,
                commands: sender.clone(),
                thread: thread.clone(),
                max_packet_size,
            },
        );
        Ok((sender, thread, max_packet_size))
    }

    /// Opens the endpoint on whichever claimed interface has it.
    fn open_endpoint(
        &self,
        address: u8,
        transfer: TransferType,
        direction: Direction,
    ) -> Result<(Ep, u8)> {
        let claimed = self.lock_claimed();
        for (&number, interface) in claimed.iter() {
            let opened = match (transfer, direction) {
                (TransferType::Bulk, Direction::In) => {
                    interface.endpoint::<Bulk, In>(address).map(Ep::BulkIn)
                }
                (TransferType::Bulk, Direction::Out) => {
                    interface.endpoint::<Bulk, Out>(address).map(Ep::BulkOut)
                }
                (TransferType::Interrupt, Direction::In) => interface
                    .endpoint::<Interrupt, In>(address)
                    .map(Ep::InterruptIn),
                (TransferType::Interrupt, Direction::Out) => interface
                    .endpoint::<Interrupt, Out>(address)
                    .map(Ep::InterruptOut),
                (TransferType::Control | TransferType::Isochronous, _) => {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        "this transfer type is not supported on this platform",
                    ));
                }
            };
            match opened {
                Ok(ep) => return Ok((ep, number)),
                Err(error) if error.kind() == nusb::ErrorKind::NotFound => continue,
                Err(error) => return Err(Error::from_nusb("opening the endpoint", &error)),
            }
        }
        Err(Error::new(
            ErrorKind::NotFound,
            format!("endpoint {address:#04x} is not in a claimed interface"),
        ))
    }

    fn lock_claimed(&self) -> MutexGuard<'_, BTreeMap<u8, Arc<Interface>>> {
        self.claimed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_workers(&self) -> MutexGuard<'_, BTreeMap<u8, Worker>> {
        self.workers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Cancels every transfer queued on the endpoint.
struct EndpointCancel {
    commands: Sender<Command>,
    thread: Thread,
}

impl Canceller for EndpointCancel {
    fn cancel(&self) {
        if self.commands.send(Command::Cancel).is_ok() {
            self.thread.unpark();
        }
    }
}

/// The endpoint worker: submits queued transfers, waits for completions in
/// order, and parks while a transfer is queued and none has completed.
fn run_worker(mut ep: Ep, commands: Receiver<Command>) {
    let waker = ThreadWaker::current();
    let mut context = Context::from_waker(&waker);
    let mut queue: VecDeque<(Arc<Shared>, Option<usize>)> = VecDeque::new();
    let mut open = true;
    loop {
        if queue.is_empty() {
            if !open {
                return;
            }
            // Idle: block for the next command.
            match commands.recv() {
                Ok(command) => apply(&mut ep, &mut queue, command),
                Err(_) => {
                    open = false;
                    continue;
                }
            }
        }
        // Take every command already waiting, so the endpoint queue fills.
        while open {
            match commands.try_recv() {
                Ok(command) => apply(&mut ep, &mut queue, command),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => open = false,
            }
        }
        if let Poll::Ready(completion) = ep.poll_next(&mut context)
            && let Some((shared, wanted)) = queue.pop_front()
        {
            shared.complete(finish(completion, wanted));
        } else if !queue.is_empty() {
            thread::park();
        }
    }
}

fn apply(ep: &mut Ep, queue: &mut VecDeque<(Arc<Shared>, Option<usize>)>, command: Command) {
    match command {
        Command::Submit {
            buffer,
            shared,
            wanted,
        } => {
            ep.submit(buffer);
            queue.push_back((shared, wanted));
        }
        Command::Cancel => ep.cancel_all(),
    }
}

fn finish(completion: Completion, wanted: Option<usize>) -> Result<TransferOutcome> {
    let actual = completion.actual_len;
    let mut buffer = completion.buffer.into_vec();
    check_status(completion.status, "USB transfer")?;
    if let Some(wanted) = wanted {
        buffer.truncate(actual.min(wanted));
    }
    Ok(TransferOutcome {
        buffer,
        actual_length: actual,
        packets: Vec::new(),
    })
}

/// A transfer's status as an error. nusb reports a timed-out transfer as
/// cancelled; the synchronous calls say so as a timeout.
fn check_sync_status(
    status: std::result::Result<(), nusb::transfer::TransferError>,
    context: &str,
    timeout: Duration,
) -> Result<()> {
    use nusb::transfer::TransferError;
    match status {
        Ok(()) => Ok(()),
        Err(TransferError::Cancelled) => Err(Error::new(
            ErrorKind::Timeout,
            format!("{context}: timed out after {timeout:?}"),
        )),
        Err(error) => Err(Error::from_transfer(context, error)),
    }
}

fn check_status(
    status: std::result::Result<(), nusb::transfer::TransferError>,
    context: &str,
) -> Result<()> {
    status.map_err(|error| Error::from_transfer(context, error))
}

fn timeout_or_forever(timeout: Duration) -> Duration {
    if timeout.is_zero() {
        NO_TIMEOUT
    } else {
        timeout
    }
}

fn control_type(kind: ControlKind) -> ControlType {
    match kind {
        ControlKind::Standard => ControlType::Standard,
        ControlKind::Class => ControlType::Class,
        ControlKind::Vendor => ControlType::Vendor,
    }
}

fn recipient(recipient: ControlRecipient) -> Recipient {
    match recipient {
        ControlRecipient::Device => Recipient::Device,
        ControlRecipient::Interface => Recipient::Interface,
        ControlRecipient::Endpoint => Recipient::Endpoint,
        ControlRecipient::Other => Recipient::Other,
    }
}
