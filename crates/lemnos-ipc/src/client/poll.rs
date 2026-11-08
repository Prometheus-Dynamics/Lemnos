//! Polled use of a [`DeviceClient`] from an event loop (`epoll`, `poll`, tokio's `AsyncFd`),
//! with no thread per client.
//!
//! [`DeviceClient::poll`] makes the socket non-blocking and returns what has arrived, or
//! `None` when nothing has: wait for [`DeviceClient::fd`] to be readable (and writable while
//! [`DeviceClient::wants_write`]) and call it again. Requests go out without waiting for
//! their answer ([`DeviceClient::send_read`], [`send_set`](DeviceClient::send_set),
//! [`send_get`](DeviceClient::send_get)), which comes back from `poll` with the request's
//! id. In the steady state (readings in, those requests out) nothing allocates: readings
//! are decoded into the caller's [`RawReading`], and requests are encoded from borrowed
//! names into a kept buffer. Events and the rarer messages are decoded as usual.
//!
//! The blocking calls still work in between (a subscription, say): they block for their
//! answer and hand the socket back non-blocking. Reconnection (a [`reconnecting`] client)
//! happens inside `poll` once [`DeviceClient::retry_at`] has passed; the connection's
//! handshake blocks briefly (up to the client's timeout). A reconnection opens a new socket,
//! so [`Polled::Connected`] means: register [`DeviceClient::fd`] again.
//!
//! LED and light requests go out the same way ([`DeviceClient::send_show`],
//! [`send_update_inputs`](DeviceClient::send_update_inputs),
//! [`send_light_brightness`](DeviceClient::send_light_brightness),
//! [`send_clear`](DeviceClient::send_clear)), on the device connection: no second connection
//! for a light. Their answers come back from `poll` as [`Polled::Answer`] (the brightness as a
//! [`Polled::Reply`]), and what the service took is remembered and sent again after a
//! reconnection, as [`LedClient`](crate::LedClient) does. These encode and keep the request,
//! so they allocate; the readings path stays allocation-free.
//!
//! [`reconnecting`]: crate::ClientOptions::reconnecting

use super::{ClientError, ClientEvent, DeviceClient, RECONNECT_MAX};
use crate::wire::{
    self, Event, LedRequest, LedShow, Message, NextFrame, RawReading, Refusal, Request,
};
use lemnos_light::{Spot, SpotList};
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::sync::Arc;
use std::time::Instant;

/// What [`DeviceClient::poll`] found.
#[derive(Debug)]
pub enum Polled {
    /// A reading, decoded into the [`RawReading`] given to `poll`: a subscription's (its
    /// `id` 0), or the answer to a [`DeviceClient::send_read`] (its id, from a service with
    /// [`FEATURE_READ_ID`](crate::FEATURE_READ_ID)).
    Reading,
    /// The answer to a [`DeviceClient::send_set`] or [`DeviceClient::send_get`] with this
    /// id: the value, or why it was refused. A refused [`DeviceClient::send_read`] answers
    /// here too, with no reading.
    Reply {
        id: u32,
        result: Result<f64, Refusal>,
    },
    /// The answer to a [`DeviceClient::send_show`], [`send_clear`](DeviceClient::send_clear)
    /// or [`send_update_inputs`](DeviceClient::send_update_inputs) with this id: taken, or
    /// refused with the service's reason (an unknown look, a look that takes no spots, a light
    /// this client may not write), the light then keeping what it showed.
    Answer { id: u32, result: Result<(), String> },
    /// A status, control or light event.
    Event(Event),
    /// Connected, with a new socket: register [`DeviceClient::fd`] again. Subscriptions and
    /// held intents are restored.
    Connected { reconnects: u64 },
    /// The connection dropped. Requests not answered yet will not be. A reconnecting
    /// client tries again from `poll` after [`DeviceClient::retry_at`].
    Disconnected { error: ClientError },
}

/// A polled LED or light request waiting for its answer.
pub(super) enum LedPending {
    /// A show (or a clear): remembered once taken.
    Show(LedRequest),
    /// New look inputs: remembered once taken.
    Inputs {
        device: String,
        progress: Option<u16>,
        spots: Option<SpotList>,
    },
    /// A light's brightness: the answer comes in thousandths.
    Brightness,
}

impl DeviceClient {
    /// Sends an LED request (a look, a look with spots, a frame, a clear, with its fade and
    /// the rest as built) without waiting; returns its id. The answer comes from
    /// [`poll`](Self::poll) as a [`Polled::Answer`] with this id; once taken, the request is
    /// what a reconnection shows again.
    pub fn send_show(&mut self, request: LedRequest) -> Result<u32, ClientError> {
        let id = self.conn.next_id();
        let mut request = request;
        request.id = id;
        let request = Request::Led(request);
        self.send_request(&request)?;
        let Request::Led(request) = request else {
            unreachable!("built as an LED request")
        };
        self.led_pending.push((id, LedPending::Show(request)));
        Ok(id)
    }

    /// Drops this client's intents ([`LedClient::clear`](crate::LedClient::clear)) without
    /// waiting; returns the request's id (see [`send_show`](Self::send_show)).
    pub fn send_clear(&mut self) -> Result<u32, ClientError> {
        self.send_show(LedRequest::new(LedShow::Clear))
    }

    /// Changes the inputs of the look this client shows on `device`
    /// ([`LedClient::update_inputs`](crate::LedClient::update_inputs)) without waiting;
    /// returns the request's id. Refused, with the reason, in a [`Polled::Answer`] when this
    /// client shows no look or the look takes no such input.
    pub fn send_update_inputs(
        &mut self,
        device: &str,
        progress: Option<f32>,
        spots: Option<&[Spot]>,
    ) -> Result<u32, ClientError> {
        let id = self.conn.next_id();
        let progress = progress.map(super::led::permille);
        let spots = spots.map(SpotList::new);
        self.send_request(&Request::LedInputs {
            id,
            device: device.to_string(),
            progress,
            spots,
        })?;
        self.led_pending.push((
            id,
            LedPending::Inputs {
                device: device.to_string(),
                progress,
                spots,
            },
        ));
        Ok(id)
    }

    /// Sets a light's ring-wide look brightness (0 to 1; `persist` saves it) without waiting
    /// ([`light_brightness`](Self::light_brightness)); returns the request's id. The value
    /// applied (0 to 1) comes from [`poll`](Self::poll) as a [`Polled::Reply`] with this id.
    pub fn send_light_brightness(
        &mut self,
        device: &str,
        value: f64,
        persist: bool,
    ) -> Result<u32, ClientError> {
        let id = self.conn.next_id();
        self.send_request(&Request::LightSetting {
            id,
            device: device.to_string(),
            look_brightness: super::permille(value),
            persist,
        })?;
        self.led_pending.push((id, LedPending::Brightness));
        Ok(id)
    }

    fn send_request(&mut self, request: &Request) -> Result<(), ClientError> {
        self.conn.scratch.clear();
        self.conn.scratch.extend_from_slice(&request.encode());
        self.conn.send_scratch()
    }

    fn take_pending(&mut self, id: u32) -> Option<LedPending> {
        let at = self.led_pending.iter().position(|(i, _)| *i == id)?;
        Some(self.led_pending.swap_remove(at).1)
    }

    /// Polled use on or off (`poll` turns it on). On, the socket is non-blocking
    /// between the blocking calls.
    pub fn set_nonblocking(&mut self, on: bool) -> Result<(), ClientError> {
        self.conn.nonblocking = on;
        self.conn.set_stream_nonblocking(on)
    }

    /// The socket, while connected: readable when [`poll`](Self::poll) has something,
    /// writable when queued requests can go ([`wants_write`](Self::wants_write)).
    pub fn fd(&self) -> Option<BorrowedFd<'_>> {
        self.conn.stream.as_ref().map(AsFd::as_fd)
    }

    /// Whether requests are queued for a full socket: wait for it to be writable too,
    /// then [`flush`](Self::flush) (or `poll`, which flushes).
    pub fn wants_write(&self) -> bool {
        !self.conn.out.is_empty()
    }

    /// Writes what the socket takes of the queued requests, without waiting.
    pub fn flush(&mut self) -> Result<(), ClientError> {
        self.conn.flush_out()
    }

    /// While a reconnecting client is disconnected, when `poll` tries again.
    pub fn retry_at(&self) -> Option<Instant> {
        (self.conn.stream.is_none() && self.conn.options.reconnect)
            .then_some(self.conn.next_attempt)
    }

    /// Asks for `device`'s latest reading without waiting; returns the request's id. The
    /// reading comes from [`poll`](Self::poll) with this id (a refusal as a
    /// [`Polled::Reply`]).
    pub fn send_read(&mut self, device: &str) -> Result<u32, ClientError> {
        let id = self.conn.next_id();
        wire::encode_read_into(&mut self.conn.scratch, id, device);
        self.conn.send_scratch()?;
        Ok(id)
    }

    /// Sets a control without waiting; returns the request's id. The value applied comes
    /// from [`poll`](Self::poll) as a [`Polled::Reply`] with this id.
    pub fn send_set(
        &mut self,
        device: &str,
        control: &str,
        value: f64,
    ) -> Result<u32, ClientError> {
        let id = self.conn.next_id();
        wire::encode_set_into(&mut self.conn.scratch, id, device, control, value);
        self.conn.send_scratch()?;
        Ok(id)
    }

    /// Asks for a control's value without waiting; returns the request's id (see
    /// [`send_set`](Self::send_set)).
    pub fn send_get(&mut self, device: &str, control: &str) -> Result<u32, ClientError> {
        let id = self.conn.next_id();
        wire::encode_get_into(&mut self.conn.scratch, id, device, control);
        self.conn.send_scratch()?;
        Ok(id)
    }

    /// What has arrived, without waiting: `Ok(None)` when nothing has (wait for
    /// [`fd`](Self::fd), or until [`retry_at`](Self::retry_at) while disconnected).
    /// Readings are decoded into `reading`. Also writes queued requests. A client that does
    /// not reconnect returns [`ClientError::Closed`] once its connection is gone.
    pub fn poll(&mut self, reading: &mut RawReading) -> Result<Option<Polled>, ClientError> {
        if !self.conn.nonblocking {
            self.set_nonblocking(true)?;
        }
        loop {
            if let Some(event) = self.conn.queue.pop_front() {
                match event {
                    ClientEvent::Connected { reconnects } => {
                        return Ok(Some(Polled::Connected { reconnects }));
                    }
                    ClientEvent::Disconnected { error } => {
                        // Nothing sent before will be answered now.
                        self.led_pending.clear();
                        return Ok(Some(Polled::Disconnected { error }));
                    }
                    ClientEvent::Data(message) => {
                        if let Some(polled) = self.polled(message, reading) {
                            return Ok(Some(polled));
                        }
                        continue;
                    }
                }
            }
            if self.conn.stream.is_none() {
                if !self.conn.options.reconnect {
                    return Err(ClientError::Closed);
                }
                if Instant::now() < self.conn.next_attempt {
                    return Ok(None);
                }
                if self.conn.connect().is_ok() {
                    self.conn.set_stream_nonblocking(true)?;
                } else {
                    self.conn.stream = None;
                    self.conn.next_attempt = Instant::now() + self.conn.backoff;
                    self.conn.backoff = (self.conn.backoff * 2).min(RECONNECT_MAX);
                    if self.conn.queue.is_empty() {
                        return Ok(None);
                    }
                }
                continue;
            }
            if !self.conn.out.is_empty() && self.conn.flush_out().is_err() {
                continue;
            }
            match wire::decode_reading_into(&self.conn.buf, reading) {
                Ok(NextFrame::Reading(used)) => {
                    self.conn.buf.drain(..used);
                    return Ok(Some(Polled::Reading));
                }
                Ok(NextFrame::Other) => match wire::decode_message(&self.conn.buf) {
                    Ok(Some((message, used))) => {
                        self.conn.buf.drain(..used);
                        if let Some(polled) = self.polled(message, reading) {
                            return Ok(Some(polled));
                        }
                        continue;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        self.conn.buf.clear();
                        self.conn.lost(ClientError::Protocol(e));
                        continue;
                    }
                },
                Ok(NextFrame::Incomplete) => {}
                // The stream cannot be resynchronised past a bad frame.
                Err(e) => {
                    self.conn.buf.clear();
                    self.conn.lost(ClientError::Protocol(e));
                    continue;
                }
            }
            let Some(stream) = self.conn.stream.as_mut() else {
                continue;
            };
            let mut chunk = [0u8; 4096];
            match stream.read(&mut chunk) {
                Ok(0) => self
                    .conn
                    .lost(ClientError::Io(io::ErrorKind::ConnectionReset.into())),
                Ok(n) => self.conn.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => self.conn.lost(ClientError::Io(e)),
            }
        }
    }

    /// A decoded message as `poll` returns it; `None` for one it keeps to itself.
    fn polled(&mut self, message: Message, reading: &mut RawReading) -> Option<Polled> {
        match message {
            Message::Reading(r) => {
                *reading = r;
                Some(Polled::Reading)
            }
            Message::Reply { id, result } => match self.take_pending(id) {
                // The service answers a brightness in thousandths, as it was asked.
                Some(LedPending::Brightness) => Some(Polled::Reply {
                    id,
                    result: result.map(|permille| permille / 1000.0),
                }),
                _ => Some(Polled::Reply { id, result }),
            },
            Message::Text { id, result } => {
                let pending = self.take_pending(id)?;
                if result.is_ok() {
                    match pending {
                        LedPending::Show(request) => self.conn.remember_led(&request),
                        LedPending::Inputs {
                            device,
                            progress,
                            spots,
                        } => self.conn.held_inputs(&device, progress, spots),
                        LedPending::Brightness => {}
                    }
                }
                Some(Polled::Answer {
                    id,
                    result: result.map(|_| ()),
                })
            }
            Message::Event(e) => Some(Polled::Event(e)),
            Message::Devices(devices) => {
                self.conn.devices = devices
                    .into_iter()
                    .map(|d| {
                        let channels: Arc<[_]> = d.channels.clone().into();
                        (d, channels)
                    })
                    .collect();
                None
            }
            _ => None,
        }
    }
}

impl AsRawFd for DeviceClient {
    /// The socket's descriptor. Panics while disconnected, like [`AsFd`]; use
    /// [`fd`](DeviceClient::fd) where the client may be.
    fn as_raw_fd(&self) -> RawFd {
        self.as_fd().as_raw_fd()
    }
}
