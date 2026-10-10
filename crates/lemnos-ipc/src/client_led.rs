//! [`LedClient`]: LED intents on `lemnosd`'s lights.

use super::{ClientError, ClientEvent, ClientOptions, Connection};
use crate::wire::{Event, LedRequest, LedShow, LooksOp, Message, Request};
use std::path::Path;
use std::time::{Duration, Instant};

/// Holds LED intents on `lemnosd`'s lights.
pub struct LedClient {
    pub(super) conn: Connection,
}

impl LedClient {
    /// The service at `path`, as client `name`.
    pub fn connect(path: impl AsRef<Path>, name: impl Into<String>) -> Result<Self, ClientError> {
        ClientOptions::new(path, name).leds()
    }

    /// Sends `request` and remembers it (to send again after a reconnection).
    pub fn send(&mut self, request: LedRequest) -> Result<(), ClientError> {
        self.remember(&request);
        if self.conn.stream.is_none() && self.conn.options.reconnect {
            self.conn.connect()?;
        }
        self.conn.send(&Request::Led(request))
    }

    /// Sends `request` and waits for the service to show it: a refused look
    /// (an unknown name, an invalid look) is an error, and the light keeps
    /// its look. Remembered for a reconnection only once accepted.
    pub fn send_checked(&mut self, mut request: LedRequest) -> Result<(), ClientError> {
        request.id = self.conn.next_id();
        let id = request.id;
        let keep = request.clone();
        let reply = self.conn.request(&Request::Led(request), |m| match m {
            Message::Text { id: got, result } if *got == id => Some(result.clone()),
            _ => None,
        })?;
        match reply {
            Ok(_) => {
                self.remember(&keep);
                Ok(())
            }
            Err(reason) => Err(ClientError::Rejected(reason)),
        }
    }

    /// Shows the named look (a built-in, or one from a look file), until
    /// replaced or cleared. Waits for the service; an unknown name is
    /// [`ClientError::Rejected`].
    pub fn look(&mut self, name: &str) -> Result<(), ClientError> {
        self.send_checked(LedRequest::new(LedShow::Look {
            name: name.to_string(),
            progress: None,
        }))
    }

    /// Shows a look given in full (`spec`), until replaced or cleared. A
    /// spec the service does not accept is [`ClientError::Rejected`], with
    /// the reason.
    pub fn show_spec(&mut self, spec: &lemnos_light::LookSpec) -> Result<(), ClientError> {
        self.send_checked(LedRequest::new(LedShow::Inline {
            spec: Box::new(*spec),
            progress: None,
        }))
    }

    /// Sends a look request as built (its `show` a [`LedShow::Look`] or
    /// [`LedShow::Inline`], with brightness, duration and the rest), waiting
    /// for the service's answer.
    pub fn send_look(&mut self, request: LedRequest) -> Result<(), ClientError> {
        self.send_checked(request)
    }

    /// A look-management request (list, show, reload, save), answered with
    /// the service's text. A refusal is [`ClientError::Rejected`].
    pub fn looks(&mut self, op: LooksOp) -> Result<String, ClientError> {
        let id = self.conn.next_id();
        let reply = self.conn.request(&Request::Looks { id, op }, |m| match m {
            Message::Text { id: got, result } if *got == id => Some(result.clone()),
            _ => None,
        })?;
        reply.map_err(ClientError::Rejected)
    }

    /// Bookkeeping for a request sent (or about to be): what a reconnection
    /// sends again.
    fn remember(&mut self, request: &LedRequest) {
        let request = request.clone();
        if request.show == LedShow::Clear {
            self.conn
                .held
                .retain(|r| r.device != request.device || (request.test && !r.test));
        } else {
            let layer = layer_of(&request);
            self.conn
                .held
                .retain(|r| r.device != request.device || layer_of(r) != layer);
            // Test intents are leases the caller renews: not re-sent.
            if request.duration_ms.is_none() && !request.test {
                self.conn.held.push(request);
            }
        }
    }

    /// Shows `status` with the light's default effect.
    pub fn status(&mut self, status: lemnos_light::Status) -> Result<(), ClientError> {
        self.send(LedRequest::new(LedShow::Status(status)))
    }

    /// Every LED `0xRRGGBB`.
    pub fn color(&mut self, rgb: u32) -> Result<(), ClientError> {
        self.send(LedRequest::new(LedShow::Color(rgb)))
    }

    /// One `0xWWRRGGBB` colour per LED.
    pub fn frame(&mut self, pixels: &[u32]) -> Result<(), ClientError> {
        self.send(LedRequest::new(LedShow::Frame(pixels.to_vec())))
    }

    /// Sets single LEDs `(logical index, 0xWWRRGGBB)`, keeping this client's
    /// other LEDs; index 0 is the ring's top.
    pub fn set_leds(&mut self, pixels: &[(u16, u32)]) -> Result<(), ClientError> {
        self.send(LedRequest::new(LedShow::Pixels(pixels.to_vec())))
    }

    /// A gauge filled to `fraction` (0.0-1.0) in `0xRRGGBB` (`None`: the
    /// board's progress colour); the fill advances eased.
    pub fn progress(&mut self, fraction: f32, color: Option<u32>) -> Result<(), ClientError> {
        let fraction = (fraction.clamp(0.0, 1.0) * 1000.0).round() as u16;
        self.send(LedRequest::new(LedShow::Progress {
            fraction,
            color,
            background: None,
        }))
    }

    /// A spinner for progress of an unknown amount.
    pub fn indeterminate(&mut self, color: Option<u32>) -> Result<(), ClientError> {
        self.send(LedRequest::new(LedShow::Indeterminate { color }))
    }

    /// Comets going round (see [`LedShow::Orbit`]): `color` (`None`: the
    /// board's progress colour), `period` per turn (`None`: the board's
    /// spinner period), `tail` in LEDs (fractions allowed), `heads` (1 or
    /// 2) and `base`, the floor brightness (0 to 1). A searching look, say.
    pub fn orbit(
        &mut self,
        color: Option<u32>,
        period_ms: Option<u32>,
        tail: Option<f32>,
        heads: u8,
        base: Option<f32>,
    ) -> Result<(), ClientError> {
        let mut request = LedRequest::new(LedShow::Orbit {
            color,
            tail: tail.map(|t| (t.clamp(0.0, 64.0) * 1000.0).round() as u16),
            heads,
            base: base.map(|b| (b.clamp(0.0, 1.0) * 1000.0).round() as u16),
        });
        request.period_ms = period_ms;
        self.send(request)
    }

    /// A built-in system animation (updating, booting, rebooting, update
    /// failed, rolled back, confirmed), above application status.
    pub fn system(&mut self, state: lemnos_light::SystemState) -> Result<(), ClientError> {
        self.send(LedRequest::new(LedShow::System(state)))
    }

    /// Shows the board's locate look over everything for `duration`.
    pub fn locate(&mut self, duration: Duration) -> Result<(), ClientError> {
        let mut request = LedRequest::new(LedShow::Locate);
        request.duration_ms = Some(duration.as_millis().min(u128::from(u32::MAX - 1)) as u32);
        self.send(request)
    }

    /// Shows `show` in the test layer, over every client's status, for
    /// `lease` (`None`: the service's default, 10 s in `lemnosd`). Send it
    /// again to renew it; it falls back to the layers below when the lease
    /// runs out or this client disconnects.
    pub fn test(&mut self, show: LedShow, lease: Option<Duration>) -> Result<(), ClientError> {
        let mut request = LedRequest::new(show).test();
        request.duration_ms = lease.map(|d| d.as_millis().min(u128::from(u32::MAX - 1)) as u32);
        self.send(request)
    }

    /// Drops this client's test intent.
    pub fn clear_test(&mut self) -> Result<(), ClientError> {
        self.send(LedRequest::new(LedShow::Clear).test())
    }

    /// Drops this client's intents.
    pub fn clear(&mut self) -> Result<(), ClientError> {
        self.send(LedRequest::new(LedShow::Clear))
    }

    /// Waits for the service to process what was sent (a round trip).
    pub fn sync(&mut self) -> Result<(), ClientError> {
        self.conn.request(&Request::List, |m| {
            matches!(m, Message::Devices(_)).then_some(())
        })
    }
}

fn layer_of(request: &LedRequest) -> u8 {
    if request.test {
        return 5;
    }
    match &request.show {
        LedShow::Clear => 0,
        LedShow::Color(_)
        | LedShow::Frame(_)
        | LedShow::Pixels(_)
        | LedShow::Progress { .. }
        | LedShow::Indeterminate { .. }
        | LedShow::Orbit { .. } => 1,
        LedShow::Look { .. } | LedShow::Inline { .. } => 1,
        LedShow::Status(_) => 2,
        LedShow::System(_) => 3,
        LedShow::Locate => 4,
    }
}

impl LedClient {
    /// The next event (connection changes, and, with
    /// [`ClientOptions::events`], LED-owner and
    /// other service events), waiting up to `wait` (`None`: forever).
    pub fn next_event(
        &mut self,
        wait: Option<Duration>,
    ) -> Result<Option<ClientEvent<Event>>, ClientError> {
        let deadline = wait.map(|w| Instant::now() + w);
        loop {
            let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
            match self.conn.next(left)? {
                None => return Ok(None),
                Some(ClientEvent::Connected { reconnects }) => {
                    return Ok(Some(ClientEvent::Connected { reconnects }));
                }
                Some(ClientEvent::Disconnected { error }) => {
                    return Ok(Some(ClientEvent::Disconnected { error }));
                }
                Some(ClientEvent::Data(Message::Event(event))) => {
                    return Ok(Some(ClientEvent::Data(event)));
                }
                Some(ClientEvent::Data(_)) => {}
            }
        }
    }
}
