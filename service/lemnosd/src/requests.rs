//! Reads one client's requests and answers them.

use std::io::{ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use lemnos_core::{MethodId, Status};
use lemnos_host::DeviceInfo;
use lemnos_wire::{Frame, MAX_FRAME, VERSION, decode, encode_description};

use crate::Core;
use crate::conn::Conn;
use crate::events::wire_state;

const BUFFER_START: usize = 16 * 1024;
const DESCRIPTION_MAX: usize = 64 * 1024;
const REPLY_START: usize = 4 * 1024;
/// Header and fields a Reply adds around its bytes.
const REPLY_OVERHEAD: usize = 32;

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
        } = frame
        {
            return self.hello(version, events);
        }
        if !self.greeted {
            return false;
        }
        match frame {
            Frame::List { id } => self.list(id),
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
            Frame::StreamOpen { id, device, name } => self.stream_open(id, device, name),
            Frame::StreamClose { id, device, name } => self.stream_close(id, device, name),
            // Only the service sends the rest.
            _ => false,
        }
    }

    fn hello(&mut self, version: u16, events: bool) -> Keep {
        if version != VERSION {
            self.refuse(0, Status::NOT_SUPPORTED, "another protocol version");
            return false;
        }
        if !self.greeted {
            self.greeted = true;
            self.conn.events.store(events, Ordering::Relaxed);
        }
        self.conn.answer(&Frame::Welcome {
            version: VERSION,
            features: 0,
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
            });
        }
        self.conn.answer(&Frame::ListDone { id });
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
        let description = match self.core.host.describe(device) {
            Ok(description) => description,
            Err(status) => return self.refuse(id, status, "the device isn't running"),
        };
        let mut bytes = vec![0; DESCRIPTION_MAX];
        match encode_description(&manifest, &description, &mut bytes) {
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
            Err(status) => return self.refuse(id, status, "the call failed"),
        }
        true
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
        match self
            .core
            .readers
            .subscribe(&self.conn, device, target, period_us, only_changes, now)
        {
            Ok((subscription, period_us)) => {
                crate::lock(&self.conn.subscriptions).push(subscription);
                self.conn.answer(&Frame::Subscribed {
                    id,
                    subscription,
                    period_us,
                });
                true
            }
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

    fn stream_open(&self, id: u32, device: u32, name: &str) -> Keep {
        let lease = match self.core.host.stream_attach(device, name, self.conn.id) {
            Ok(lease) => lease,
            Err(status) => return self.refuse(id, status, "can't open the stream"),
        };
        let layout = lease.layout;
        let Ok(descriptors) = u8::try_from(lease.descriptors.len()) else {
            let _ = self.core.host.stream_detach(device, name, self.conn.id);
            return self.refuse(id, Status::IO, "too many descriptors");
        };
        // The descriptors close here if this connection has already gone. It stays attached
        // until `release` runs, which it will.
        self.conn.answer_with_descriptors(
            &Frame::StreamReady {
                id,
                buffers: layout.buffers,
                descriptors,
                buffer_bytes: layout.buffer_bytes,
                frame_slots: layout.frame_slots,
                job_slots: layout.job_slots,
                job_bytes: layout.job_bytes,
            },
            lease.descriptors,
        );
        true
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
