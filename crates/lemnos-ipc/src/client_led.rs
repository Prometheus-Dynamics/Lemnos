//! [`LedClient`]: LED intents on `lemnosd`'s lights.

use super::{ClientError, ClientEvent, ClientOptions, Connection};
use crate::wire::{Event, LedRequest, LedShow, Message, Request};
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
                self.conn.held.push(request.clone());
            }
        }
        if self.conn.stream.is_none() && self.conn.options.reconnect {
            self.conn.connect()?;
        }
        self.conn.send(&Request::Led(request))
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

    /// A built-in system animation (updating, booting, rebooting, update
    /// failed, rolled back), above application status.
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
        | LedShow::Indeterminate { .. } => 1,
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
