//! `lemnos-isp-proof`: the PiSP back end on a real board, through `lemnosd`'s socket.
//!
//! ```text
//! lemnos-isp-proof --input FILE [--socket PATH] [--isp ID] [--sensor ID] [--jobs N] [--save DIR]
//!                  [--rate N --seconds S] [--checks 1]
//! ```
//!
//! `--input` is one packed RAW10 frame from the receiver (1280x800, 1600 bytes a line, as
//! `lemnos-camera-proof --save` writes it). The tool unpacks it to 16-bit Bayer, writes it into
//! the back end's `input` buffer 0 and a Bayer-to-NV12 configuration built with the configuration
//! library into `config` buffer 0, both through the client's own mappings, and posts jobs naming
//! them, one `output0` buffer each. It prints:
//!
//! - the first output's luma: the mean of the middle of each eighth of a line (the colour bars
//!   give eight distinct values), and how many lines equal the middle one;
//! - the time from posting a job to holding its frame, one job at a time (`--jobs`, default 1000);
//! - the same jobs again with two queued, as jobs per second;
//! - the back end's counters.
//!
//! With `--save`, the first output's luma as a PGM.
//!
//! With `--rate` and `--seconds`, jobs at that rate for that long with two queued: every job must
//! finish, in order, with no drop and no error.
//!
//! With `--checks 1` (the board file needs two `config` buffers and two `input` buffers), what the
//! driver must not run, each followed by a job that must: a configuration with a byte set in its
//! address fields (dropped, no frame); a configuration rewritten with zero gains right after its
//! job was posted (the frame is the first configuration's); a job naming the output buffer the
//! client holds (dropped, the held frame's bytes unchanged); two jobs naming the same input in one
//! go (one frame, one dropped).

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lemnos_client::{Client, FrameStream};
use lemnos_contract_camera_isp::BayerOrder;
use lemnos_contract_camera_isp::{CameraIsp, IspInput, IspJob, IspOutput, NO_BUFFER};
use lemnos_contract_camera_sensor::CameraSensor;
use lemnos_contract_isp_pisp::LAYOUT_BACK_END;
use lemnos_core::{Encode, Writer};
use lemnos_driver_pisp::be::BackEnd;
use lemnos_driver_pisp::format::{compute_stride_align, formats};
use lemnos_driver_pisp::uapi::{
    BayerOrder as Order, BeCcmConfig, BeTilesConfig, ImageFormatConfig, Wire,
};
use lemnos_linux_stream::LinuxStreams;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;
const RAW10_STRIDE: usize = 1600;
/// RAW10's black level, moved to 16 bits.
const BLACK: u16 = 64 << 6;
/// Full-range BT.601, as the module's own check uses.
const CSC: BeCcmConfig = BeCcmConfig {
    coeffs: [306, 601, 117, -173, -339, 512, 512, -429, -83],
    pad: [0; 2],
    offsets: [0, 33_554_432, 33_554_432],
};

struct Args {
    socket: String,
    isp: String,
    sensor: String,
    input: PathBuf,
    jobs: u32,
    save: Option<PathBuf>,
    rate: u32,
    seconds: u32,
    checks: bool,
}

fn parse() -> Option<Args> {
    let mut args = Args {
        socket: "/run/lemnos/lemnosd.sock".into(),
        isp: "cam0-be".into(),
        sensor: "cam0-sensor".into(),
        input: PathBuf::new(),
        jobs: 1000,
        save: None,
        rate: 0,
        seconds: 0,
        checks: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next()?;
        match flag.as_str() {
            "--socket" => args.socket = value,
            "--isp" => args.isp = value,
            "--sensor" => args.sensor = value,
            "--input" => args.input = PathBuf::from(value),
            "--jobs" => args.jobs = value.parse().ok()?,
            "--save" => args.save = Some(PathBuf::from(value)),
            "--rate" => args.rate = value.parse().ok()?,
            "--seconds" => args.seconds = value.parse().ok()?,
            "--checks" => args.checks = value != "0",
            _ => return None,
        }
    }
    (!args.input.as_os_str().is_empty()).then_some(args)
}

fn out(text: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stdout(), "{text}");
}

fn now_ns() -> u64 {
    lemnos_linux_sys::time::monotonic_now()
        .map(|t| u64::try_from(t.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn percentile(sorted: &[u64], p: usize) -> u64 {
    sorted
        .get((sorted.len() * p / 100).min(sorted.len().saturating_sub(1)))
        .copied()
        .unwrap_or(0)
}

/// Packed RAW10 (four pixels in five bytes, the low bits last) to 16-bit little-endian Bayer,
/// each value moved to the top of the 16 bits, on lines of `stride` bytes.
fn unpack(raw: &[u8], stride: usize) -> Vec<u8> {
    let mut image = vec![0u8; stride * HEIGHT as usize];
    for (row, line) in raw.chunks(RAW10_STRIDE).take(HEIGHT as usize).enumerate() {
        let Some(to) = image.get_mut(row * stride..row * stride + WIDTH as usize * 2) else {
            break;
        };
        let (groups, _) = line.as_chunks::<5>();
        let (pixels, _) = to.as_chunks_mut::<8>();
        for ([a, b, c, d, low], px) in groups.iter().zip(pixels) {
            let mut at = 0;
            for (i, high) in [a, b, c, d].into_iter().enumerate() {
                let value = (u16::from(*high) << 2 | u16::from(low >> (2 * i) & 3)) << 6;
                let [lo, hi] = value.to_le_bytes();
                if let Some(pair) = px.get_mut(at..at + 2) {
                    pair.copy_from_slice(&[lo, hi]);
                }
                at += 2;
            }
        }
    }
    image
}

fn order(bayer: BayerOrder) -> Order {
    match bayer {
        BayerOrder::Rggb => Order::Rggb,
        BayerOrder::Grbg => Order::Grbg,
        BayerOrder::Gbrg => Order::Gbrg,
        BayerOrder::Bggr => Order::Bggr,
        BayerOrder::Mono => Order::Greyscale,
    }
}

fn job(identity: u64, config: u32, input: u32, output: u32) -> ([u8; 128], usize) {
    let job = IspJob {
        identity,
        layout: LAYOUT_BACK_END,
        config_buffer: config,
        input_buffer: input,
        outputs: [output, NO_BUFFER, NO_BUFFER, NO_BUFFER],
    };
    let mut buf = [0u8; 128];
    let mut w = Writer::new(&mut buf);
    let n = job.encode(&mut w).map(|()| w.len()).unwrap_or(0);
    (buf, n)
}

/// Posts a job reading input buffer `input` and writing output buffer `output`. A job naming a
/// buffer another job still has on the block is dropped, so jobs in flight together use different
/// buffers of each stream.
fn post(config: &mut FrameStream, identity: u64, input: u32, output: u32) -> Result<(), String> {
    post_on(config, identity, 0, input, output)
}

/// As [`post`], with the configuration in config buffer `cfg_buffer`.
fn post_on(
    config: &mut FrameStream,
    identity: u64,
    cfg_buffer: u32,
    input: u32,
    output: u32,
) -> Result<(), String> {
    let (buf, n) = job(identity, cfg_buffer, input, output);
    match config.post_job(buf.get(..n).unwrap_or(&[])) {
        Ok(true) => Ok(()),
        Ok(false) => Err("the job queue is full".into()),
        Err(e) => Err(format!("post: {e:?}")),
    }
}

/// The luma of the first frame: each eighth's middle, and the lines equal to the middle one.
fn luma(y: &[u8], stride: usize) -> (Vec<u8>, usize) {
    let line = |row: usize| y.get(row * stride..row * stride + WIDTH as usize);
    let middle = line(HEIGHT as usize / 2).unwrap_or(&[]);
    let eighth = WIDTH as usize / 8;
    let bars = (0..8)
        .map(|b| {
            let part = middle
                .get(b * eighth + 40..b * eighth + eighth - 40)
                .unwrap_or(&[]);
            let sum: usize = part.iter().map(|&v| usize::from(v)).sum();
            u8::try_from(sum / part.len().max(1)).unwrap_or(u8::MAX)
        })
        .collect();
    let same = (0..HEIGHT as usize)
        .filter(|&row| {
            line(row).is_some_and(|l| l.iter().zip(middle).all(|(a, b)| a.abs_diff(*b) <= 2))
        })
        .count();
    (bars, same)
}

#[allow(clippy::too_many_lines)]
fn run(args: &Args) -> Result<bool, String> {
    let client = Client::connect(&args.socket, "lemnos-isp-proof")
        .map_err(|e| format!("{}: {e}", args.socket))?;
    client.set_timeout(Duration::from_secs(5));
    let list = client.list().map_err(|e| format!("list: {e}"))?;
    let index = |name: &str| {
        list.iter()
            .find(|d| d.name == name)
            .map(|d| d.index)
            .ok_or(format!("no device {name}"))
    };
    let isp_index = index(&args.isp)?;
    let mut sensor = client
        .open::<dyn CameraSensor>(&args.sensor)
        .map_err(|e| format!("{}: {e}", args.sensor))?;
    let bayer = sensor
        .mode(0)
        .map_err(|s| format!("sensor mode: {s:?}"))?
        .bayer;
    let mut isp = client
        .open::<dyn CameraIsp>(&args.isp)
        .map_err(|e| format!("{}: {e}", args.isp))?;

    let raw = std::fs::read(&args.input).map_err(|e| format!("{}: {e}", args.input.display()))?;
    let mut input = ImageFormatConfig {
        width: WIDTH as u16,
        height: HEIGHT as u16,
        format: formats::BAYER16,
        ..Default::default()
    };
    compute_stride_align(&mut input, 64);
    let input_stride = input.stride as usize;
    let image = unpack(&raw, input_stride);
    let config: BeTilesConfig = BackEnd::simple_bayer(
        input,
        order(bayer),
        BLACK,
        (1.6, 1.0, 1.6),
        None,
        formats::NV12,
        Some(CSC),
    )
    .prepare()
    .map_err(|e| format!("the configuration library refused: {e:?}"))?;
    let y_stride = config.config.output_format[0].image.stride as usize;
    let mut config_bytes = vec![0u8; BeTilesConfig::SIZE];
    config
        .write_to(&mut config_bytes)
        .map_err(|_| "configuration size".to_string())?;
    out(format_args!(
        "input: {} bytes of RAW10, {bayer:?}, unpacked to 16-bit at stride {input_stride}; configuration {} bytes, {} tiles, luma stride {y_stride}",
        raw.len(),
        config_bytes.len(),
        config.num_tiles
    ));

    let _ = isp.stop();
    isp.set_input(IspInput {
        width: WIDTH,
        height: HEIGHT,
        bit_depth: 16,
        bayer,
    })
    .map_err(|s| format!("set_input: {s:?}"))?;
    let layout = isp
        .set_output(
            0,
            IspOutput {
                width: WIDTH,
                height: HEIGHT,
                fourcc: u32::from_le_bytes(*b"NV12"),
                pad_bytes: 0,
            },
        )
        .map_err(|s| format!("set_output: {s:?}"))?;
    out(format_args!(
        "output 0: stride {} size {}",
        layout.stride, layout.size
    ));

    let platform = Arc::new(LinuxStreams);
    let mut cfg = client
        .open_stream_writable(isp_index, "config", platform.clone())
        .map_err(|e| format!("config stream: {e}"))?;
    let mut inp = client
        .open_stream_writable(isp_index, "input", platform.clone())
        .map_err(|e| format!("input stream: {e}"))?;
    let mut output = client
        .open_stream(isp_index, "output0", platform)
        .map_err(|e| format!("output0 stream: {e}"))?;
    let outputs = output.layout().buffers;
    isp.start().map_err(|s| format!("start: {s:?}"))?;
    cfg.write(0, 0, &config_bytes)
        .map_err(|e| format!("config write: {e:?}"))?;
    // The same picture in every input buffer, so queued jobs can each read their own.
    let inputs = inp.layout().buffers;
    for buffer in 0..inputs {
        inp.write(buffer, 0, &image)
            .map_err(|e| format!("input write: {e:?}"))?;
    }

    // The first frame: its content. A job is dropped, not run, when it names a buffer the driver
    // took before this client attached; the driver takes it again for this client, so the job is
    // posted again, at most three times, and the drops are reported.
    let mut first = None;
    for attempt in 0..3u64 {
        post(&mut cfg, 1 + attempt, 0, 0)?;
        first = output
            .next_frame(Duration::from_secs(2))
            .map_err(|e| format!("output0: {e:?}"))?;
        if first.is_some() {
            break;
        }
        out(format_args!(
            "first job, attempt {attempt}: no frame; back end counters {:?}",
            isp.counters()
        ));
    }
    let Some(first) = first else {
        return Err(format!("no frame after three jobs; info {:?}", isp.info()));
    };
    let next_identity = first.identity() + 1;
    let y = output.bytes(&first);
    let (bars, same) = luma(y, y_stride);
    let chroma: Vec<u8> = (0..8)
        .map(|b| {
            let at = y_stride * HEIGHT as usize + y_stride * (HEIGHT as usize / 4) + b * 160 + 80;
            y.get(at).copied().unwrap_or(0)
        })
        .collect();
    out(format_args!(
        "first frame: identity {} buffer {} bytes {}; luma by eighths {bars:?}; lines equal to the middle one (within 2): {same} of {HEIGHT}; Cb by eighths {chroma:?}",
        first.identity(),
        first.buffer(),
        first.len()
    ));
    if let Some(dir) = &args.save {
        let mut pgm = format!("P5\n{WIDTH} {HEIGHT}\n255\n").into_bytes();
        for row in 0..HEIGHT as usize {
            pgm.extend_from_slice(
                y.get(row * y_stride..row * y_stride + WIDTH as usize)
                    .unwrap_or(&[]),
            );
        }
        let path = dir.join("be-output0-luma.pgm");
        std::fs::write(&path, pgm).map_err(|e| format!("{}: {e}", path.display()))?;
        out(format_args!("saved {}", path.display()));
    }
    let distinct = {
        let mut seen = bars.clone();
        seen.sort_unstable();
        seen.dedup_by(|a, b| a.abs_diff(*b) < 4);
        seen.len()
    };
    output
        .release(first)
        .map_err(|e| format!("release: {e:?}"))?;

    // One job at a time: post to frame.
    let mut times = Vec::with_capacity(args.jobs as usize);
    let mut identity = next_identity;
    for n in 0..args.jobs {
        let posted = now_ns();
        post(&mut cfg, identity, 0, n % outputs)?;
        let frame = output
            .next_frame(Duration::from_secs(2))
            .map_err(|e| format!("output0: {e:?}"))?
            .ok_or(format!("no frame for 2 s after job {identity}"))?;
        times.push(now_ns().saturating_sub(posted));
        if frame.identity() != identity {
            return Err(format!(
                "frame identity {} for job {identity}",
                frame.identity()
            ));
        }
        output
            .release(frame)
            .map_err(|e| format!("release: {e:?}"))?;
        identity += 1;
    }
    times.sort_unstable();
    out(format_args!(
        "{} jobs one at a time, post to frame: median {} us, p99 {} us, max {} us",
        args.jobs,
        percentile(&times, 50) / 1000,
        percentile(&times, 99) / 1000,
        times.last().copied().unwrap_or(0) / 1000
    ));

    // Two queued.
    let started = Instant::now();
    let (mut posted, mut done, mut in_order) = (0u32, 0u32, true);
    let mut expect = identity;
    while posted < 2 {
        post(&mut cfg, identity, posted % inputs, posted % outputs)?;
        identity += 1;
        posted += 1;
    }
    while done < args.jobs {
        let Some(frame) = output
            .next_frame(Duration::from_secs(2))
            .map_err(|e| format!("output0: {e:?}"))?
        else {
            out(format_args!(
                "no frame for 2 s with {done} done of {posted} posted; counters {:?}",
                isp.counters()
            ));
            break;
        };
        in_order &= frame.identity() == expect;
        expect = frame.identity() + 1;
        output
            .release(frame)
            .map_err(|e| format!("release: {e:?}"))?;
        done += 1;
        if posted < args.jobs {
            // Round the buffers: each named was last used by a job that has finished.
            post(&mut cfg, identity, posted % inputs, posted % outputs)?;
            identity += 1;
            posted += 1;
        }
    }
    let secs = started.elapsed().as_secs_f64();
    out(format_args!(
        "{done} jobs with two queued in {:.3} s: {:.1} jobs/s; identities in order: {in_order}",
        secs,
        f64::from(done) / secs
    ));
    let counters = isp.counters().map_err(|s| format!("counters: {s:?}"))?;
    out(format_args!("back end counters: {counters:?}"));
    let mut ok = distinct >= 6 && same > HEIGHT as usize * 9 / 10 && in_order && done == args.jobs;

    if args.rate > 0 && args.seconds > 0 {
        let jobs = args.rate * args.seconds;
        let period = Duration::from_secs(1) / args.rate;
        let before = isp.counters().map_err(|s| format!("counters: {s:?}"))?;
        let started = Instant::now();
        let (mut posted, mut done, mut in_order) = (0u32, 0u32, true);
        let mut expect = identity;
        while posted < 2 {
            post(&mut cfg, identity, posted % inputs, posted % outputs)?;
            identity += 1;
            posted += 1;
        }
        while done < jobs {
            let Some(frame) = output
                .next_frame(Duration::from_secs(2))
                .map_err(|e| format!("output0: {e:?}"))?
            else {
                break;
            };
            in_order &= frame.identity() == expect;
            expect = frame.identity() + 1;
            output
                .release(frame)
                .map_err(|e| format!("release: {e:?}"))?;
            done += 1;
            if posted < jobs {
                // Paced: job n goes in no earlier than n periods after the start.
                let due = period * posted;
                if let Some(wait) = due.checked_sub(started.elapsed()) {
                    std::thread::sleep(wait);
                }
                post(&mut cfg, identity, posted % inputs, posted % outputs)?;
                identity += 1;
                posted += 1;
            }
        }
        let after = isp.counters().map_err(|s| format!("counters: {s:?}"))?;
        out(format_args!(
            "sustained: {done} of {jobs} jobs at {} jobs/s with two queued in {:.1} s; in order: {in_order}; dropped +{}, errors +{}",
            args.rate,
            started.elapsed().as_secs_f64(),
            after.dropped - before.dropped,
            after.errors - before.errors
        ));
        ok &= done == jobs
            && in_order
            && after.dropped == before.dropped
            && after.errors == before.errors;
    }

    if args.checks {
        ok &= checks(
            &mut isp,
            &mut cfg,
            &mut output,
            &config_bytes,
            &mut identity,
        )?;
    }
    isp.stop().map_err(|s| format!("stop: {s:?}"))?;
    Ok(ok)
}

fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// One job on config buffer `cfg_buffer`, input 0 and output `out`: the frame's hash, or `None`
/// when nothing came in `wait`.
fn one(
    cfg: &mut FrameStream,
    output: &mut FrameStream,
    identity: &mut u64,
    cfg_buffer: u32,
    out_buffer: u32,
    wait: Duration,
) -> Result<Option<u64>, String> {
    post_on(cfg, *identity, cfg_buffer, 0, out_buffer)?;
    *identity += 1;
    let Some(frame) = output
        .next_frame(wait)
        .map_err(|e| format!("output0: {e:?}"))?
    else {
        return Ok(None);
    };
    let h = hash(output.bytes(&frame));
    output
        .release(frame)
        .map_err(|e| format!("release: {e:?}"))?;
    Ok(Some(h))
}

/// D15 check 3 and D17 checks 3, 4 and 6: what the driver must not run.
#[allow(clippy::too_many_lines)]
fn checks(
    isp: &mut impl CameraIsp,
    cfg: &mut FrameStream,
    output: &mut FrameStream,
    good: &[u8],
    identity: &mut u64,
) -> Result<bool, String> {
    let wait = Duration::from_millis(500);
    let mut ok = true;
    let reference = one(cfg, output, identity, 0, 0, Duration::from_secs(2))?
        .ok_or("no frame for the reference job")?;

    // An address field: one byte set in the first 112 bytes, in config buffer 1.
    let mut addressed = good.to_vec();
    if let Some(b) = addressed.get_mut(8) {
        *b = 0x10;
    }
    cfg.write(1, 0, &addressed)
        .map_err(|e| format!("config write: {e:?}"))?;
    let before = isp.counters().map_err(|s| format!("counters: {s:?}"))?;
    let got = one(cfg, output, identity, 1, 0, wait)?;
    let after = isp.counters().map_err(|s| format!("counters: {s:?}"))?;
    let next = one(cfg, output, identity, 0, 0, Duration::from_secs(2))?;
    out(format_args!(
        "configuration naming an address: frame {got:?}, dropped +{}, errors +{}; the next job's frame equal to the reference: {}",
        after.dropped - before.dropped,
        after.errors - before.errors,
        next == Some(reference)
    ));
    ok &= got.is_none() && after.dropped == before.dropped + 1 && next == Some(reference);

    // Rewritten after its job completed (contract rule 3): the next job on that buffer runs the
    // rewritten configuration, and rewriting it back gives the reference again. The client never
    // writes a configuration while a job holds it (rule 1); the driver's copy at accept (rule 5)
    // is what the tests of each driver pin.
    let mut zeroed = good.to_vec();
    zeroed.iter_mut().skip(112).for_each(|b| *b = 0);
    cfg.write(1, 0, good)
        .map_err(|e| format!("config write: {e:?}"))?;
    let first = one(cfg, output, identity, 1, 0, Duration::from_secs(2))?;
    cfg.write(1, 0, &zeroed)
        .map_err(|e| format!("config write: {e:?}"))?;
    let zeroed_frame = one(cfg, output, identity, 1, 0, Duration::from_secs(2))?;
    cfg.write(1, 0, good)
        .map_err(|e| format!("config write: {e:?}"))?;
    let back = one(cfg, output, identity, 1, 0, Duration::from_secs(2))?;
    out(format_args!(
        "configuration rewritten after its job completed: first job equal to the reference: {}; zeroed one a frame: {}, different from the reference: {}; rewritten back equal to the reference: {}",
        first == Some(reference),
        zeroed_frame.is_some(),
        zeroed_frame.is_some() && zeroed_frame != Some(reference),
        back == Some(reference)
    ));
    ok &= first == Some(reference)
        && zeroed_frame.is_some_and(|h| h != reference)
        && back == Some(reference);
    cfg.write(1, 0, good)
        .map_err(|e| format!("config write: {e:?}"))?;

    // A job naming the output buffer the client holds.
    post_on(cfg, *identity, 0, 0, 1)?;
    *identity += 1;
    let held = output
        .next_frame(Duration::from_secs(2))
        .map_err(|e| format!("output0: {e:?}"))?
        .ok_or("no frame to hold")?;
    let held_hash = hash(output.bytes(&held));
    let before = isp.counters().map_err(|s| format!("counters: {s:?}"))?;
    let got = one(cfg, output, identity, 0, held.buffer(), wait)?;
    let after = isp.counters().map_err(|s| format!("counters: {s:?}"))?;
    let unchanged = hash(output.bytes(&held)) == held_hash;
    out(format_args!(
        "a job on the output buffer the client holds ({}): frame {got:?}, dropped +{}, the held frame unchanged: {unchanged}",
        held.buffer(),
        after.dropped - before.dropped
    ));
    ok &= got.is_none() && after.dropped == before.dropped + 1 && unchanged;
    output
        .release(held)
        .map_err(|e| format!("release: {e:?}"))?;

    // Two jobs naming the same input in one go.
    let before = isp.counters().map_err(|s| format!("counters: {s:?}"))?;
    post_on(cfg, *identity, 0, 0, 0)?;
    post_on(cfg, *identity + 1, 1, 0, 1)?;
    *identity += 2;
    let mut frames = 0;
    while let Some(f) = output
        .next_frame(wait)
        .map_err(|e| format!("output0: {e:?}"))?
    {
        frames += 1;
        output.release(f).map_err(|e| format!("release: {e:?}"))?;
    }
    let after = isp.counters().map_err(|s| format!("counters: {s:?}"))?;
    out(format_args!(
        "two jobs on one input in one go: {frames} frames, dropped +{}, errors +{}",
        after.dropped - before.dropped,
        after.errors - before.errors
    ));
    ok &= frames == 1 && after.dropped == before.dropped + 1 && after.errors == before.errors;
    Ok(ok)
}

fn main() -> ExitCode {
    let Some(args) = parse() else {
        out(format_args!(
            "usage: lemnos-isp-proof --input FILE [--socket PATH] [--isp ID] [--sensor ID] [--jobs N] [--save DIR] [--rate N --seconds S] [--checks 1]"
        ));
        return ExitCode::from(2);
    };
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            out(format_args!("lemnos-isp-proof: {e}"));
            ExitCode::FAILURE
        }
    }
}
