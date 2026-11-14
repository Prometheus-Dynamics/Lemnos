//! Reads one client's requests and answers them.

use std::io::{ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use lemnos_core::{MethodId, Status};
use lemnos_host::DeviceInfo;
use lemnos_host::stream::StreamMemory;
use lemnos_wire::ring::fds;
use lemnos_wire::{
    CORE_HASH, Frame, Identity, LEMNOS, LemnosName, MAX_FRAME, MAX_PLANES, PART_BYTES,
    PLANE_LENGTH_BYTES, REPLY_OVERHEAD, Tail, VERSION, WireError, check_parts, decode,
    encode_description, feature, handle_descriptors, package_mode,
};

use crate::Core;
use crate::conn::Conn;
use crate::events::wire_state;
use crate::packages::PackageMode;
use crate::readers::Point;

const BUFFER_START: usize = 16 * 1024;
const DESCRIPTION_MAX: usize = 64 * 1024;
const REPLY_START: usize = 4 * 1024;
/// Whether the connection should carry on.
type Keep = bool;

pub(crate) struct Client {
    core: Arc<Core>,
    conn: Arc<Conn>,
    greeted: bool,
    reply: Vec<u8>,
    buf: Vec<u8>,
    start: usize,
    end: usize,
}

impl Client {
    pub(crate) fn new(core: Arc<Core>, conn: Arc<Conn>) -> Self {
        Self {
            core,
            conn,
            greeted: false,
            reply: vec![0; REPLY_START],
            buf: vec![0; BUFFER_START],
            start: 0,
            end: 0,
        }
    }

    /// Takes what the client has sent. False when the connection should end.
    pub(crate) fn read_ready(&mut self, stream: &UnixStream) -> Keep {
        if self.start == self.end {
            self.start = 0;
            self.end = 0;
        } else if self.end == self.buf.len() {
            self.buf.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
            if self.end == self.buf.len() {
                if self.buf.len() >= MAX_FRAME {
                    return false;
                }
                self.buf.resize((self.buf.len() * 2).min(MAX_FRAME), 0);
            }
        }
        match (&*stream).read(&mut self.buf[self.end..]) {
            Ok(0) => return false,
            Ok(n) => self.end += n,
            Err(e) if matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) => {
                return true;
            }
            Err(_) => return false,
        }
        // `handle` borrows the client, and each frame borrows this.
        let buf = core::mem::take(&mut self.buf);
        let keep = self.handle_frames(&buf);
        self.buf = buf;
        keep
    }

    fn handle_frames(&mut self, buf: &[u8]) -> Keep {
        loop {
            match decode(&buf[self.start..self.end]) {
                Ok(None) => return true,
                Err(WireError::LayoutMismatch(_)) => {
                    // Built from another core: say which, then close. The peer checks our core
                    // before anything else in the Welcome it gets.
                    self.conn.answer(&Frame::Welcome {
                        core: CORE_HASH,
                        parts: Tail(&PART_BYTES[..]),
                        lemnos: LemnosName(LEMNOS),
                        version: VERSION,
                        features: 0,
                        host: "",
                    });
                    return false;
                }
                Err(_) => return false,
                Ok(Some((frame, used))) => {
                    self.start += used;
                    if !self.handle(frame) {
                        return false;
                    }
                }
            }
        }
    }

    fn refuse(&self, id: u32, status: Status, text: &str) -> Keep {
        self.conn.answer(&Frame::Refusal {
            id,
            status: status.0,
            text,
        });
        true
    }

    fn handle(&mut self, frame: Frame<'_>) -> Keep {
        if let Frame::Hello {
            version,
            name: _,
            events,
            parts,
            ..
        } = frame
        {
            return self.hello(version, events, parts.0);
        }
        if !self.greeted {
            return false;
        }
        match frame {
            Frame::List { id } => self.list(id),
            Frame::Packages { id } => self.packages(id),
            Frame::Open { id, device, tag } => self.open(id, device, tag),
            Frame::Describe { id, device } => self.describe(id, device),
            Frame::Call {
                id,
                device,
                contract,
                method,
                reply_capacity,
                args,
            } => self.call(id, device, contract, method, reply_capacity, args),
            Frame::Subscribe {
                id,
                device,
                contract,
                method,
                period_us,
                only_changes,
            } => self.subscribe(id, device, contract, method, period_us, only_changes),
            Frame::Unsubscribe { id, subscription } => self.unsubscribe(id, subscription),
            Frame::SetPower { id, device, on } => self.set_power(id, device, on),
            Frame::StreamOpen { id, device, name } => self.stream_open(id, device, name, false),
            Frame::StreamOpenHandles { id, device, name } => {
                self.stream_open(id, device, name, true)
            }
            Frame::StreamClose { id, device, name } => self.stream_close(id, device, name),
            Frame::CallJob {
                id,
                device,
                contract,
                method,
                args,
            } => self.call_job(id, device, contract, method, args),
            Frame::JobCancel { id, job } => self.cancel_job(id, job),
            Frame::EndpointsOpen { id, device } => self.endpoints_open(id, device),
            Frame::EndpointsOpenEnd { id, device } => self.endpoints_open_end(id, device),
            Frame::EndpointsClose { id, device } => self.endpoints_close(id, device),
            // Only the service sends the rest.
            _ => false,
        }
    }

    fn hello(&mut self, version: u16, events: bool, parts: &[u8]) -> Keep {
        if version != VERSION {
            self.refuse(0, Status::NOT_SUPPORTED, "another protocol version");
            return false;
        }
        let Ok(theirs) = check_parts(parts) else {
            self.refuse(0, Status::INVALID_ARGUMENT, "malformed wire parts");
            return false;
        };
        if !self.greeted {
            self.greeted = true;
            self.conn.events.store(events, Ordering::Relaxed);
        }
        // A part the client and this build disagree on is refused by name in the client; here its
        // bit is cleared, so the client does not send its frames. The rest of the connection works.
        self.conn.answer(&Frame::Welcome {
            core: CORE_HASH,
            parts: Tail(&PART_BYTES[..]),
            lemnos: LemnosName(LEMNOS),
            version: VERSION,
            // END belongs to the endpoints part: a client that refuses that part gets neither bit.
            features: (feature::EVERY | lemnos_wire::ENDPOINTS_END)
                & !theirs.refused
                & if theirs.refused & feature::ENDPOINTS != 0 {
                    !lemnos_wire::ENDPOINTS_END
                } else {
                    !0
                },
            host: &self.core.name,
        });
        true
    }

    fn list(&self, id: u32) -> Keep {
        for info in self.core.host.devices() {
            let (state, health, reason) = wire_state(&info);
            let tags = info
                .tags
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(",");
            self.conn.answer(&Frame::Device {
                id,
                device: info.index,
                name: &info.id,
                driver: &info.driver,
                tags: &tags,
                state,
                health,
                reason: &reason,
                link: Tail(info.link.as_deref().unwrap_or("")),
            });
        }
        self.conn.answer(&Frame::ListDone { id });
        true
    }

    /// What became of the driver packages: every file, then how packages are run.
    fn packages(&self, id: u32) -> Keep {
        let report = &self.core.packages;
        for file in &report.files {
            self.conn.answer(&Frame::Package {
                id,
                path: &file.path.to_string_lossy(),
                driver: file.driver.as_deref().unwrap_or(""),
                reason: &file.reason,
            });
        }
        self.conn.answer(&Frame::PackagesDone {
            id,
            mode: match report.mode {
                PackageMode::Off => package_mode::OFF,
                PackageMode::Signed => package_mode::SIGNED,
                PackageMode::Open => package_mode::OPEN,
            },
            note: &report.note,
        });
        true
    }

    fn resolve(&self, name: &str, tag: &str) -> Option<DeviceInfo> {
        if name.is_empty() {
            return self.core.host.find(tag).into_iter().next();
        }
        self.core
            .host
            .device(name)
            .filter(|info| info.tags.iter().any(|t| t.as_str() == tag))
    }

    fn open(&self, id: u32, name: &str, tag: &str) -> Keep {
        let Some(info) = self.resolve(name, tag) else {
            return self.refuse(id, Status::NOT_FOUND, "no such device");
        };
        let position = self
            .core
            .host
            .manifest(info.index)
            .and_then(|manifest| manifest.provides.iter().position(|c| c.tag.as_str() == tag));
        let Some(contract) = position else {
            return self.refuse(id, Status::NOT_FOUND, "no such contract");
        };
        self.conn.answer(&Frame::Opened {
            id,
            device: info.index,
            contract: contract as u8,
        });
        true
    }

    fn describe(&self, id: u32, device: u32) -> Keep {
        let Some(manifest) = self.core.host.manifest(device) else {
            return self.refuse(id, Status::NOT_FOUND, "no such device");
        };
        // A device whose driver is not running (off, retrying, failed, never started) has no
        // driver to describe it: it is described from its manifest alone, and says so (`not_live`).
        // Only a call in flight (BUSY) or a panic refuses.
        let (description, not_live) = match self.core.host.describe(device) {
            Ok(description) => (description, false),
            Err(status) if status == Status::NO_DEVICE => (lemnos_core::Description::new(), true),
            Err(status) => return self.refuse(id, status, "the device can't be described now"),
        };
        // The identity the board and the host give: its label, bus and state. Read after the
        // description, so a state that changed in between is the one reported.
        let Some(info) = self
            .core
            .host
            .devices()
            .into_iter()
            .find(|i| i.index == device)
        else {
            return self.refuse(id, Status::NOT_FOUND, "no such device");
        };
        let (state, _, reason) = wire_state(&info);
        let identity = Identity {
            label: &info.label,
            bus: info.bus.as_deref().unwrap_or(""),
            state,
            reason: &reason,
            not_live,
        };
        let mut bytes = vec![0; DESCRIPTION_MAX];
        match encode_description(&manifest, &description, &identity, &mut bytes) {
            Ok(n) => self.conn.answer(&Frame::Description {
                id,
                bytes: &bytes[..n],
            }),
            Err(status) => return self.refuse(id, status, "the description is too large"),
        }
        true
    }

    fn call(
        &mut self,
        id: u32,
        device: u32,
        contract: u8,
        method: u16,
        reply_capacity: u32,
        args: &[u8],
    ) -> Keep {
        let capacity = reply_capacity as usize;
        if capacity > MAX_FRAME - REPLY_OVERHEAD {
            return self.refuse(id, Status::INVALID_ARGUMENT, "reply capacity too large");
        }
        if self.reply.len() < capacity {
            self.reply.resize(capacity, 0);
        }
        let method = MethodId { contract, method };
        let reply = &mut self.reply[..capacity];
        match self
            .core
            .host
            .call_as(self.conn.id, device, method, args, reply)
        {
            Ok(n) => self.conn.answer(&Frame::Reply {
                id,
                bytes: &reply[..n],
            }),
            Err(status) => {
                let text = call_refusal(
                    self.core.host.manifest(device).as_ref(),
                    contract,
                    method.method,
                    status,
                );
                return self.refuse(id, status, &text);
            }
        }
        true
    }

    fn call_job(&self, id: u32, device: u32, contract: u8, method: u16, args: &[u8]) -> Keep {
        let method = MethodId { contract, method };
        match self.core.host.start_job(self.conn.id, device, method, args) {
            Ok(job) => self.conn.answer(&Frame::JobStarted { id, job }),
            Err(status) => return self.refuse(id, status, "the job could not be started"),
        }
        // What the job has already done goes out after this answer.
        self.core.wake();
        true
    }

    fn cancel_job(&self, id: u32, job: u64) -> Keep {
        match self.core.host.cancel_job(self.conn.id, job) {
            Ok(()) => {
                self.conn.answer(&Frame::Reply { id, bytes: &[] });
                self.core.wake();
                true
            }
            Err(status) => self.refuse(id, status, "can't cancel the job"),
        }
    }

    fn subscribe(
        &self,
        id: u32,
        device: u32,
        contract: u8,
        method: u16,
        period_us: u64,
        only_changes: bool,
    ) -> Keep {
        let readable = self
            .core
            .host
            .manifest(device)
            .and_then(|m| m.provides.get(contract as usize).copied())
            .and_then(|c| c.methods.iter().find(|m| m.id == method))
            .is_some_and(|m| m.params.is_empty() && !m.has_out_buffer);
        if !readable {
            return self.refuse(id, Status::INVALID_ARGUMENT, "not a readable method");
        }
        let target = MethodId { contract, method };
        let now = self.core.clock.now_us();
        let subscribed = self.core.readers.subscribe(
            &self.conn,
            Point {
                device,
                method: target,
            },
            period_us,
            only_changes,
            now,
            |subscription, period_us| {
                crate::lock(&self.conn.subscriptions).push(subscription);
                self.conn.answer(&Frame::Subscribed {
                    id,
                    subscription,
                    period_us,
                });
            },
        );
        match subscribed {
            Ok(()) => true,
            Err(status) => self.refuse(id, status, "can't subscribe"),
        }
    }

    fn unsubscribe(&self, id: u32, subscription: u64) -> Keep {
        let now = self.core.clock.now_us();
        if !self.core.readers.unsubscribe(&self.conn, subscription, now) {
            return self.refuse(id, Status::NOT_FOUND, "no such subscription");
        }
        crate::lock(&self.conn.subscriptions).retain(|s| *s != subscription);
        self.conn.answer(&Frame::Reply { id, bytes: &[] });
        true
    }

    fn set_power(&self, id: u32, device: u32, on: bool) -> Keep {
        let Some(info) = self
            .core
            .host
            .devices()
            .into_iter()
            .find(|d| d.index == device)
        else {
            return self.refuse(id, Status::NOT_FOUND, "no such device");
        };
        let done = if on {
            self.core.host.on(&info.id)
        } else {
            self.core.host.off(&info.id)
        };
        if done.is_err() {
            return self.refuse(id, Status::NOT_FOUND, "no such device");
        }
        self.conn.answer(&Frame::Reply { id, bytes: &[] });
        true
    }

    /// `takes_handles` is whether the client asked in the way that says it can take one
    /// descriptor per buffer for an answer.
    fn stream_open(&self, id: u32, device: u32, name: &str, takes_handles: bool) -> Keep {
        let Some(memory) = self.core.host.stream_memory(device, name) else {
            return self.refuse(id, Status::NOT_FOUND, "can't open the stream");
        };
        let dma_bufs = memory == StreamMemory::DmaBuf;
        // Before anything is taken or allocated: a client from before streams of dma-bufs gets
        // an answer it understands, and the stream stays free.
        if dma_bufs && !takes_handles {
            return self.refuse(
                id,
                Status::NOT_SUPPORTED,
                "the stream's buffers are dma-bufs, which this client can't take",
            );
        }
        let lease = match self.core.host.stream_attach(device, name, self.conn.id) {
            Ok(lease) => lease,
            Err(Status::BUSY) => return self.refuse(id, Status::BUSY, "can't open the stream"),
            Err(status) if dma_bufs => {
                return self.refuse(id, status, "can't get dma-bufs for the stream");
            }
            Err(status) => return self.refuse(id, status, "can't open the stream"),
        };
        let layout = lease.layout;
        if let Some(handles) = lease.handles {
            let planes = lease.planes.count();
            let descriptors = handle_descriptors(layout.buffers, planes).filter(|_| {
                takes_handles
                    && lease.descriptors.len() == fds::BEFORE + handles.len()
                    && handles.len() == layout.buffers as usize * planes as usize
            });
            let mut plane_bytes = [0; PLANE_LENGTH_BYTES * MAX_PLANES as usize];
            let plane_len = lease.planes.encode(&mut plane_bytes);
            let Some(descriptors) = descriptors else {
                let _ = self.core.host.stream_detach(device, name, self.conn.id);
                return self.refuse(id, Status::IO, "the stream's descriptors can't be sent");
            };
            // The descriptors close here if this connection has already gone, and the host's
            // hold on the buffers goes with them.
            self.conn.answer_with_descriptors(
                &Frame::StreamRingHandles {
                    id,
                    buffers: layout.buffers,
                    descriptors,
                    buffer_bytes: layout.buffer_bytes,
                    frame_slots: layout.frame_slots,
                    job_slots: layout.job_slots,
                    job_bytes: layout.job_bytes,
                    plane_bytes: plane_bytes.get(..plane_len).unwrap_or(&[]),
                    working: Tail(lease.working),
                    kind: Tail(u32::from(
                        lease.kind == lemnos_host::stream::StreamKind::JobDriven,
                    )),
                },
                lease.descriptors,
            );
            return true;
        }
        let Ok(descriptors) = u8::try_from(lease.descriptors.len()) else {
            let _ = self.core.host.stream_detach(device, name, self.conn.id);
            return self.refuse(id, Status::IO, "too many descriptors");
        };
        // The descriptors close here if this connection has already gone. It stays attached
        // until `release` runs, which it will.
        self.conn.answer_with_descriptors(
            &Frame::StreamRings {
                id,
                buffers: layout.buffers,
                descriptors,
                buffer_bytes: layout.buffer_bytes,
                frame_slots: layout.frame_slots,
                job_slots: layout.job_slots,
                job_bytes: layout.job_bytes,
                working: Tail(lease.working),
                kind: Tail(u32::from(
                    lease.kind == lemnos_host::stream::StreamKind::JobDriven,
                )),
            },
            lease.descriptors,
        );
        true
    }

    /// Lends this connection the bulk pair of a device's function. The descriptors close here if
    /// the connection has already gone, and the host's hold on them goes with it.
    fn endpoints_open(&self, id: u32, device: u32) -> Keep {
        match self.core.host.usb_endpoints(self.conn.id, device) {
            Ok([out, input]) => {
                self.conn
                    .answer_with_descriptors(&Frame::Endpoints { id }, vec![out, input]);
                true
            }
            Err(status) => self.refuse(id, status, "can't open the endpoints"),
        }
    }

    /// Lends this connection the bulk pair and the END descriptor of a device's function. As
    /// [`Self::endpoints_open`], and the hold's END goes when the hold is over for good.
    fn endpoints_open_end(&self, id: u32, device: u32) -> Keep {
        match self.core.host.usb_endpoints_end(self.conn.id, device) {
            Ok([out, input, end]) => {
                self.conn
                    .answer_with_descriptors(&Frame::EndpointsEnd { id }, vec![out, input, end]);
                true
            }
            Err(status) => self.refuse(id, status, "can't open the endpoints"),
        }
    }

    fn endpoints_close(&self, id: u32, device: u32) -> Keep {
        match self.core.host.usb_endpoints_release(self.conn.id, device) {
            Ok(()) => {
                self.conn.answer(&Frame::Reply { id, bytes: &[] });
                true
            }
            Err(status) => self.refuse(id, status, "can't close the endpoints"),
        }
    }

    fn stream_close(&self, id: u32, device: u32, name: &str) -> Keep {
        match self.core.host.stream_detach(device, name, self.conn.id) {
            Ok(()) => {
                self.conn.answer(&Frame::Reply { id, bytes: &[] });
                true
            }
            Err(status) => self.refuse(id, status, "can't close the stream"),
        }
    }

    /// Ends every subscription this connection held.
    pub(crate) fn release(&self) {
        let held = core::mem::take(&mut *crate::lock(&self.conn.subscriptions));
        let now = self.core.clock.now_us();
        for subscription in held {
            self.core.readers.unsubscribe(&self.conn, subscription, now);
        }
        // A client that is finishing comes through here twice.
        if !self.conn.gone.swap(true, Ordering::AcqRel) {
            self.core.host.caller_gone(self.conn.id);
        }
    }
}

/// The text a refused call carries. A method the device's contract does not have, or one its
/// driver does not support, is named by number and, where the contract names it, by name.
fn call_refusal(
    manifest: Option<&lemnos_core::Manifest>,
    contract: u8,
    method: u16,
    status: Status,
) -> String {
    let named = manifest.and_then(|manifest| {
        let info = manifest.provides.get(usize::from(contract))?;
        Some(match info.methods.iter().find(|m| m.id == method) {
            Some(found) => format!(
                "the device's driver does not support method {} `{}` of {}",
                found.id,
                found.name,
                info.tag.as_str()
            ),
            None => format!("contract {} has no method {method}", info.tag.as_str()),
        })
    });
    match (status, named) {
        (Status::NOT_SUPPORTED, Some(text)) => text,
        _ => "the call failed".to_string(),
    }
}
