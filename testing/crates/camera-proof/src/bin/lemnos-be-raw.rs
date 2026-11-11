//! `lemnos-be-raw`: the PiSP back end's round trip on the raw module node and on the stock
//! driver's video nodes, the same job on both.
//!
//! ```text
//! lemnos-be-raw module --input FILE [--jobs N] [--device NAME] [--save FILE] [--close-cycles N]
//! lemnos-be-raw stock --input FILE [--jobs N] [--save FILE]
//! ```
//!
//! `--input` is one packed RAW10 frame (1280x800, 1600 bytes a line), unpacked to 16-bit Bayer.
//! The job is the configuration library's Bayer-to-NV12 (`BackEnd::simple_bayer`), the one
//! `lemnos-isp-proof` posts through `lemnosd`. Input, output and (for the stock driver) the
//! configuration are dma-bufs from the CMA heap.
//!
//! `module` needs the back end bound to the Lemnos module, and runs the driver's own hardware
//! layer (`be_hw::LinuxHardware`) with no daemon and no client: per job, the time from posting it
//! (configuration written, job on the ring, kick) to its frame on the ring, found by polling the
//! ring without sleeping. `stock` needs `pispbe` bound: the input, config and output0 nodes,
//! dma-buf memory, and per job the time from queueing the last of the three buffers to
//! dequeueing the output, polled the same way. Both print the median, 99th percentile and
//! maximum in microseconds, and the output's hash and luma bars; `--save` writes the output.
//!
//! `module` then posts a job whose output address is 4 KiB past its buffer, which the module must
//! refuse at the kick (the stream ends), and runs 10 jobs on a new stream. With `--close-cycles
//! N`, N times: a stream, two jobs posted, and the stream closed at once with both on the block;
//! then 10 jobs on a new stream.

use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use lemnos_driver_pisp::be::BackEnd;
use lemnos_driver_pisp::be_hw::{Hardware, LinuxHardware, Plan, Settings};
use lemnos_driver_pisp::be_job::{self, JobMemory, KernelJob};
use lemnos_driver_pisp::format::{compute_stride_align, formats};
use lemnos_driver_pisp::uapi::{
    BayerOrder as Order, BeCcmConfig, BeTilesConfig, ImageFormatConfig, Wire,
};
use lemnos_driver_pisp_sys::CONFIG_BYTES;
use lemnos_linux_sys::dma_buf::{SYNC_END, SYNC_READ, SYNC_START, SYNC_WRITE, sync};
use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::v4l2::{
    self, BufType, Format, Memory, PixFormatMplane, PlaneFormat, PlaneMemory, QueuePlane,
};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;
const RAW10_STRIDE: usize = 1600;
/// RAW10's black level, moved to 16 bits.
const BLACK: u16 = 64 << 6;
/// Full-range BT.601, as `lemnos-isp-proof` uses.
const CSC: BeCcmConfig = BeCcmConfig {
    coeffs: [306, 601, 117, -173, -339, 512, 512, -429, -83],
    pad: [0; 2],
    offsets: [0, 33_554_432, 33_554_432],
};
const POLL_LIMIT: Duration = Duration::from_secs(2);

struct Args {
    mode: String,
    input: PathBuf,
    jobs: u32,
    device: String,
    save: Option<PathBuf>,
    close_cycles: u32,
}

fn parse() -> Option<Args> {
    let mut it = std::env::args().skip(1);
    let mut args = Args {
        mode: it.next()?,
        input: PathBuf::new(),
        jobs: 1000,
        device: "1000880000.pisp_be".into(),
        save: None,
        close_cycles: 0,
    };
    while let Some(flag) = it.next() {
        let value = it.next()?;
        match flag.as_str() {
            "--input" => args.input = PathBuf::from(value),
            "--jobs" => args.jobs = value.parse().ok()?,
            "--device" => args.device = value,
            "--save" => args.save = Some(PathBuf::from(value)),
            "--close-cycles" => args.close_cycles = value.parse().ok()?,
            _ => return None,
        }
    }
    (["module", "stock"].contains(&args.mode.as_str()) && !args.input.as_os_str().is_empty())
        .then_some(args)
}

fn out(text: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stdout(), "{text}");
}

/// Packed RAW10 to 16-bit little-endian Bayer, each value moved to the top of the 16 bits.
fn unpack(raw: &[u8], stride: usize) -> Vec<u8> {
    let mut image = vec![0u8; stride * HEIGHT as usize];
    for (row, line) in raw.chunks(RAW10_STRIDE).take(HEIGHT as usize).enumerate() {
        let Some(to) = image.get_mut(row * stride..row * stride + WIDTH as usize * 2) else {
            break;
        };
        let (groups, _) = line.as_chunks::<5>();
        for (g, [a, b, c, d, low]) in groups.iter().enumerate() {
            for (i, high) in [a, b, c, d].into_iter().enumerate() {
                let value = (u16::from(*high) << 2 | u16::from(low >> (2 * i) & 3)) << 6;
                let at = (g * 4 + i) * 2;
                if let Some(pair) = to.get_mut(at..at + 2) {
                    pair.copy_from_slice(&value.to_le_bytes());
                }
            }
        }
    }
    image
}

/// The job: 1280x800 16-bit Bayer (BGGR, the sensor's) to NV12, from the library.
fn job_config() -> Result<(ImageFormatConfig, BeTilesConfig), String> {
    let mut input = ImageFormatConfig {
        width: WIDTH as u16,
        height: HEIGHT as u16,
        format: formats::BAYER16,
        ..Default::default()
    };
    compute_stride_align(&mut input, 64);
    let config = BackEnd::simple_bayer(
        input,
        Order::Bggr,
        BLACK,
        (1.6, 1.0, 1.6),
        None,
        formats::NV12,
        Some(CSC),
    )
    .prepare()
    .map_err(|e| format!("the configuration library refused: {e:?}"))?;
    Ok((input, config))
}

struct Buffer {
    fd: OwnedFd,
    map: Mapping,
}

fn allocate(len: usize) -> Result<Buffer, String> {
    let heap = std::fs::File::open("/dev/dma_heap/linux,cma")
        .map_err(|e| format!("/dev/dma_heap/linux,cma: {e}"))?;
    let fd = lemnos_linux_sys::dma_heap::allocate(heap.as_fd(), len as u64)
        .map_err(|e| format!("allocate {len}: {e}"))?;
    let map = Mapping::new(fd.as_fd(), len, 0, true).map_err(|e| format!("map: {e}"))?;
    Ok(Buffer { fd, map })
}

impl Buffer {
    fn fill(&mut self, bytes: &[u8]) -> Result<(), String> {
        sync(self.fd.as_fd(), SYNC_START | SYNC_WRITE).map_err(|e| format!("sync: {e}"))?;
        let to = self.map.as_mut_slice();
        to.fill(0);
        if let Some(dst) = to.get_mut(..bytes.len()) {
            dst.copy_from_slice(bytes);
        }
        sync(self.fd.as_fd(), SYNC_END | SYNC_WRITE).map_err(|e| format!("sync: {e}"))
    }

    fn read(&self, len: usize) -> Result<Vec<u8>, String> {
        sync(self.fd.as_fd(), SYNC_START | SYNC_READ).map_err(|e| format!("sync: {e}"))?;
        let bytes = self.map.as_slice().get(..len).unwrap_or(&[]).to_vec();
        sync(self.fd.as_fd(), SYNC_END | SYNC_READ).map_err(|e| format!("sync: {e}"))?;
        Ok(bytes)
    }
}

fn percentile(sorted: &[u64], p: usize) -> u64 {
    sorted
        .get((sorted.len() * p / 100).min(sorted.len().saturating_sub(1)))
        .copied()
        .unwrap_or(0)
}

fn report(name: &str, mut ns: Vec<u64>) {
    ns.sort_unstable();
    out(format_args!(
        "{name}: {} jobs, median {} us, p99 {} us, max {} us",
        ns.len(),
        percentile(&ns, 50) / 1000,
        percentile(&ns, 99) / 1000,
        ns.last().copied().unwrap_or(0) / 1000
    ));
}

/// The output's hash, and the luma of the middle of each eighth of the middle line.
fn describe(nv12: &[u8], y_stride: usize, save: Option<&Path>) {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &b in nv12 {
        hash = (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    let middle = nv12.get(HEIGHT as usize / 2 * y_stride..).unwrap_or(&[]);
    let eighth = WIDTH as usize / 8;
    let bars: Vec<u8> = (0..8)
        .map(|b| middle.get(b * eighth + eighth / 2).copied().unwrap_or(0))
        .collect();
    out(format_args!(
        "output: {} bytes, hash {hash:016x}, luma of each eighth of the middle line {bars:?}",
        nv12.len()
    ));
    if let Some(path) = save {
        match std::fs::write(path, nv12) {
            Ok(()) => out(format_args!("saved {}", path.display())),
            Err(e) => out(format_args!("{}: {e}", path.display())),
        }
    }
}

fn module(args: &Args, image: &[u8], config: &BeTilesConfig) -> Result<(), String> {
    let y_stride = config.config.output_format[0].image.stride as usize;
    let out_bytes =
        be_job::image_bytes(&config.config.output_format[0].image).ok_or("output size")? as usize;
    be_job::check(config).map_err(|e| format!("the driver's check refused: {e:?}"))?;
    let mut bytes = [0u8; CONFIG_BYTES];
    config
        .write_to(&mut bytes)
        .map_err(|_| "configuration size".to_string())?;

    let mut hw = LinuxHardware::new(
        Settings::new(&args.device, "linux,cma").map_err(|s| format!("settings: {s:?}"))?,
    );
    let plan = Plan {
        buffers: 2,
        job_slots: 8,
        tdn_bytes: None,
    };
    hw.start(&plan).map_err(|s| format!("start: {s:?}"))?;
    out(format_args!("module: hw_version {:#010x}", hw.hw_version()));
    let mut input = allocate(image.len())?;
    input.fill(image)?;
    let mut output = allocate(out_bytes)?;
    output.fill(&[])?;
    let inp = hw
        .register(input.fd.as_fd())
        .map_err(|s| format!("register input: {s:?}"))?;
    let outp = hw
        .register(output.fd.as_fd())
        .map_err(|s| format!("register output: {s:?}"))?;
    let addr = job_addresses(config, inp.addr, outp.addr)?;
    let mut times = Vec::with_capacity(args.jobs as usize);
    for identity in 1..=u64::from(args.jobs) {
        let job = KernelJob {
            identity,
            buffer: 0,
            config: 0,
            addr,
        };
        let t = Instant::now();
        hw.post(&job, &bytes).map_err(|s| format!("post: {s:?}"))?;
        let done = loop {
            if let Some(done) = hw.next_finished().map_err(|s| format!("finished: {s:?}"))? {
                break done;
            }
            if t.elapsed() > POLL_LIMIT {
                return Err(format!("job {identity}: nothing finished in 2 s"));
            }
            std::hint::spin_loop();
        };
        times.push(u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX));
        if done.identity != identity || done.bytes == 0 {
            return Err(format!("job {identity}: finished as {done:?}"));
        }
        hw.give_back(done.buffer)
            .map_err(|s| format!("give back: {s:?}"))?;
    }
    report("module: post to frame on the ring", times);
    hw.check().map_err(|s| format!("stream: {s:?}"))?;
    describe(&output.read(out_bytes)?, y_stride, args.save.as_deref());

    // An output address 4 KiB past its buffer: refused at the kick, and the stream is over.
    let mut past = addr;
    let slot = past
        .iter()
        .position(|&a| a == outp.addr)
        .ok_or("no output address in the job")?;
    past[slot] = outp.addr + outp.size.max(4096);
    let refused = hw.post(
        &KernelJob {
            identity: 1 << 40,
            buffer: 0,
            config: 0,
            addr: past,
        },
        &bytes,
    );
    let state = hw.check();
    out(format_args!(
        "a job whose output is past its buffer: post {refused:?}, the stream after it {state:?}"
    ));
    hw.stop();
    let again = ten(&mut hw, &plan, &input, &output, config, &bytes)?;
    out(format_args!(
        "a new stream after it: {again} of 10 jobs finished"
    ));

    for _ in 0..args.close_cycles {
        hw.start(&plan).map_err(|s| format!("start: {s:?}"))?;
        let i = hw
            .register(input.fd.as_fd())
            .map_err(|s| format!("register input: {s:?}"))?;
        let o = hw
            .register(output.fd.as_fd())
            .map_err(|s| format!("register output: {s:?}"))?;
        let addr = job_addresses(config, i.addr, o.addr)?;
        for buffer in 0..2 {
            hw.post(
                &KernelJob {
                    identity: u64::from(buffer) + 1,
                    buffer,
                    config: 0,
                    addr,
                },
                &bytes,
            )
            .map_err(|s| format!("post: {s:?}"))?;
        }
        // Closed with both on the block.
        hw.stop();
    }
    if args.close_cycles > 0 {
        let after = ten(&mut hw, &plan, &input, &output, config, &bytes)?;
        out(format_args!(
            "{} streams closed with two jobs on the block; then a new stream: {after} of 10 jobs finished",
            args.close_cycles
        ));
    }
    Ok(())
}

fn job_addresses(
    config: &BeTilesConfig,
    input: u64,
    output: u64,
) -> Result<[u64; lemnos_driver_pisp_sys::JOB_ADDRS], String> {
    be_job::addresses(
        &config.config,
        &JobMemory {
            input,
            output: [Some(output), None],
            ..JobMemory::default()
        },
    )
    .map_err(|e| format!("addresses: {e:?}"))
}

/// A new stream, 10 jobs one at a time, and the stream closed: how many finished.
fn ten(
    hw: &mut LinuxHardware,
    plan: &Plan,
    input: &Buffer,
    output: &Buffer,
    config: &BeTilesConfig,
    bytes: &[u8; CONFIG_BYTES],
) -> Result<u32, String> {
    hw.start(plan).map_err(|s| format!("start: {s:?}"))?;
    let i = hw
        .register(input.fd.as_fd())
        .map_err(|s| format!("register input: {s:?}"))?;
    let o = hw
        .register(output.fd.as_fd())
        .map_err(|s| format!("register output: {s:?}"))?;
    let addr = job_addresses(config, i.addr, o.addr)?;
    let mut finished = 0;
    for identity in 1..=10 {
        hw.post(
            &KernelJob {
                identity,
                buffer: 0,
                config: 0,
                addr,
            },
            bytes,
        )
        .map_err(|s| format!("post: {s:?}"))?;
        let t = Instant::now();
        while t.elapsed() < POLL_LIMIT {
            if let Some(done) = hw.next_finished().map_err(|s| format!("finished: {s:?}"))? {
                finished += u32::from(done.identity == identity && done.bytes != 0);
                hw.give_back(done.buffer)
                    .map_err(|s| format!("give back: {s:?}"))?;
                break;
            }
        }
    }
    hw.stop();
    Ok(finished)
}

/// The pispbe video node named `name`.
/// The stock driver's node of that name in its first node group: the driver registers two
/// groups of nodes with the same names, so the lowest-numbered match is taken for every node
/// (directory order would mix the groups, and a job then never forms).
fn node(name: &str) -> Result<std::fs::File, String> {
    let dir = std::fs::read_dir("/sys/class/video4linux").map_err(|e| format!("{e}"))?;
    let mut found: Vec<(u32, std::ffi::OsString)> = dir
        .flatten()
        .filter(|entry| {
            std::fs::read_to_string(entry.path().join("name"))
                .unwrap_or_default()
                .trim()
                == name
        })
        .filter_map(|entry| {
            let file = entry.file_name();
            let index = file.to_str()?.strip_prefix("video")?.parse().ok()?;
            Some((index, file))
        })
        .collect();
    found.sort();
    let (_, file) = found
        .into_iter()
        .next()
        .ok_or_else(|| format!("no video node named {name}"))?;
    let path = Path::new("/dev").join(file);
    v4l2::open_node(&path).map_err(|e| format!("{}: {e}", path.display()))
}

fn plane(b: &Buffer, len: usize, used: usize) -> QueuePlane<'_> {
    QueuePlane {
        memory: PlaneMemory::Dmabuf(b.fd.as_fd()),
        length: u32::try_from(len).unwrap_or(u32::MAX),
        bytes_used: u32::try_from(used).unwrap_or(u32::MAX),
        data_offset: 0,
    }
}

fn single_plane(width: u32, height: u32, fourcc: u32, stride: u32, size: u32) -> Format {
    let mut planes = [PlaneFormat::default(); v4l2::MAX_PLANES];
    planes[0] = PlaneFormat {
        size_image: size,
        bytes_per_line: stride,
    };
    Format::Multi(PixFormatMplane {
        width,
        height,
        pixelformat: fourcc,
        field: 1,
        colorspace: 0,
        planes,
        num_planes: 1,
    })
}

#[allow(clippy::too_many_lines)]
fn stock(args: &Args, image: &[u8], in_stride: u32, config: &BeTilesConfig) -> Result<(), String> {
    let y_stride = u32::try_from(config.config.output_format[0].image.stride)
        .map_err(|_| "output stride".to_string())?;
    let out_bytes =
        be_job::image_bytes(&config.config.output_format[0].image).ok_or("output size")? as usize;
    let input_node = node("pispbe-input")?;
    let config_node = node("pispbe-config")?;
    let output_node = node("pispbe-output0")?;
    let set = v4l2::s_fmt(
        input_node.as_fd(),
        BufType::VideoOutputMplane,
        &single_plane(
            WIDTH,
            HEIGHT,
            v4l2::fourcc(b"BYR2"),
            in_stride,
            image.len() as u32,
        ),
    )
    .map_err(|e| format!("input format: {e}"))?;
    out(format_args!("stock input format: {set:?}"));
    let set = v4l2::s_fmt(
        output_node.as_fd(),
        BufType::VideoCaptureMplane,
        &single_plane(
            WIDTH,
            HEIGHT,
            v4l2::fourcc(b"NV12"),
            y_stride,
            out_bytes as u32,
        ),
    )
    .map_err(|e| format!("output format: {e}"))?;
    out(format_args!("stock output format: {set:?}"));

    let mut bytes = vec![0u8; BeTilesConfig::SIZE];
    config
        .write_to(&mut bytes)
        .map_err(|_| "configuration size".to_string())?;
    let mut cfg = allocate(bytes.len().next_multiple_of(4096))?;
    cfg.fill(&bytes)?;
    let mut input = allocate(image.len())?;
    input.fill(image)?;
    let mut output = allocate(out_bytes)?;
    output.fill(&[])?;

    let queues = [
        (&config_node, BufType::MetaOutput),
        (&input_node, BufType::VideoOutputMplane),
        (&output_node, BufType::VideoCaptureMplane),
    ];
    for (n, t) in queues {
        v4l2::reqbufs(n.as_fd(), t, Memory::DmaBuf, 1)
            .map_err(|e| format!("reqbufs {t:?}: {e}"))?;
    }
    for (n, t) in queues {
        v4l2::streamon(n.as_fd(), t).map_err(|e| format!("streamon {t:?}: {e}"))?;
    }
    let mut times = Vec::with_capacity(args.jobs as usize);
    let result = (|| -> Result<(), String> {
        for job in 0..args.jobs {
            v4l2::qbuf(
                config_node.as_fd(),
                BufType::MetaOutput,
                Memory::DmaBuf,
                0,
                &[plane(&cfg, cfg.map.len(), bytes.len())],
                Duration::ZERO,
            )
            .map_err(|e| format!("qbuf config: {e}"))?;
            v4l2::qbuf(
                input_node.as_fd(),
                BufType::VideoOutputMplane,
                Memory::DmaBuf,
                0,
                &[plane(&input, image.len(), image.len())],
                Duration::ZERO,
            )
            .map_err(|e| format!("qbuf input: {e}"))?;
            let t = Instant::now();
            v4l2::qbuf(
                output_node.as_fd(),
                BufType::VideoCaptureMplane,
                Memory::DmaBuf,
                0,
                &[plane(&output, out_bytes, 0)],
                Duration::ZERO,
            )
            .map_err(|e| format!("qbuf output: {e}"))?;
            let done = loop {
                if let Some(d) = v4l2::dqbuf(
                    output_node.as_fd(),
                    BufType::VideoCaptureMplane,
                    Memory::DmaBuf,
                    1,
                )
                .map_err(|e| format!("dqbuf output: {e}"))?
                {
                    break d;
                }
                if t.elapsed() > POLL_LIMIT {
                    return Err(format!("job {job}: no output in 2 s"));
                }
                std::hint::spin_loop();
            };
            times.push(u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX));
            if done.flags & v4l2::BUF_FLAG_ERROR != 0 {
                return Err(format!("job {job}: the output came back with an error"));
            }
            for (n, t, planes) in [
                (&config_node, BufType::MetaOutput, 1),
                (&input_node, BufType::VideoOutputMplane, 1),
            ] {
                let start = Instant::now();
                while v4l2::dqbuf(n.as_fd(), t, Memory::DmaBuf, planes)
                    .map_err(|e| format!("dqbuf {t:?}: {e}"))?
                    .is_none()
                {
                    if start.elapsed() > POLL_LIMIT {
                        return Err(format!("job {job}: {t:?} not given back"));
                    }
                }
            }
        }
        Ok(())
    })();
    for (n, t) in queues {
        let _ = v4l2::streamoff(n.as_fd(), t);
        let _ = v4l2::reqbufs(n.as_fd(), t, Memory::DmaBuf, 0);
    }
    result?;
    report("stock: last buffer queued to output dequeued", times);
    describe(
        &output.read(out_bytes)?,
        y_stride as usize,
        args.save.as_deref(),
    );
    Ok(())
}

fn run(args: &Args) -> Result<(), String> {
    let raw = std::fs::read(&args.input).map_err(|e| format!("{}: {e}", args.input.display()))?;
    let (input, config) = job_config()?;
    let image = unpack(&raw, input.stride as usize);
    out(format_args!(
        "job: {}x{} 16-bit Bayer at stride {} to NV12 at stride {}, {} tiles",
        WIDTH, HEIGHT, input.stride, config.config.output_format[0].image.stride, config.num_tiles
    ));
    match args.mode.as_str() {
        "module" => module(args, &image, &config),
        _ => stock(args, &image, input.stride as u32, &config),
    }
}

fn main() -> ExitCode {
    let Some(args) = parse() else {
        out(format_args!(
            "usage: lemnos-be-raw module|stock --input FILE [--jobs N] [--device NAME] [--save FILE]"
        ));
        return ExitCode::from(2);
    };
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            out(format_args!("lemnos-be-raw: {e}"));
            ExitCode::FAILURE
        }
    }
}
