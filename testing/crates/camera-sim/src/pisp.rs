//! The two PiSP blocks as the modules would present them to the drivers, over files.
//!
//! The drivers are the real ones: `lemnos-driver-pisp` binds `pisp-fe` and `pisp-be` as it does on a
//! board, and only the block under them is replaced ([`install`]). Each stand-in does what the
//! module and the silicon do for a job, by device address: it checks the configuration the way the
//! module does (`fe_job::check`, `be_job` shapes), takes it from the memory the driver gave the
//! module, reads and writes only what was registered, and finishes the job. What it makes is the
//! shared scene ([`crate::scene`]): the front end writes it as 16-bit Bayer at the frame rate the
//! sensor model's registers give, the back end reads its input and writes NV12 whose luma is the
//! input's high byte.

use std::collections::VecDeque;
use std::os::fd::{AsFd, BorrowedFd};
use std::sync::{Arc, Mutex, PoisonError};

use lemnos_contract_camera_isp::Counters;
use lemnos_core::{Result, Status};
use lemnos_driver_pisp::be_hw as be;
use lemnos_driver_pisp::be_job::{KernelJob, addr_output};
use lemnos_driver_pisp::fe_hw as fe;
use lemnos_driver_pisp::fe_job::{self, CONFIG_BYTES as FE_CONFIG_BYTES, Fixed, STATS_BYTES};
use lemnos_driver_pisp::uapi::{BeTilesConfig, FeConfig, Wire};
use lemnos_driver_pisp_sys::CONFIG_BYTES as BE_CONFIG_BYTES;
use lemnos_linux_sys::mmap::Mapping;

use crate::scene;

/// Gives every `pisp-fe` and `pisp-be` this process binds a simulated block. `false` if either
/// already has a block installed: nothing is changed then.
#[must_use]
pub fn install() -> bool {
    be::install_simulated(|_| Box::new(BeBlock::default()))
        && fe::install_simulated(|_| Box::new(FeBlock::default()))
}

fn now_ns() -> u64 {
    lemnos_linux_sys::time::monotonic_now()
        .map(|t| u64::try_from(t.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Memory the block can reach, by device address: a dma-buf (or a memfd where the host has no
/// heap) mapped whole, since a dma-buf can be mapped but not read or written with `pread`.
struct Region {
    addr: u64,
    map: Mapping,
    /// Which buffer it is: its device and inode.
    id: (u64, u64),
    size: u64,
}

impl Region {
    /// A region at `addr` over `memory`, which is mapped writable. `NO_MEMORY` if it can't be.
    fn over(memory: BorrowedFd<'_>, addr: u64) -> Result<Self> {
        let size = lemnos_linux_sys::dma_buf::size(memory).map_err(|_| Status::IO)?;
        let len = usize::try_from(size).map_err(|_| Status::IO)?;
        let map = Mapping::new(memory, len, 0, true).map_err(|_| Status::NO_MEMORY)?;
        Ok(Self {
            addr,
            map,
            id: identity(memory)?,
            size,
        })
    }
}

fn identity(fd: BorrowedFd<'_>) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let file = std::fs::File::from(fd.try_clone_to_owned().map_err(|_| Status::IO)?);
    let meta = file.metadata().map_err(|_| Status::IO)?;
    Ok((meta.dev(), meta.ino()))
}

/// The region `len` bytes at `addr` are in, and where in it.
fn find<'a>(
    mut regions: impl Iterator<Item = &'a Region>,
    addr: u64,
    len: u64,
) -> Option<(&'a Region, usize)> {
    regions.find_map(|r| {
        let end = addr.checked_add(len)?;
        if addr >= r.addr && end <= r.addr + r.size {
            Some((r, usize::try_from(addr - r.addr).ok()?))
        } else {
            None
        }
    })
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------- the front end

const FE_CONFIG: u64 = 0x1000_0000;
const FE_REGIONS: u64 = 0x8000_0000;
const FE_MEMORY: u64 = 0x4000_0000;
const REGION_SPAN: u64 = 0x0100_0000;
/// The module's room for registered memory.
const MAX_REGIONS: usize = 14;

#[derive(Default)]
struct FeState {
    configs: Option<Region>,
    regions: Vec<Option<Region>>,
    buffers: Vec<Option<Region>>,
    given: u32,
    output0_bytes: u32,
    fixed: Option<Fixed>,
    started: bool,
    pending: VecDeque<fe::Job>,
    finished: VecDeque<fe::Finished>,
    copy: Option<Box<[u8; FE_CONFIG_BYTES]>>,
    parsed: Box<FeConfig>,
    /// Output 0's picture, made at `go`.
    picture: Vec<u8>,
    next_start_ns: Option<u64>,
    sequence: u64,
    counters: Counters,
}

/// The front end's block.
#[derive(Clone, Default)]
pub struct FeBlock(Arc<Mutex<FeState>>);

impl FeState {
    fn reach(&self, addr: u64, len: u64) -> Option<(&Region, usize)> {
        let regions = self
            .configs
            .iter()
            .chain(self.regions.iter().flatten())
            .chain(self.buffers.iter().flatten());
        find(regions, addr, len)
    }

    fn put(&self, addr: u64, data: &[u8]) -> bool {
        self.reach(addr, data.len() as u64)
            .is_some_and(|(region, at)| region.map.write_bytes(at, data).is_some())
    }

    /// Runs the oldest job once a frame starts for it. The frame rate is the sensor's: its
    /// registers' frame period, from the first start after both are running. A start with no job
    /// waits for one instead of being dropped; only silicon shows a frame start with no job.
    fn run(&mut self) {
        if !self.started || !crate::world(|w| w.streaming) {
            self.next_start_ns = None;
            return;
        }
        let period = u64::try_from(crate::period_ps() / 1000)
            .unwrap_or(u64::MAX)
            .max(1);
        let now = now_ns();
        let due = *self.next_start_ns.get_or_insert(now + period);
        // The schedule is the sensor's and never moves: a start that finds no job waits for one,
        // and its frame keeps the time the sensor started it.
        if due > now || self.pending.is_empty() {
            return;
        }
        let Some(job) = self.pending.pop_front() else {
            return;
        };
        self.next_start_ns = Some(due + period);
        let Some(fixed) = self.fixed else {
            self.counters.errors += 1;
            return;
        };
        let config = job.range[0];
        let copy = self
            .copy
            .get_or_insert_with(|| Box::new([0; FE_CONFIG_BYTES]));
        let read = config.len == FE_CONFIG_BYTES as u64
            && find(
                self.configs.iter().chain(self.regions.iter().flatten()),
                config.addr,
                config.len,
            )
            .is_some_and(|(region, at)| {
                region
                    .map
                    .as_slice()
                    .get(at..at + FE_CONFIG_BYTES)
                    .is_some_and(|bytes| {
                        copy.copy_from_slice(bytes);
                        true
                    })
            });
        // The module's checks, on its copy: no address may come from the configuration.
        let shape = if read && self.parsed.read_from(&copy[..]).is_ok() {
            fe_job::check(&self.parsed, &fixed).ok()
        } else {
            None
        };
        let Some(shape) = shape.filter(|s| s.count() == job.count) else {
            self.counters.errors += 1;
            return;
        };
        let output0 = self
            .buffers
            .get(job.buffer as usize)
            .and_then(Option::as_ref)
            .map(|m| m.addr);
        let mut ok = output0.is_some_and(|addr| self.put(addr, &self.picture));
        let mut next = 1;
        if shape.stats {
            let r = job.range[next];
            let stats = vec![(job.identity as u8) | 1; STATS_BYTES.min(r.len as usize)];
            ok &= r.len == STATS_BYTES as u64 && self.put(r.addr, &stats);
            next += 1;
        }
        if shape.output1 {
            let r = job.range[next];
            let bytes = fe_job::output_bytes(&self.parsed, 1).unwrap_or(u64::MAX);
            ok &= bytes <= r.len && self.put(r.addr, &vec![1u8; bytes as usize]);
        }
        if !ok {
            self.counters.errors += 1;
        }
        self.counters.jobs += 1;
        self.finished.push_back(fe::Finished {
            buffer: job.buffer,
            bytes: self.output0_bytes,
            sequence: self.sequence,
            identity: job.identity,
            timestamp_ns: due,
        });
        self.sequence += 1;
    }
}

impl FeBlock {
    fn with<R>(&self, f: impl FnOnce(&mut FeState) -> R) -> R {
        f(&mut lock(&self.0))
    }
}

impl fe::Hardware for FeBlock {
    fn hw_version(&self) -> u32 {
        0x0225_2700
    }

    fn make(&mut self, plan: &fe::Plan) -> Result<fe::Placed> {
        let bytes = u64::from(plan.buffers) * fe::slot_bytes(FE_CONFIG_BYTES as u32);
        let configs = lemnos_linux_sys::dma_buf::sealed_memfd(c"sim-fe-configs", bytes)
            .map_err(|_| Status::IO)?;
        let configs = Region::over(configs.as_fd(), FE_CONFIG)?;
        self.with(|b| {
            b.configs = Some(configs);
            b.regions.clear();
            b.buffers = (0..plan.buffers).map(|_| None).collect();
            b.output0_bytes = plan.output0_bytes;
            b.pending.clear();
            b.finished.clear();
        });
        Ok(fe::Placed { config: FE_CONFIG })
    }

    fn set_buffer(&mut self, buffer: u32, memory: BorrowedFd<'_>) -> Result<fe::Set> {
        let region = Region::over(memory, 0)?;
        self.with(|b| {
            if region.size < u64::from(b.output0_bytes) {
                return Ok(fe::Set::Refused);
            }
            let busy = b.pending.iter().any(|j| j.buffer == buffer);
            let addr = FE_MEMORY + u64::from(b.given) * REGION_SPAN;
            let Some(slot) = b.buffers.get_mut(buffer as usize) else {
                return Ok(fe::Set::Refused);
            };
            if slot.as_ref().is_some_and(|m| m.id == region.id) {
                return Ok(fe::Set::Done);
            }
            if busy {
                return Ok(fe::Set::Busy);
            }
            *slot = Some(Region { addr, ..region });
            b.given += 1;
            Ok(fe::Set::Done)
        })
    }

    fn go(&mut self, start: &fe::Start) -> Result<()> {
        // The module's rule (`crate::EXCLUSIVE`): no front end while a raw stream runs.
        crate::take(crate::FRONT_END)?;
        self.with(|b| {
            let narrow = |v: u32| u16::try_from(v).map_err(|_| Status::INVALID_ARGUMENT);
            b.fixed = Some(Fixed {
                width: narrow(start.width)?,
                height: narrow(start.height)?,
                output_width: narrow(start.output_width)?,
                output_height: narrow(start.output_height)?,
                output_stride: start.output_stride,
                output_format: start.output_format,
            });
            b.picture = scene::frame16(start.output_stride as usize, b.output0_bytes as usize);
            b.started = true;
            b.sequence = 0;
            b.next_start_ns = None;
            b.counters = Counters::default();
            Ok(())
        })
    }

    fn stop(&mut self) {
        crate::release(crate::FRONT_END);
        self.with(|b| {
            b.started = false;
            b.pending.clear();
            b.regions.clear();
            b.buffers.clear();
        });
    }

    fn write_config(&mut self, slot: u32, config: &[u8; FE_CONFIG_BYTES]) -> Result<()> {
        self.with(|b| {
            let at = u64::from(slot) * fe::slot_bytes(FE_CONFIG_BYTES as u32);
            let configs = b.configs.as_ref().ok_or(Status::NO_DEVICE)?;
            if at + FE_CONFIG_BYTES as u64 > configs.size {
                return Err(Status::INVALID_ARGUMENT);
            }
            configs
                .map
                .write_bytes(usize::try_from(at).map_err(|_| Status::IO)?, config)
                .ok_or(Status::IO)
        })
    }

    fn register(&mut self, memory: BorrowedFd<'_>) -> Result<fe::Registered> {
        let mapped = Region::over(memory, 0)?;
        self.with(|b| {
            let region = match b.regions.iter().position(Option::is_none) {
                Some(at) => at,
                None if b.regions.len() < MAX_REGIONS => {
                    b.regions.push(None);
                    b.regions.len() - 1
                }
                None => return Err(Status::NO_MEMORY),
            };
            let addr = FE_REGIONS + region as u64 * REGION_SPAN;
            let size = mapped.size;
            b.regions[region] = Some(Region { addr, ..mapped });
            Ok(fe::Registered {
                region: region as u32,
                addr,
                size,
            })
        })
    }

    fn unregister(&mut self, region: u32) -> Result<()> {
        self.with(|b| match b.regions.get_mut(region as usize) {
            Some(slot @ Some(_)) => {
                *slot = None;
                Ok(())
            }
            _ => Err(Status::NOT_FOUND),
        })
    }

    fn post(&mut self, job: &fe::Job) -> Result<()> {
        self.with(|b| b.pending.push_back(*job));
        Ok(())
    }

    fn next_finished(&mut self) -> Result<Option<fe::Finished>> {
        Ok(self.with(|b| {
            b.run();
            b.finished.pop_front()
        }))
    }

    fn give_back(&mut self, _buffer: u32) -> Result<()> {
        Ok(())
    }

    fn check(&mut self) -> Result<()> {
        Ok(())
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(self.with(|b| b.counters))
    }
}

// ---------------------------------------------------------------- the back end

const BE_POOL: u64 = 0x1000_0000;
const BE_REGIONS: u64 = 0x8000_0000;

struct BePending {
    job: KernelJob,
    config: Box<[u8; BE_CONFIG_BYTES]>,
}

#[derive(Default)]
struct BeState {
    /// The pool, then what was registered; a slot let go of is `None` and is used again.
    regions: Vec<Option<Region>>,
    pending: VecDeque<BePending>,
    finished: VecDeque<be::Finished>,
    parsed: Box<BeTilesConfig>,
    sequence: u64,
    last_ns: u64,
}

/// The back end's block.
#[derive(Clone, Default)]
pub struct BeBlock(Arc<Mutex<BeState>>);

impl BeState {
    /// Runs the oldest job: reads the input image the job names and writes NV12 whose luma is the
    /// input's high byte.
    fn run(&mut self) {
        let Some(BePending { job, config }) = self.pending.pop_front() else {
            return;
        };
        if self.parsed.read_from(&config[..]).is_err() {
            return;
        }
        let input = self.parsed.config.input_format;
        let output = self.parsed.config.output_format[0].image;
        let (width, height) = (usize::from(output.width), usize::from(output.height));
        let (in_stride, out_stride) = (input.stride as usize, output.stride as usize);
        let mut raw = vec![0u8; in_stride * usize::from(input.height)];
        let read = find(self.regions.iter().flatten(), job.addr[0], raw.len() as u64)
            .and_then(|(region, at)| region.map.as_slice().get(at..at + raw.len()))
            .is_some_and(|bytes| {
                raw.copy_from_slice(bytes);
                true
            });
        let mut luma = vec![0u8; out_stride * height];
        for (src, dst) in raw.chunks(in_stride).zip(luma.chunks_mut(out_stride)) {
            // The high byte of each 16-bit pixel, a line at a time.
            for (pixel, slot) in src
                .as_chunks::<2>()
                .0
                .iter()
                .take(width)
                .zip(dst.iter_mut())
            {
                *slot = pixel[1];
            }
        }
        let chroma = vec![128u8; out_stride * height / 2];
        let wrote = read
            && [
                (job.addr[addr_output(0, 0)], &luma),
                (job.addr[addr_output(0, 1)], &chroma),
            ]
            .into_iter()
            .all(|(addr, data)| {
                find(self.regions.iter().flatten(), addr, data.len() as u64)
                    .is_some_and(|(region, at)| region.map.write_bytes(at, data).is_some())
            });
        // A job that did not complete is a finished job with no bytes: the block flagged it.
        let bytes = if wrote { 4096 } else { 0 };
        // Timestamps strictly increase, as frame completions do.
        self.last_ns = now_ns().max(self.last_ns + 1);
        self.finished.push_back(be::Finished {
            buffer: job.buffer,
            bytes,
            sequence: self.sequence,
            identity: job.identity,
            timestamp_ns: self.last_ns,
        });
        self.sequence += 1;
    }
}

impl BeBlock {
    fn with<R>(&self, f: impl FnOnce(&mut BeState) -> R) -> R {
        f(&mut lock(&self.0))
    }
}

impl be::Hardware for BeBlock {
    fn hw_version(&self) -> u32 {
        0x0225_2700
    }

    fn start(&mut self, plan: &be::Plan) -> Result<be::Placed> {
        let pool_bytes = 4096 * u64::from(plan.buffers);
        let pool = lemnos_linux_sys::dma_buf::sealed_memfd(c"sim-be-pool", pool_bytes)
            .map_err(|_| Status::IO)?;
        let pool = Region::over(pool.as_fd(), BE_POOL)?;
        self.with(|b| {
            b.regions.clear();
            b.regions.push(Some(pool));
            b.pending.clear();
            b.finished.clear();
            b.sequence = 0;
        });
        Ok(be::Placed {
            tdn: plan.tdn_bytes.map(|_| [0x4000_0000, 0x4100_0000]),
        })
    }

    fn stop(&mut self) {
        self.with(|b| {
            b.regions.clear();
            b.pending.clear();
        });
    }

    fn register(&mut self, memory: BorrowedFd<'_>) -> Result<be::Registered> {
        let mapped = Region::over(memory, 0)?;
        self.with(|b| {
            let region = match b.regions.iter().skip(1).position(Option::is_none) {
                Some(at) => at + 1,
                None => {
                    b.regions.push(None);
                    b.regions.len() - 1
                }
            };
            let addr = BE_REGIONS + region as u64 * REGION_SPAN;
            let size = mapped.size;
            b.regions[region] = Some(Region { addr, ..mapped });
            Ok(be::Registered {
                region: region as u32,
                addr,
                size,
            })
        })
    }

    fn unregister(&mut self, region: u32) -> Result<()> {
        self.with(|b| match b.regions.get_mut(region as usize) {
            Some(slot @ Some(_)) if region != 0 => {
                *slot = None;
                Ok(())
            }
            _ => Err(Status::NOT_FOUND),
        })
    }

    fn post(&mut self, job: &KernelJob, config: &[u8; BE_CONFIG_BYTES]) -> Result<()> {
        self.with(|b| {
            b.pending.push_back(BePending {
                job: *job,
                config: Box::new(*config),
            });
        });
        Ok(())
    }

    fn next_finished(&mut self) -> Result<Option<be::Finished>> {
        Ok(self.with(|b| {
            b.run();
            b.finished.pop_front()
        }))
    }

    fn give_back(&mut self, _buffer: u32) -> Result<()> {
        Ok(())
    }

    fn check(&mut self) -> Result<()> {
        Ok(())
    }
}
