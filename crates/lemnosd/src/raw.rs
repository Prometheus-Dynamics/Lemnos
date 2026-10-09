//! Raw bus and line access for clients: lemnosd stays the one owner of the
//! hardware and brokers it.
//!
//! - GPIO lines and PWM channels are claimed per connection (exclusive);
//!   I2C addresses and SPI chip selects need no claim for single
//!   transactions (each is atomic here) and can be locked for sequences.
//! - What a board device owns is refused (`Owned`): its lines and channels
//!   always, its I2C/SPI addresses unless the device lists the client in
//!   `raw`.
//! - Claims and locks end with the connection (unless the client keeps its
//!   intents): lines go to their safe state, PWM channels are disabled.

use crate::clients::Requester;
use lemnos_board::raw::{
    DynLine, DynPwm, DynSpi, Owner, Resource, line_safe_state, owned_resources,
};
use lemnos_board::{BoardDefinition, BusRef, Buses, DriverRegistry};
use lemnos_hal::raw::{
    EdgeDetect, LineConfig, PwmConfig, RawLine, RawPwm, RawSpi, SafeState, SpiSegment,
};
use lemnos_hal::{ErrorKind, HalError};
use lemnos_ipc::{Event, I2cOp, LineTarget, Message, PwmTarget, RawRequest, Refusal};
use std::os::fd::RawFd;

/// Largest I2C transaction, bytes written plus read.
pub(crate) const MAX_I2C_BYTES: usize = 4096;
/// Largest SPI transaction, bytes sent plus received.
pub(crate) const MAX_SPI_BYTES: usize = 64 * 1024;
/// Most lines and channels one client may hold.
pub(crate) const MAX_CLAIMS: usize = 64;

/// Who holds a claim or lock: a connection, and (for clients that keep
/// their intents) a name that outlives it.
#[derive(Debug, Clone)]
struct Holder {
    client: u32,
    keep_name: Option<String>,
}

impl Holder {
    fn of(who: &Requester<'_>) -> Self {
        Self {
            client: who.id,
            keep_name: who.keep.then(|| who.name.to_string()),
        }
    }

    fn is(&self, who: &Requester<'_>) -> bool {
        self.client == who.id || self.keep_name.as_deref() == Some(who.name)
    }
}

enum Held {
    Line {
        line: DynLine,
        safe: SafeState,
        edges: bool,
    },
    Pwm(DynPwm),
}

struct Claim {
    handle: u32,
    holder: Holder,
    resource: Resource,
    held: Held,
}

struct Lock {
    resource: Resource,
    holder: Holder,
}

/// The board and buses a raw request works against.
pub(crate) struct Context<'a> {
    pub buses: &'a mut dyn Buses,
    pub board: &'a BoardDefinition,
    pub registry: &'a DriverRegistry,
}

impl Context<'_> {
    fn owned(&self) -> Vec<(Resource, Owner)> {
        let buses = &*self.buses;
        owned_resources(self.board, self.registry, &buses.sys(), &|c| {
            buses.line_chip_id(c)
        })
    }

    fn i2c_bus(&self, bus: &str) -> Result<u32, Refusal> {
        if let Ok(number) = bus.parse::<u32>() {
            return Ok(number);
        }
        let parsed: BusRef = bus
            .parse()
            .map_err(|_| Refusal::Device(ErrorKind::InvalidInput))?;
        match parsed.i2c_bus(&self.buses.sys()) {
            Some(Ok(number)) => Ok(number),
            Some(Err(_)) => Err(Refusal::Device(ErrorKind::NotFound)),
            None => Err(Refusal::Device(ErrorKind::InvalidInput)),
        }
    }
}

/// Every claim and lock, with the buses opened for transactions.
#[derive(Default)]
pub(crate) struct RawState {
    claims: Vec<Claim>,
    locks: Vec<Lock>,
    next_handle: u32,
    i2c: Vec<(u32, lemnos_board::DynI2c)>,
    spi: Vec<((u32, u16), DynSpi)>,
}

fn reply(id: u32, result: Result<f64, Refusal>) -> Message {
    Message::Reply { id, result }
}

impl RawState {
    /// Answers one request.
    pub fn handle(
        &mut self,
        request: RawRequest,
        who: &Requester<'_>,
        ctx: &mut Context<'_>,
    ) -> Message {
        let id = request.id();
        if !ctx.board.raw_clients.is_empty() && !ctx.board.raw_clients.iter().any(|c| c == who.name)
        {
            return match request {
                RawRequest::LineClaim { .. } | RawRequest::PwmClaim { .. } => Message::Claimed {
                    id,
                    result: Err(Refusal::NotAllowed),
                },
                RawRequest::I2cTransfer { .. } | RawRequest::SpiTransfer { .. } => Message::Data {
                    id,
                    result: Err(Refusal::NotAllowed),
                },
                _ => reply(id, Err(Refusal::NotAllowed)),
            };
        }
        match request {
            RawRequest::LineClaim {
                line,
                config,
                on_release,
                ..
            } => Message::Claimed {
                id,
                result: self.claim_line(line, config, on_release, who, ctx),
            },
            RawRequest::PwmClaim { pwm, .. } => Message::Claimed {
                id,
                result: self.claim_pwm(pwm, who, ctx),
            },
            RawRequest::LineConfigure { handle, config, .. } => reply(
                id,
                self.line(handle, who).and_then(|(line, edges)| {
                    *edges = config.edge != EdgeDetect::None;
                    line.configure(&config)
                        .map(|()| 0.0)
                        .map_err(Refusal::Device)
                }),
            ),
            RawRequest::LineGet { handle, .. } => reply(
                id,
                self.line(handle, who).and_then(|(line, _)| {
                    line.get()
                        .map(|v| f64::from(u8::from(v)))
                        .map_err(Refusal::Device)
                }),
            ),
            RawRequest::LineSet { handle, value, .. } => reply(
                id,
                self.line(handle, who)
                    .and_then(|(line, _)| line.set(value).map(|()| 0.0).map_err(Refusal::Device)),
            ),
            RawRequest::PwmConfigure { handle, config, .. } => reply(
                id,
                self.pwm(handle, who).and_then(|pwm| {
                    pwm.configure(&config)
                        .map(|()| 0.0)
                        .map_err(Refusal::Device)
                }),
            ),
            RawRequest::Unclaim { handle, .. } => {
                reply(id, self.unclaim(handle, who).map(|()| 0.0))
            }
            RawRequest::I2cTransfer {
                bus, address, ops, ..
            } => Message::Data {
                id,
                result: self.i2c_transfer(&bus, address, ops, who, ctx),
            },
            RawRequest::I2cLock {
                bus, address, lock, ..
            } => reply(
                id,
                ctx.i2c_bus(&bus).and_then(|bus| {
                    self.lock(Resource::I2c { bus, address }, lock, who, ctx)
                        .map(|()| 0.0)
                }),
            ),
            RawRequest::SpiTransfer {
                bus,
                chip_select,
                transfers,
                ..
            } => Message::Data {
                id,
                result: self.spi_transfer(bus, chip_select, transfers, who, ctx),
            },
            RawRequest::SpiLock {
                bus,
                chip_select,
                lock,
                ..
            } => reply(
                id,
                self.lock(Resource::Spi { bus, chip_select }, lock, who, ctx)
                    .map(|()| 0.0),
            ),
        }
    }

    /// Refuses `resource` if a board device owns it (unless `raw_ok` and the
    /// device lists the client) or someone else holds it.
    fn check_free(
        &self,
        resource: &Resource,
        who: &Requester<'_>,
        owned: &[(Resource, Owner)],
        raw_ok: bool,
    ) -> Result<(), Refusal> {
        if let Some((_, owner)) = owned.iter().find(|(r, _)| r == resource)
            && !(raw_ok && owner.raw.iter().any(|c| c == who.name))
        {
            return Err(Refusal::Owned);
        }
        let held_by_other = self
            .claims
            .iter()
            .filter(|c| &c.resource == resource)
            .map(|c| &c.holder)
            .chain(
                self.locks
                    .iter()
                    .filter(|l| &l.resource == resource)
                    .map(|l| &l.holder),
            )
            .any(|h| !h.is(who));
        if held_by_other {
            return Err(Refusal::Claimed);
        }
        Ok(())
    }

    fn new_handle(&mut self) -> u32 {
        self.next_handle = self.next_handle.wrapping_add(1).max(1);
        self.next_handle
    }

    fn count(&self, who: &Requester<'_>) -> usize {
        self.claims.iter().filter(|c| c.holder.is(who)).count()
    }

    fn claim_line(
        &mut self,
        target: LineTarget,
        config: LineConfig,
        on_release: Option<SafeState>,
        who: &Requester<'_>,
        ctx: &mut Context<'_>,
    ) -> Result<u32, Refusal> {
        let (chip, offset) = match target {
            LineTarget::Chip { chip, offset } => (ctx.buses.line_chip_id(&chip), offset),
            LineTarget::Name(name) => match ctx.board.lines.iter().find(|l| l.name == name) {
                Some(line) => (ctx.buses.line_chip_id(&line.chip), line.line),
                None => ctx
                    .buses
                    .find_line(&name)
                    .ok_or(Refusal::Device(ErrorKind::NotFound))?,
            },
        };
        let resource = Resource::Line {
            chip: chip.clone(),
            offset,
        };
        self.check_free(&resource, who, &ctx.owned(), false)?;
        if self.claims.iter().any(|c| c.resource == resource) {
            return Err(Refusal::Claimed);
        }
        if self.count(who) >= MAX_CLAIMS {
            return Err(Refusal::Device(ErrorKind::Busy));
        }
        let buses = &*ctx.buses;
        let safe = on_release
            .or_else(|| line_safe_state(ctx.board, &chip, offset, &|c| buses.line_chip_id(c)))
            .unwrap_or_default();
        let consumer = format!("lemnosd:{}", who.name);
        let line = ctx
            .buses
            .line(&chip, offset, &config, &consumer)
            .map_err(|e| Refusal::Device(e.kind()))?;
        let handle = self.new_handle();
        self.claims.push(Claim {
            handle,
            holder: Holder::of(who),
            resource,
            held: Held::Line {
                line,
                safe,
                edges: config.edge != EdgeDetect::None,
            },
        });
        Ok(handle)
    }

    fn claim_pwm(
        &mut self,
        target: PwmTarget,
        who: &Requester<'_>,
        ctx: &mut Context<'_>,
    ) -> Result<u32, Refusal> {
        let (chip, channel) = match target {
            PwmTarget::Chip { chip, channel } => (chip, channel),
            PwmTarget::Name(name) => ctx
                .board
                .pwms
                .iter()
                .find(|p| p.name == name)
                .map(|p| (p.chip, p.channel))
                .ok_or(Refusal::Device(ErrorKind::NotFound))?,
        };
        let resource = Resource::Pwm { chip, channel };
        self.check_free(&resource, who, &ctx.owned(), false)?;
        if self.claims.iter().any(|c| c.resource == resource) {
            return Err(Refusal::Claimed);
        }
        if self.count(who) >= MAX_CLAIMS {
            return Err(Refusal::Device(ErrorKind::Busy));
        }
        let pwm = ctx
            .buses
            .pwm(chip, channel)
            .map_err(|e| Refusal::Device(e.kind()))?;
        let handle = self.new_handle();
        self.claims.push(Claim {
            handle,
            holder: Holder::of(who),
            resource,
            held: Held::Pwm(pwm),
        });
        Ok(handle)
    }

    fn claim_mut(&mut self, handle: u32, who: &Requester<'_>) -> Result<&mut Claim, Refusal> {
        self.claims
            .iter_mut()
            .find(|c| c.handle == handle && c.holder.is(who))
            .ok_or(Refusal::UnknownHandle)
    }

    fn line(
        &mut self,
        handle: u32,
        who: &Requester<'_>,
    ) -> Result<(&mut DynLine, &mut bool), Refusal> {
        match &mut self.claim_mut(handle, who)?.held {
            Held::Line { line, edges, .. } => Ok((line, edges)),
            Held::Pwm(_) => Err(Refusal::UnknownHandle),
        }
    }

    fn pwm(&mut self, handle: u32, who: &Requester<'_>) -> Result<&mut DynPwm, Refusal> {
        match &mut self.claim_mut(handle, who)?.held {
            Held::Pwm(pwm) => Ok(pwm),
            Held::Line { .. } => Err(Refusal::UnknownHandle),
        }
    }

    fn unclaim(&mut self, handle: u32, who: &Requester<'_>) -> Result<(), Refusal> {
        let index = self
            .claims
            .iter()
            .position(|c| c.handle == handle && c.holder.is(who))
            .ok_or(Refusal::UnknownHandle)?;
        release(self.claims.remove(index));
        Ok(())
    }

    fn lock(
        &mut self,
        resource: Resource,
        lock: bool,
        who: &Requester<'_>,
        ctx: &Context<'_>,
    ) -> Result<(), Refusal> {
        if !lock {
            self.locks
                .retain(|l| !(l.resource == resource && l.holder.is(who)));
            return Ok(());
        }
        // Locking a device's address would stall the device: never allowed.
        self.check_free(&resource, who, &ctx.owned(), false)?;
        if !self.locks.iter().any(|l| l.resource == resource) {
            self.locks.push(Lock {
                resource,
                holder: Holder::of(who),
            });
        }
        Ok(())
    }

    fn i2c_transfer(
        &mut self,
        bus: &str,
        address: u16,
        ops: Vec<I2cOp>,
        who: &Requester<'_>,
        ctx: &mut Context<'_>,
    ) -> Result<Vec<u8>, Refusal> {
        let bus = ctx.i2c_bus(bus)?;
        let size: usize = ops
            .iter()
            .map(|op| match op {
                I2cOp::Write(bytes) => bytes.len(),
                I2cOp::Read(n) => usize::from(*n),
            })
            .sum();
        if size > MAX_I2C_BYTES || address > 0x7f || ops.is_empty() {
            return Err(Refusal::OutOfRange);
        }
        self.check_free(&Resource::I2c { bus, address }, who, &ctx.owned(), true)?;
        let index = match self.i2c.iter().position(|(b, _)| *b == bus) {
            Some(index) => index,
            None => {
                let opened = ctx.buses.i2c(bus).map_err(|e| Refusal::Device(e.kind()))?;
                self.i2c.push((bus, opened));
                self.i2c.len() - 1
            }
        };
        let mut reads: Vec<Vec<u8>> = ops
            .iter()
            .map(|op| match op {
                I2cOp::Read(n) => vec![0u8; usize::from(*n)],
                I2cOp::Write(_) => Vec::new(),
            })
            .collect();
        {
            use embedded_hal::i2c::{I2c, Operation};
            let mut operations: Vec<Operation<'_>> = ops
                .iter()
                .zip(reads.iter_mut())
                .map(|(op, buf)| match op {
                    I2cOp::Write(bytes) => Operation::Write(bytes),
                    I2cOp::Read(_) => Operation::Read(buf),
                })
                .collect();
            let address = u8::try_from(address).map_err(|_| Refusal::OutOfRange)?;
            if let Err(kind) = self.i2c[index].1.transaction(address, &mut operations) {
                // A failing bus may be gone (hotplug): open it again next time.
                if !matches!(kind, ErrorKind::Nack) {
                    self.i2c.remove(index);
                }
                return Err(Refusal::Device(kind));
            }
        }
        Ok(reads.concat())
    }

    fn spi_transfer(
        &mut self,
        bus: u32,
        chip_select: u16,
        transfers: Vec<lemnos_ipc::SpiXfer>,
        who: &Requester<'_>,
        ctx: &mut Context<'_>,
    ) -> Result<Vec<u8>, Refusal> {
        let size: usize = transfers
            .iter()
            .map(|t| t.tx.len() + usize::from(t.rx_len))
            .sum();
        if size > MAX_SPI_BYTES || transfers.is_empty() {
            return Err(Refusal::OutOfRange);
        }
        let key = (bus, chip_select);
        self.check_free(&Resource::Spi { bus, chip_select }, who, &ctx.owned(), true)?;
        let index = match self.spi.iter().position(|(k, _)| *k == key) {
            Some(index) => index,
            None => {
                let opened = ctx
                    .buses
                    .spi(bus, chip_select)
                    .map_err(|e| Refusal::Device(e.kind()))?;
                self.spi.push((key, opened));
                self.spi.len() - 1
            }
        };
        let mut reads: Vec<Vec<u8>> = transfers
            .iter()
            .map(|t| vec![0u8; usize::from(t.rx_len)])
            .collect();
        {
            let mut segments: Vec<SpiSegment<'_>> = transfers
                .iter()
                .zip(reads.iter_mut())
                .map(|(t, rx)| SpiSegment {
                    config: t.config,
                    tx: &t.tx,
                    rx,
                    cs_change: t.cs_change,
                    delay_us: t.delay_us,
                })
                .collect();
            if let Err(kind) = self.spi[index].1.transfer(&mut segments) {
                self.spi.remove(index);
                return Err(Refusal::Device(kind));
            }
        }
        Ok(reads.concat())
    }

    /// A connection closed: its claims and locks end, unless the client
    /// keeps its intents (then they wait for a release by name).
    pub fn client_gone(&mut self, client: u32, keep: bool) {
        if keep {
            for claim in self.claims.iter_mut().filter(|c| c.holder.client == client) {
                claim.holder.client = 0;
            }
            for lock in self.locks.iter_mut().filter(|l| l.holder.client == client) {
                lock.holder.client = 0;
            }
            return;
        }
        self.locks.retain(|l| l.holder.client != client);
        let (gone, kept): (Vec<Claim>, Vec<Claim>) = std::mem::take(&mut self.claims)
            .into_iter()
            .partition(|c| c.holder.client == client);
        self.claims = kept;
        gone.into_iter().for_each(release);
    }

    /// Pending edges of lines with edge detection: `(client, event)`.
    pub fn take_edges(&mut self) -> Vec<(u32, Event)> {
        let mut events = Vec::new();
        for claim in &mut self.claims {
            if let Held::Line {
                line, edges: true, ..
            } = &mut claim.held
            {
                while let Ok(Some(edge)) = line.read_edge() {
                    events.push((
                        claim.holder.client,
                        Event::Edge {
                            handle: claim.handle,
                            rising: edge.rising,
                            timestamp_ns: edge.timestamp_ns,
                            seq: edge.seq,
                        },
                    ));
                }
            }
        }
        events
    }

    /// Descriptors that become readable on an edge.
    pub fn edge_fds(&self) -> impl Iterator<Item = RawFd> + '_ {
        self.claims.iter().filter_map(|c| match &c.held {
            Held::Line {
                line, edges: true, ..
            } => line.fd(),
            _ => None,
        })
    }

    /// Whether some edge-detecting line has no descriptor (it must be
    /// polled on a timer).
    pub fn polls_edges(&self) -> bool {
        self.claims
            .iter()
            .any(|c| matches!(&c.held, Held::Line { line, edges: true, .. } if line.fd().is_none()))
    }

    /// Ends every claim (service stop).
    pub fn release_all(&mut self) {
        self.locks.clear();
        std::mem::take(&mut self.claims)
            .into_iter()
            .for_each(release);
    }
}

/// Ends a claim: the line goes to its safe state, the channel is disabled.
fn release(claim: Claim) {
    match claim.held {
        Held::Line { mut line, safe, .. } => {
            if let Some(config) = safe.config() {
                let _ = line.configure(&config);
            }
        }
        Held::Pwm(mut pwm) => {
            if let Ok(config) = pwm.config() {
                let _ = pwm.configure(&PwmConfig {
                    enabled: false,
                    ..config
                });
            }
        }
    }
}
