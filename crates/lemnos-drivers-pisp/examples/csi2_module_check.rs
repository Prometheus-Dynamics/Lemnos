//! On a Pi 5: a sensor captured through Lemnos's receiver module (`/dev/lemnos-rp1-csi2`,
//! feature `csi2-module`), the sensor set up over i2c-dev from a register file.
//!
//! ```text
//! csi2_module_check REGFILE [options]
//!   --bus PATH        i2c-dev bus (/dev/i2c-10)
//!   --addr N          7-bit sensor address (0x60)
//!   --power LINE      a GPIO line, by name, held high for the run (none)
//!   --id ADDR:LEN=V[,V...]  identification register and accepted values (none)
//!   --lanes N --mbps N --dt N --width N --height N --stride N   the link and frame
//!                     (2, 800, 0x2b, 1280, 800, 1600)
//!   --frames N        frames to capture (300)
//!   --bars            check the frame is vertical bars: rows of a parity identical, columns
//!                     varying
//!   --dump PATH       write the checked frame's bytes to PATH
//!   --be              (feature `be-module` too) unpack to 16 bits in the receiver and hand each
//!                     frame's buffer to the back end module, which demosaics it to RGB888: the
//!                     frame end to back end interrupt and wake-up times, and a bar check of
//!                     the RGB output
//! ```
//!
//! The register file's sections run in file order before streaming, except `stream_on`, run
//! after the receiver starts, and `stream_off`, run before it stops. The sensor's kernel driver
//! must be unbound (the address is claimed, never forced), and the receiver bound to the module.
//! Prints the frame rate, frame start to frame end, drops and the bar check.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::fs::File;
use std::os::fd::{AsFd, OwnedFd};
use std::time::Duration;

use lemnos_drivers_pisp::device::cci::{Cci, I2cCci, RegisterFile};
use lemnos_drivers_pisp::device::csi2_module::{
    Csi2Module, DEPTH, Done, Receiver, Start, UNPACK16,
};
use lemnos_linux::dma::{Access, DmaBuf, DmaHeap};
use lemnos_linux_sys::gpio;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Args {
    regfile: String,
    bus: String,
    addr: u16,
    power: Option<String>,
    id: Option<(u16, usize, Vec<u32>)>,
    start: Start,
    frames: u32,
    bars: bool,
    dump: Option<String>,
    be: bool,
}

fn num(s: &str) -> Result<u32> {
    Ok(match s.strip_prefix("0x") {
        Some(h) => u32::from_str_radix(h, 16)?,
        None => s.parse()?,
    })
}

fn args() -> Result<Args> {
    let mut it = std::env::args().skip(1);
    let mut a = Args {
        regfile: String::new(),
        bus: "/dev/i2c-10".into(),
        addr: 0x60,
        power: None,
        id: None,
        start: Start {
            lanes: 2,
            mbps: 800,
            dt: 0x2b,
            width: 1280,
            height: 800,
            stride: 1600,
            ..Start::default()
        },
        frames: 300,
        bars: false,
        dump: None,
        be: false,
    };
    while let Some(arg) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--bus" => a.bus = val()?,
            "--addr" => a.addr = num(&val()?)? as u16,
            "--power" => a.power = Some(val()?),
            "--id" => {
                let v = val()?;
                let (reg, vals) = v.split_once('=').ok_or("--id ADDR:LEN=V[,V]")?;
                let (addr, len) = reg.split_once(':').unwrap_or((reg, "1"));
                let vals = vals.split(',').map(num).collect::<Result<_>>()?;
                a.id = Some((num(addr)? as u16, num(len)? as usize, vals));
            }
            "--lanes" => a.start.lanes = num(&val()?)?,
            "--mbps" => a.start.mbps = num(&val()?)?,
            "--dt" => a.start.dt = num(&val()?)?,
            "--width" => a.start.width = num(&val()?)?,
            "--height" => a.start.height = num(&val()?)?,
            "--stride" => a.start.stride = num(&val()?)?,
            "--frames" => a.frames = num(&val()?)?,
            "--bars" => a.bars = true,
            "--dump" => a.dump = Some(val()?),
            "--be" => a.be = true,
            _ if a.regfile.is_empty() && !arg.starts_with('-') => a.regfile = arg,
            _ => return Err(format!("unknown argument {arg}").into()),
        }
    }
    if a.regfile.is_empty() {
        return Err("usage: csi2_module_check REGFILE [options]".into());
    }
    if a.be {
        a.start.flags |= UNPACK16;
        a.start.stride = a.start.stride.max(a.start.width * 2).next_multiple_of(64);
    }
    a.start.size = (a.start.stride * a.start.height).next_multiple_of(16);
    Ok(a)
}

/// Requests the GPIO line named `name` (on any chip) as an output driven high; the line stays
/// high while the returned descriptor is open.
fn power_on(name: &str) -> Result<OwnedFd> {
    for entry in std::fs::read_dir("/dev")? {
        let path = entry?.path();
        if !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("gpiochip"))
        {
            continue;
        }
        let chip = File::open(&path)?;
        let info = gpio::chip_info(chip.as_fd())?;
        for offset in 0..info.lines {
            let line = gpio::line_info(chip.as_fd(), offset)?;
            let len = line
                .name
                .iter()
                .position(|b| *b == 0)
                .unwrap_or(line.name.len());
            if &line.name[..len] != name.as_bytes() {
                continue;
            }
            let mut req = gpio::LineRequest::default();
            req.offsets[0] = offset;
            req.num_lines = 1;
            req.consumer[..12].copy_from_slice(b"csi2-capture");
            req.config.flags = gpio::flag::OUTPUT;
            req.config.num_attrs = 1;
            req.config.attrs[0].attr.id = gpio::attr::OUTPUT_VALUES;
            req.config.attrs[0].attr.value = 1;
            req.config.attrs[0].mask = 1;
            let fd = gpio::request_lines(chip.as_fd(), &req)?;
            println!("power: {name} is {}:{offset}, driven high", path.display());
            return Ok(fd);
        }
    }
    Err(format!("no GPIO line named {name}").into())
}

/// The share of the sampled rows' bytes that differ from row 0 or 1 (the row of the same
/// parity: a Bayer sensor's bars alternate between two rows), and the distinct values in row 0.
fn bars(frame: &[u8], start: &Start) -> (f64, usize) {
    let (stride, h) = (start.stride as usize, start.height as usize);
    let line = (start.width as usize * 10).div_ceil(8).min(stride);
    let row0 = &frame[..line];
    let rows = [h / 4, h / 2, 3 * h / 4, h - 1];
    let differ: usize = rows
        .iter()
        .map(|r| {
            let row = &frame[r * stride..r * stride + line];
            let first = &frame[(r % 2) * stride..(r % 2) * stride + line];
            row.iter().zip(first).filter(|(a, b)| a != b).count()
        })
        .sum();
    let mut seen = [false; 256];
    row0.iter().for_each(|b| seen[usize::from(*b)] = true);
    (
        differ as f64 / (rows.len() * line) as f64,
        seen.iter().filter(|s| **s).count(),
    )
}

fn main() -> Result<()> {
    let a = args()?;
    let regs = RegisterFile::load(&a.regfile)?;
    let _power = a.power.as_deref().map(power_on).transpose()?;
    std::thread::sleep(Duration::from_millis(20));
    let mut cci = I2cCci::open(&a.bus, a.addr)?;
    if let Some((reg, len, ok)) = &a.id {
        let id = cci.read_be(*reg, *len)?;
        println!("id: {reg:#06x} = {id:#x}");
        if !ok.contains(&id) {
            return Err(format!("unexpected id {id:#x}").into());
        }
    }
    for name in regs.names() {
        if name != "stream_on" && name != "stream_off" {
            let ops = regs.section(name).unwrap_or_default();
            cci.apply(&ops)?;
            println!("registers: [{name}] {} items", ops.len());
        }
    }

    let mut rx = Csi2Module::open()?;
    let info = rx.info()?;
    println!(
        "module: abi {} host version {:#x} max lanes {} depth {}",
        info.abi, info.host_version, info.max_lanes, info.depth
    );
    let heap = DmaHeap::open("linux,cma")?;
    let bufs: Vec<DmaBuf> = (0..DEPTH)
        .map(|_| heap.allocate(a.start.size as usize))
        .collect::<std::io::Result<_>>()?;
    #[cfg(feature = "be-module")]
    let mut be = a.be.then(|| be::Stage::new(&heap, &a.start)).transpose()?;
    #[cfg(not(feature = "be-module"))]
    if a.be {
        return Err("--be needs the be-module feature".into());
    }
    rx.start(&a.start)?;
    for (i, b) in bufs.iter().enumerate() {
        rx.queue(b.as_fd(), 0, i as u64)?;
    }
    cci.apply(&regs.section("stream_on").unwrap_or_default())?;

    let mut got: Vec<Done> = Vec::with_capacity(a.frames as usize);
    let mut done = [Done::default(); DEPTH];
    let mut checked = None;
    let mut failed = 0;
    while got.len() < a.frames as usize {
        let n = rx.wait_done(&mut done, Duration::from_secs(1))?;
        if n == 0 {
            eprintln!("no frame within 1 s after {} frames", got.len());
            break;
        }
        for d in &done[..n] {
            let buf = &bufs[d.tag as usize];
            if d.status != 0 {
                failed += 1;
            } else if (a.bars || a.dump.is_some()) && checked.is_none() && got.len() >= 5 {
                buf.begin_cpu_access(Access::Read)?;
                let map = buf.map_read_only()?;
                checked = Some(bars(map.as_slice(), &a.start));
                if let Some(path) = &a.dump {
                    std::fs::write(path, &map.as_slice()[..a.start.size as usize])?;
                }
                buf.end_cpu_access(Access::Read)?;
            }
            #[cfg(feature = "be-module")]
            if let Some(be) = be.as_mut().filter(|_| d.status == 0) {
                be.run(buf, d, got.len() == 10)?;
            }
            got.push(*d);
            rx.queue(buf.as_fd(), 0, d.tag)?;
        }
    }
    cci.apply(&regs.section("stream_off").unwrap_or_default())?;
    let info = rx.info()?;
    rx.stop()?;

    let ok: Vec<&Done> = got.iter().filter(|d| d.status == 0).collect();
    if ok.len() >= 2 {
        let span = (ok[ok.len() - 1].fs_ns - ok[0].fs_ns) as f64 / 1e9;
        let seq = ok[ok.len() - 1].sequence - ok[0].sequence;
        let mut readout: Vec<u64> = ok
            .iter()
            .filter(|d| d.fe_ns > d.fs_ns)
            .map(|d| d.fe_ns - d.fs_ns)
            .collect();
        readout.sort_unstable();
        let mid = readout.get(readout.len() / 2).copied().unwrap_or(0);
        println!(
            "frames: {} done ({} failed), {:.2} fps over {} frame starts, FS to FE median {} us",
            got.len(),
            failed,
            f64::from(seq) / span,
            seq,
            mid / 1000
        );
    }
    println!(
        "receiver: {} frame starts, {} without a buffer",
        info.frames, info.dropped
    );
    #[cfg(feature = "be-module")]
    if let Some(be) = &be {
        be.report();
    }
    if let Some((differ, distinct)) = checked.filter(|_| a.bars) {
        println!(
            "bars: {:.2} % of sampled row bytes differ from row 0 or 1, {distinct} distinct values in row 0",
            differ * 100.0
        );
    }
    Ok(())
}

/// The back end module behind the receiver (`--be`): each frame's buffer, unpacked to 16 bits,
/// is the input of a demosaic job, run one frame at a time.
#[cfg(feature = "be-module")]
mod be {
    use super::Result;
    use lemnos_drivers_pisp::be::ops::demosaic_ccm;
    use lemnos_drivers_pisp::device::be_module::{BeModule, BeModuleJob};
    use lemnos_drivers_pisp::device::csi2_module::{Done, Start};
    use lemnos_drivers_pisp::uapi::BayerOrder;
    use lemnos_linux::dma::{Access, DmaBuf, DmaHeap};
    use lemnos_linux_sys::time::monotonic_now;
    use std::os::fd::AsFd;
    use std::time::Duration;

    pub struct Stage {
        be: BeModule,
        job: BeModuleJob,
        out: DmaBuf,
        size: (usize, usize),
        /// Per frame: frame end to the back end's interrupt, and to this thread's wake-up, ns.
        times: Vec<(u64, u64)>,
        failed: u32,
        checked: Option<(usize, usize)>,
    }

    impl Stage {
        pub fn new(heap: &DmaHeap, start: &Start) -> Result<Self> {
            let size = (start.width as u16, start.height as u16);
            let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
            let mut op = demosaic_ccm(BayerOrder::Bggr, size, identity, [0.0; 3])?;
            let job = BeModuleJob::new(&mut op)?;
            if job.input_len() > start.size as usize {
                return Err(format!(
                    "the back end reads {} bytes, the receiver writes {}",
                    job.input_len(),
                    start.size
                )
                .into());
            }
            let out_len = job.output_len(0).ok_or("the op writes no output 0")?;
            let be = BeModule::open()?;
            println!(
                "back end: demosaic {}x{} to RGB888, {out_len} bytes",
                size.0, size.1
            );
            Ok(Self {
                be,
                job,
                out: heap.allocate(out_len)?,
                size: (usize::from(size.0), usize::from(size.1)),
                times: Vec::new(),
                failed: 0,
                checked: None,
            })
        }

        /// Runs the job on the frame `d` the receiver wrote into `input`, and checks the bars
        /// in its output when `check`.
        pub fn run(&mut self, input: &DmaBuf, d: &Done, check: bool) -> Result<()> {
            self.be.submit(
                &self.job,
                d.tag,
                input.as_fd(),
                [Some(self.out.as_fd()), None],
            )?;
            let done = self.be.wait(Duration::from_secs(1))?;
            let woke = monotonic_now().as_nanos() as u64;
            let Some(job) = done.first() else {
                return Err("the back end did not finish within 1 s".into());
            };
            if job.status != 0 {
                self.failed += 1;
                return Ok(());
            }
            self.times.push((
                job.irq_ns.saturating_sub(d.fe_ns),
                woke.saturating_sub(d.fe_ns),
            ));
            if check {
                self.out.begin_cpu_access(Access::Read)?;
                let map = self.out.map_read_only()?;
                self.checked = Some(rgb_bars(map.as_slice(), self.size));
                self.out.end_cpu_access(Access::Read)?;
            }
            Ok(())
        }

        pub fn report(&self) {
            let mut irq: Vec<u64> = self.times.iter().map(|t| t.0).collect();
            let mut wake: Vec<u64> = self.times.iter().map(|t| t.1).collect();
            irq.sort_unstable();
            wake.sort_unstable();
            let pct = |v: &[u64], p: usize| {
                v.get((v.len() * p / 100).min(v.len().saturating_sub(1)))
                    .copied()
                    .unwrap_or(0)
                    / 1000
            };
            println!(
                "back end: {} jobs ({} failed); frame end to back end interrupt p50 {} p99 {} max {} us; to wake-up p50 {} p99 {} max {} us",
                self.times.len(),
                self.failed,
                pct(&irq, 50),
                pct(&irq, 99),
                irq.last().copied().unwrap_or(0) / 1000,
                pct(&wake, 50),
                pct(&wake, 99),
                wake.last().copied().unwrap_or(0) / 1000,
            );
            if let Some((differ, colours)) = self.checked {
                println!(
                    "back end bars: {differ} of the sampled RGB rows' bytes differ from row {}, {colours} colour runs in it",
                    self.size.1 / 8
                );
            }
        }
    }

    /// Bytes of rows h/4, h/2 and 3h/4 that differ from row h/8 (bars are vertical, and rows
    /// away from the edges are whole after the demosaic), and the runs of one colour in row h/8.
    fn rgb_bars(rgb: &[u8], (w, h): (usize, usize)) -> (usize, usize) {
        let stride = rgb.len() / h;
        let row = |r: usize| &rgb[r * stride..r * stride + w * 3];
        let base = row(h / 8);
        let differ = [h / 4, h / 2, 3 * h / 4]
            .iter()
            .map(|r| row(*r).iter().zip(base).filter(|(a, b)| a != b).count())
            .sum();
        let runs = 1 + base
            .chunks(3)
            .zip(base.chunks(3).skip(1))
            .filter(|(a, b)| a != b)
            .count();
        (differ, runs)
    }
}
