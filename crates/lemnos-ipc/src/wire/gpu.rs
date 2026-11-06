//! The advisory GPU lease on the wire (`Request::GpuAcquire` and friends,
//! `Message::GpuLease`, `Message::GpuState`).
//!
//! Cooperative only: the GPU's driver (Mesa) opens the render node itself, so
//! a lease grants nothing the kernel enforces. It tells clients that agree to
//! ask who else is using the GPU, in what order they get it, and when a
//! holder has kept it longer than it said it would. A client that never asks
//! keeps working as before. Added within protocol 1: a service that has it
//! sets [`FEATURE_GPU_LEASE`] in its welcome, and a client sends these
//! requests only then (an older service closes the connection on them).

use super::codec::{Decoder, Encoder};
use super::*;

/// The welcome's feature bit for the GPU lease requests.
pub const FEATURE_GPU_LEASE: u32 = 1;

/// How a lease shares its GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GpuMode {
    /// No other lease at the same time.
    #[default]
    Exclusive,
    /// Other shared leases at the same time, no exclusive one.
    Shared,
}

impl GpuMode {
    fn code(self) -> u8 {
        match self {
            Self::Exclusive => 0,
            Self::Shared => 1,
        }
    }

    /// Unknown codes are exclusive: the safe reading of a newer mode.
    fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Shared,
            _ => Self::Exclusive,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Exclusive => "exclusive",
            Self::Shared => "shared",
        }
    }
}

/// What a lease asks for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GpuAcquire {
    /// The GPU's name (see [`GpuInfo::name`]); empty: the first one.
    pub gpu: String,
    pub mode: GpuMode,
    /// Higher goes first among waiters (0-255); equal priorities in arrival
    /// order.
    pub priority: u8,
    /// Queue behind the current holders (`false`: refused with
    /// [`Refusal::Claimed`] unless granted at once).
    pub wait: bool,
    /// How long the holder expects to keep it, in ms (0: no limit). Past it
    /// the holder is told once ([`GpuLeaseState::Overdue`]) and the lease is
    /// shown overdue; nothing is taken back.
    pub max_hold_ms: u32,
    /// What it is for, shown in the GPU's state (`benchmark`, `pipeline`).
    pub label: String,
}

/// Where a lease stands, in [`Message::GpuLease`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuLeaseState {
    /// The lease is held.
    Granted,
    /// Waiting, with this many leases ahead of it (holders and waiters).
    Queued { ahead: u16 },
    /// Not granted and not queued.
    Refused(Refusal),
    /// A waiter of higher priority is held back by this lease (sent once
    /// per lease). Advisory: the holder decides whether to release.
    YieldAsked,
    /// Held past its `max_hold_ms` (sent once per lease).
    Overdue,
}

/// One lease, held or waiting, in [`GpuInfo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuHolder {
    pub lease: u32,
    pub client_id: u32,
    /// The client's name from its greeting.
    pub client: String,
    pub label: String,
    pub mode: GpuMode,
    pub priority: u8,
    /// Held for (a holder) or waited for (a waiter), in ms.
    pub for_ms: u64,
    pub max_hold_ms: u32,
    /// Held past `max_hold_ms`.
    pub overdue: bool,
}

/// The last hang a client reported on a GPU ([`Request::GpuHang`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuHangReport {
    pub client: String,
    pub label: String,
    pub detail: String,
    /// How long ago, in ms.
    pub ago_ms: u64,
}

/// One GPU and its leases, in [`Message::GpuState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuInfo {
    /// The name leases use: the kernel driver's (`v3d`), with a number after
    /// it when the board has more than one GPU of that driver (`v3d1`).
    pub name: String,
    pub driver: String,
    /// The render node (`/dev/dri/renderD128`).
    pub node: String,
    pub holders: Vec<GpuHolder>,
    /// In the order they will be granted.
    pub waiters: Vec<GpuHolder>,
    /// Hangs reported since the service started.
    pub hangs: u32,
    pub last_hang: Option<GpuHangReport>,
}

pub(super) fn acquire_encode(e: &mut Encoder, a: &GpuAcquire) {
    e.str(&a.gpu)
        .u8(a.mode.code())
        .u8(a.priority)
        .u8(u8::from(a.wait))
        .u32(a.max_hold_ms)
        .str(&a.label);
}

pub(super) fn acquire_decode(d: &mut Decoder<'_>) -> Result<GpuAcquire, WireError> {
    Ok(GpuAcquire {
        gpu: d.str()?,
        mode: GpuMode::from_code(d.u8()?),
        priority: d.u8()?,
        wait: d.u8()? != 0,
        max_hold_ms: d.u32()?,
        label: d.str()?,
    })
}

pub(super) fn state_encode(e: &mut Encoder, state: GpuLeaseState) {
    let (tag, (code, kind), ahead) = match state {
        GpuLeaseState::Granted => (0, (0, 0), 0),
        GpuLeaseState::Queued { ahead } => (1, (0, 0), ahead),
        GpuLeaseState::Refused(r) => (2, r.code(), 0),
        GpuLeaseState::YieldAsked => (3, (0, 0), 0),
        GpuLeaseState::Overdue => (4, (0, 0), 0),
    };
    e.u8(tag).u8(code).u8(kind).u16(ahead);
}

pub(super) fn state_decode(d: &mut Decoder<'_>) -> Result<GpuLeaseState, WireError> {
    let (tag, code, kind, ahead) = (d.u8()?, d.u8()?, d.u8()?, d.u16()?);
    Ok(match tag {
        0 => GpuLeaseState::Granted,
        1 => GpuLeaseState::Queued { ahead },
        2 => GpuLeaseState::Refused(Refusal::from_code(code, kind)),
        3 => GpuLeaseState::YieldAsked,
        4 => GpuLeaseState::Overdue,
        other => return Err(bad(format!("gpu lease state {other}"))),
    })
}

fn holder_encode(e: &mut Encoder, h: &GpuHolder) {
    e.u32(h.lease)
        .u32(h.client_id)
        .str(&h.client)
        .str(&h.label)
        .u8(h.mode.code())
        .u8(h.priority)
        .u64(h.for_ms)
        .u32(h.max_hold_ms)
        .u8(u8::from(h.overdue));
}

fn holder_decode(d: &mut Decoder<'_>) -> Result<GpuHolder, WireError> {
    Ok(GpuHolder {
        lease: d.u32()?,
        client_id: d.u32()?,
        client: d.str()?,
        label: d.str()?,
        mode: GpuMode::from_code(d.u8()?),
        priority: d.u8()?,
        for_ms: d.u64()?,
        max_hold_ms: d.u32()?,
        overdue: d.u8()? != 0,
    })
}

fn list_encode(e: &mut Encoder, list: &[GpuHolder]) {
    e.u16(list.len().min(usize::from(u16::MAX)) as u16);
    for h in list.iter().take(usize::from(u16::MAX)) {
        holder_encode(e, h);
    }
}

fn list_decode(d: &mut Decoder<'_>) -> Result<Vec<GpuHolder>, WireError> {
    let n = usize::from(d.u16()?);
    let mut out = Vec::with_capacity(n.min(16));
    for _ in 0..n {
        out.push(holder_decode(d)?);
    }
    Ok(out)
}

pub(super) fn gpus_encode(e: &mut Encoder, gpus: &[GpuInfo]) {
    e.u16(gpus.len().min(usize::from(u16::MAX)) as u16);
    for g in gpus.iter().take(usize::from(u16::MAX)) {
        e.str(&g.name).str(&g.driver).str(&g.node);
        list_encode(e, &g.holders);
        list_encode(e, &g.waiters);
        e.u32(g.hangs);
        match &g.last_hang {
            None => {
                e.u8(0);
            }
            Some(h) => {
                e.u8(1)
                    .str(&h.client)
                    .str(&h.label)
                    .str(&h.detail)
                    .u64(h.ago_ms);
            }
        }
    }
}

pub(super) fn gpus_decode(d: &mut Decoder<'_>) -> Result<Vec<GpuInfo>, WireError> {
    let n = usize::from(d.u16()?);
    let mut out = Vec::with_capacity(n.min(8));
    for _ in 0..n {
        let (name, driver, node) = (d.str()?, d.str()?, d.str()?);
        let holders = list_decode(d)?;
        let waiters = list_decode(d)?;
        let hangs = d.u32()?;
        let last_hang = match d.u8()? {
            0 => None,
            _ => Some(GpuHangReport {
                client: d.str()?,
                label: d.str()?,
                detail: d.str()?,
                ago_ms: d.u64()?,
            }),
        };
        out.push(GpuInfo {
            name,
            driver,
            node,
            holders,
            waiters,
            hangs,
            last_hang,
        });
    }
    Ok(out)
}
