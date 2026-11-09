//! Reads one client's requests and answers them.

use std::io::Read;
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
}

impl Client {
    pub(crate) fn new(core: Arc<Core>, conn: Arc<Conn>) -> Self {
        Self {
            core,
            conn,
            greeted: false,
            reply: vec![0; REPLY_START],
        }
    }

    pub(crate) fn read_loop(&mut self, mut stream: UnixStream) {
        let mut buf = vec![0u8; BUFFER_START];
        let mut start = 0;
        let mut end = 0;
        loop {
            if start == end {
                start = 0;
                end = 0;
            } else if end == buf.len() {
                buf.copy_within(start..end, 0);
                end -= start;
                start = 0;
                if end == buf.len() {
                    if buf.len() >= MAX_FRAME {
                        return;
                    }
                    buf.resize((buf.len() * 2).min(MAX_FRAME), 0);
                }
            }
            match stream.read(&mut buf[end..]) {
                Ok(0) => return,
                Ok(n) => end += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
            loop {
                match decode(&buf[start..end]) {
                    Ok(None) => break,
                    Err(_) => return,
                    Ok(Some((frame, used))) => {
                        start += used;
                        if !self.handle(frame) {
                            return;
                        }
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
            Frame::StreamOpen { id, .. } => {
                self.refuse(id, Status::NOT_SUPPORTED, "streams are not served yet")
            }
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
        match self.core.host.call(device, method, args, reply) {
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

    /// Ends everything this connection held.
    pub(crate) fn finish(&self) {
        let held = core::mem::take(&mut *crate::lock(&self.conn.subscriptions));
        let now = self.core.clock.now_us();
        for subscription in held {
            self.core.readers.unsubscribe(&self.conn, subscription, now);
        }
        self.conn.finish();
        crate::lock(&self.core.clients).retain(|c| !Arc::ptr_eq(c, &self.conn));
    }
}
