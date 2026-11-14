//! `lemnos-isp-proof`: the PiSP back end on a real board, through `lemnosd`'s socket.
//!
//! ```text
//! lemnos-isp-proof --input FILE [--socket PATH] [--isp ID] [--sensor ID] [--jobs N] [--save DIR]
//!                  [--rate N --seconds S] [--checks 1] [--sharpen 1] [--resample WxH]
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
//! With `--sharpen 1`, the default sharpening (the configuration library's `SHARPEN_DEFAULT`, all
//! five filters) is enabled as well: the same job is run without it and with it, the sharpened
//! luma's eighths and its mean absolute difference from the plain run are printed, and a zero
//! difference fails the run (the sharpen made no change).
//!
//! With `--resample WxH`, output 0 is WxH (NV12) instead of the input size, through the
//! configuration library's smart resize with the resampler filter for that downscale
//! (`ResampleFilter::for_downscale`). It prints the output's stride and size, checks the size is
//! stride times height times 1.5 and that the frame holds exactly that, and prints the luma eighths
//! of the resized frame. A size the library refuses is printed with the library's reason.
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

use lemnos_camera_proof::chain::{Stage, resolve_end};
use lemnos_client::{Client, FrameStream};
use lemnos_contract_camera_isp::BayerOrder;
use lemnos_contract_camera_isp::{CameraIsp, IspInput, IspJob, IspOutput, NO_BUFFER};
use lemnos_contract_camera_sensor::CameraSensor;
use lemnos_contract_isp_pisp::LAYOUT_BACK_END;
use lemnos_core::{Encode, Writer};
use lemnos_driver_pisp::be::{BackEnd, ResampleFilter, defaults::SHARPEN_DEFAULT};
use lemnos_driver_pisp::format::{compute_stride_align, formats};
use lemnos_driver_pisp::uapi::{
    BayerOrder as Order, BeCcmConfig, BeTilesConfig, Branch, ImageFormatConfig, WbgConfig, Wire,
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
    /// Not given: the ISP the sensor's chain ends at.
    isp: Option<String>,
    /// Not given: the board's one camera sensor.
    sensor: Option<String>,
    input: PathBuf,
    jobs: u32,
    save: Option<PathBuf>,
    rate: u32,
    seconds: u32,
    checks: bool,
    sharpen: bool,
    resample: Option<(u32, u32)>,
}

fn parse() -> Option<Args> {
    let mut args = Args {
        socket: "/run/lemnos/lemnosd.sock".into(),
        isp: None,
        sensor: None,
        input: PathBuf::new(),
        jobs: 1000,
        save: None,
        rate: 0,
        seconds: 0,
        checks: false,
        sharpen: false,
        resample: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next()?;
        match flag.as_str() {
            "--socket" => args.socket = value,
            "--isp" => args.isp = Some(value),
            "--sensor" => args.sensor = Some(value),
            "--input" => args.input = PathBuf::from(value),
            "--jobs" => args.jobs = value.parse().ok()?,
            "--save" => args.save = Some(PathBuf::from(value)),
            "--rate" => args.rate = value.parse().ok()?,
            "--seconds" => args.seconds = value.parse().ok()?,
            "--checks" => args.checks = value != "0",
            "--sharpen" => args.sharpen = value != "0",
            "--resample" => {
                let (w, h) = value.split_once('x')?;
                let size = (w.parse().ok()?, h.parse().ok()?);
                if size.0 == 0
                    || size.1 == 0
                    || size.0 > u32::from(u16::MAX)
                    || size.1 > u32::from(u16::MAX)
                {
                    return None;
                }
                args.resample = Some(size);
            }
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

/// The luma of a frame of `width` by `height`, `stride` bytes a line: each eighth's middle, and the
/// lines equal to the middle one.
fn luma(y: &[u8], stride: usize, width: usize, height: usize) -> (Vec<u8>, usize) {
    let line = |row: usize| y.get(row * stride..row * stride + width);
    let middle = line(height / 2).unwrap_or(&[]);
    if middle.len() != width {
        // The frame is shorter than its size: nothing to read, not a frame of zeros.
        return (vec![0; 8], 0);
    }
    let eighth = width / 8;
    // The middle half of each eighth, so the edges of a bar are left out at any width. (A fixed
    // 40-pixel margin, right for 1280 wide, left nothing of a 640-wide eighth: all bars read 0.)
    let margin = eighth / 4;
    let bars = (0..8)
        .map(|b| {
            let part = middle
                .get(b * eighth + margin..b * eighth + eighth - margin)
                .unwrap_or(&[]);
            let sum: usize = part.iter().map(|&v| usize::from(v)).sum();
            u8::try_from(sum / part.len().max(1)).unwrap_or(u8::MAX)
        })
        .collect();
    let same = (0..height)
        .filter(|&row| {
            line(row).is_some_and(|l| l.iter().zip(middle).all(|(a, b)| a.abs_diff(*b) <= 2))
        })
        .count();
    (bars, same)
}

/// The luma rows of a frame, packed: `height` rows of `width` bytes.
fn plane(y: &[u8], stride: usize, width: usize, height: usize) -> Vec<u8> {
    (0..height)
        .flat_map(|row| {
            y.get(row * stride..row * stride + width)
                .unwrap_or(&[])
                .iter()
                .copied()
        })
        .collect()
}

/// The mean of the absolute differences of two packed planes of the same size.
fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    let sum: u64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    sum as f64 / a.len().max(1) as f64
}

/// The back end's configuration for the input, with output 0 at `out` (NV12). Sharpening is off
/// unless asked for. The library's refusal is returned as it says it.
fn build(
    input: ImageFormatConfig,
    bayer: BayerOrder,
    out: (u32, u32),
    sharpen: bool,
) -> Result<BeTilesConfig, String> {
    let mut be = BackEnd::simple_bayer(
        input,
        order(bayer),
        BLACK,
        (1.6, 1.0, 1.6),
        None,
        formats::NV12,
        Some(CSC),
    );
    if out != (WIDTH, HEIGHT) {
        // Smart resize: the library's resampler, with its filter for this downscale.
        let (w, h) = (
            u16::try_from(out.0).unwrap_or(u16::MAX),
            u16::try_from(out.1).unwrap_or(u16::MAX),
        );
        be.set_smart_resize(Branch::Zero, w, h);
        be.set_resample_default(
            Branch::Zero,
            ResampleFilter::for_downscale(f64::from(WIDTH) / f64::from(out.0)),
        );
    }
    if sharpen {
        be.set_sharpen(Some(SHARPEN_DEFAULT))
            .map_err(|e| format!("the configuration library refused sharpen: {e}"))?;
    }
    be.prepare()
        .map_err(|e| format!("the configuration library refused: {e:?}"))
}

/// One job on config buffer 0, input 0 and output 0: the frame's luma plane, packed.
fn one_plane(
    cfg: &mut FrameStream,
    output: &mut FrameStream,
    identity: &mut u64,
    stride: usize,
    (width, height): (usize, usize),
) -> Result<Vec<u8>, String> {
    post(cfg, *identity, 0, 0)?;
    *identity += 1;
    let frame = output
        .next_frame(Duration::from_secs(2))
        .map_err(|e| format!("output0: {e:?}"))?
        .ok_or("no frame for the comparison job")?;
    let packed = plane(output.bytes(&frame), stride, width, height);
    output
        .release(frame)
        .map_err(|e| format!("release: {e:?}"))?;
    Ok(packed)
}

#[allow(clippy::too_many_lines)]
fn run(args: &Args) -> Result<bool, String> {
    let (out_w, out_h) = args.resample.unwrap_or((WIDTH, HEIGHT));
    let client = Client::connect(&args.socket, "lemnos-isp-proof")
        .map_err(|e| format!("{}: {e}", args.socket))?;
    client.set_timeout(Duration::from_secs(5));
    let list = client.list().map_err(|e| format!("list: {e}"))?;
    let names = resolve_end(
        &list,
        args.sensor.as_deref(),
        Stage {
            label: "ISP",
            flag: "--isp",
            name: args.isp.as_deref(),
        },
    )?;
    let sensor_name = &names.sensor;
    let isp_name = names.stages.last().ok_or("no ISP resolved")?;
    let index = |name: &str| {
        list.iter()
            .find(|d| d.name == name)
            .map(|d| d.index)
            .ok_or(format!("no device {name}"))
    };
    let isp_index = index(isp_name)?;
    let mut sensor = client
        .open::<dyn CameraSensor>(sensor_name)
        .map_err(|e| format!("{}: {e}", sensor_name))?;
    let bayer = sensor
        .mode(0)
        .map_err(|s| format!("sensor mode: {s:?}"))?
        .bayer;
    let mut isp = client
        .open::<dyn CameraIsp>(isp_name)
        .map_err(|e| format!("{}: {e}", isp_name))?;

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
    let config = build(input, bayer, (out_w, out_h), false)?;
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
                width: out_w,
                height: out_h,
                fourcc: u32::from_le_bytes(*b"NV12"),
                pad_bytes: 0,
            },
        )
        .map_err(|s| format!("set_output: {s:?}"))?;
    let nv12 = u64::from(layout.stride) * u64::from(out_h) * 3 / 2;
    out(format_args!(
        "output 0: {out_w}x{out_h} NV12, stride {} size {} (stride x height x 1.5 = {nv12})",
        layout.stride, layout.size
    ));
    // The size check is for a resized output; the default run's output is as it always was.
    let mut ok = args.resample.is_none()
        || (layout.stride as usize == y_stride && u64::from(layout.size) == nv12);
    if args.resample.is_some() {
        out(format_args!(
            "resampled output 0: stride {} size {}, the configuration's stride {y_stride}, size check {}",
            layout.stride,
            layout.size,
            if ok { "ok" } else { "FAILED" }
        ));
    }

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
    // Every config buffer holds the same configuration: jobs in flight together each name their
    // own, since the driver refuses a job naming a config buffer another job still holds.
    let configs = cfg.layout().buffers;
    for buffer in 0..configs {
        cfg.write(buffer, 0, &config_bytes)
            .map_err(|e| format!("config write: {e:?}"))?;
    }
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
    let (bars, same) = luma(y, y_stride, out_w as usize, out_h as usize);
    let chroma: Vec<u8> = (0..8)
        .map(|b| {
            let at = y_stride * out_h as usize + y_stride * (out_h as usize / 4) + b * 160 + 80;
            y.get(at).copied().unwrap_or(0)
        })
        .collect();
    out(format_args!(
        "first frame: identity {} buffer {} bytes {}; luma by eighths {bars:?}; lines equal to the middle one (within 2): {same} of {out_h}; Cb by eighths {chroma:?}",
        first.identity(),
        first.buffer(),
        first.len()
    ));
    if args.resample.is_some() {
        ok &= first.len() as u64 == u64::from(layout.size);
        let peak = y
            .get(..y_stride * out_h as usize)
            .and_then(|s| s.iter().max())
            .copied()
            .unwrap_or(0);
        out(format_args!(
            "resampled frame: {} bytes, the output's size {}, stride {y_stride}, peak luma {peak}, luma eighths {bars:?}",
            first.len(),
            layout.size
        ));
    }
    if let Some(dir) = &args.save {
        let mut pgm = format!("P5\n{out_w} {out_h}\n255\n").into_bytes();
        for row in 0..out_h as usize {
            pgm.extend_from_slice(
                y.get(row * y_stride..row * y_stride + out_w as usize)
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
    let base_plane = plane(y, y_stride, out_w as usize, out_h as usize);
    output
        .release(first)
        .map_err(|e| format!("release: {e:?}"))?;

    let mut identity = next_identity;
    if args.sharpen {
        // The same job again with the default sharpening, then the plain configuration back in
        // every config buffer (no job is in flight here).
        let sharp = build(input, bayer, (out_w, out_h), true)?;
        let mut sharp_bytes = vec![0u8; BeTilesConfig::SIZE];
        sharp
            .write_to(&mut sharp_bytes)
            .map_err(|_| "configuration size".to_string())?;
        for buffer in 0..configs {
            cfg.write(buffer, 0, &sharp_bytes)
                .map_err(|e| format!("config write: {e:?}"))?;
        }
        let sharp_plane = one_plane(
            &mut cfg,
            &mut output,
            &mut identity,
            y_stride,
            (out_w as usize, out_h as usize),
        )?;
        for buffer in 0..configs {
            cfg.write(buffer, 0, &config_bytes)
                .map_err(|e| format!("config write: {e:?}"))?;
        }
        let (sharp_bars, _) = luma(&sharp_plane, out_w as usize, out_w as usize, out_h as usize);
        let diff = mean_abs_diff(&base_plane, &sharp_plane);
        let changed = diff > 0.0;
        out(format_args!(
            "sharpen (default filters, enables {}): luma eighths {sharp_bars:?}; mean absolute luma difference from the same run without sharpen: {diff:.3}{}",
            SHARPEN_DEFAULT.enables,
            if changed {
                ""
            } else {
                "; the sharpen made no change"
            }
        ));
        ok &= changed;
    }

    // One job at a time: post to frame.
    let mut times = Vec::with_capacity(args.jobs as usize);
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
        post_on(
            &mut cfg,
            identity,
            posted % configs,
            posted % inputs,
            posted % outputs,
        )?;
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
            post_on(
                &mut cfg,
                identity,
                posted % configs,
                posted % inputs,
                posted % outputs,
            )?;
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
    ok &= distinct >= 6 && same > out_h as usize * 9 / 10 && in_order && done == args.jobs;

    if args.rate > 0 && args.seconds > 0 {
        let jobs = args.rate * args.seconds;
        let period = Duration::from_secs(1) / args.rate;
        let before = isp.counters().map_err(|s| format!("counters: {s:?}"))?;
        let started = Instant::now();
        let (mut posted, mut done, mut in_order) = (0u32, 0u32, true);
        let mut expect = identity;
        while posted < 2 {
            post_on(
                &mut cfg,
                identity,
                posted % configs,
                posted % inputs,
                posted % outputs,
            )?;
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
                post_on(
                    &mut cfg,
                    identity,
                    posted % configs,
                    posted % inputs,
                    posted % outputs,
                )?;
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
    let id = *identity;
    post_on(cfg, id, cfg_buffer, 0, out_buffer)?;
    *identity += 1;
    let deadline = Instant::now() + wait;
    loop {
        let left = deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        let Some(frame) = output
            .next_frame(left)
            .map_err(|e| format!("output0: {e:?}"))?
        else {
            return Ok(None);
        };
        if frame.identity() == id {
            let h = hash(output.bytes(&frame));
            output
                .release(frame)
                .map_err(|e| format!("release: {e:?}"))?;
            return Ok(Some(h));
        }
        // A frame of an earlier job that came late: released, and not this job's.
        output
            .release(frame)
            .map_err(|e| format!("release: {e:?}"))?;
        if Instant::now() >= deadline {
            return Ok(None);
        }
    }
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
    // The same configuration with its three white-balance gains zero: the driver's check does not
    // refuse it (only reserved bytes, tile count, HOG and the TDN flags), and the frame changes.
    // Its known output is black luma: the gains multiply each colour to zero, and the full-range
    // BT.601 luma offset is zero, so every Y byte is 0 (the all-zero check counts this as expected
    // for this configuration only, never for the white-balanced one).
    let mut parsed = BeTilesConfig::default();
    parsed
        .read_from(good)
        .map_err(|_| "configuration size".to_string())?;
    parsed.config.wbg = WbgConfig {
        gain_r: 0,
        gain_g: 0,
        gain_b: 0,
        pad: [0; 2],
    };
    let mut zeroed = vec![0u8; BeTilesConfig::SIZE];
    parsed
        .write_to(&mut zeroed)
        .map_err(|_| "configuration size".to_string())?;
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
            "usage: lemnos-isp-proof --input FILE [--socket PATH] [--isp ID] [--sensor ID] [--jobs N] [--save DIR] [--rate N --seconds S] [--checks 1] [--sharpen 1] [--resample WxH]"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampled_configuration_is_valid_for_the_driver() {
        let mut input = ImageFormatConfig {
            width: WIDTH as u16,
            height: HEIGHT as u16,
            format: formats::BAYER16,
            ..Default::default()
        };
        compute_stride_align(&mut input, 64);
        let config = build(input, BayerOrder::Rggb, (640, 400), false).expect("prepare");
        let image = config.config.output_format[0].image;
        assert_eq!((image.width, image.height), (640, 400));
        assert!(image.stride >= 640);
        let bytes = lemnos_driver_pisp::be_job::image_bytes(&image).expect("size");
        assert_eq!(bytes, u64::try_from(image.stride).unwrap() * 400 * 3 / 2);
        lemnos_driver_pisp::be_job::check(&config).expect("driver check");
    }

    #[test]
    fn sharpened_and_zero_gain_configurations_are_valid_for_the_driver() {
        let mut input = ImageFormatConfig {
            width: WIDTH as u16,
            height: HEIGHT as u16,
            format: formats::BAYER16,
            ..Default::default()
        };
        compute_stride_align(&mut input, 64);
        let sharp = build(input, BayerOrder::Rggb, (WIDTH, HEIGHT), true).expect("prepare");
        lemnos_driver_pisp::be_job::check(&sharp).expect("driver check");
        assert_ne!(sharp.config.sharpen.enables, 0);
        let plain = build(input, BayerOrder::Rggb, (WIDTH, HEIGHT), false).expect("prepare");
        let mut zero = plain;
        zero.config.wbg = WbgConfig {
            gain_r: 0,
            gain_g: 0,
            gain_b: 0,
            pad: [0; 2],
        };
        lemnos_driver_pisp::be_job::check(&zero).expect("driver check");
    }

    /// A 640x400 NV12 frame: luma rows of 50, then the chroma plane of 128 (as the block writes it).
    fn nv12_640x400() -> Vec<u8> {
        let mut frame = vec![50u8; 640 * 400];
        frame.extend(std::iter::repeat_n(128u8, 640 * 200));
        frame
    }

    #[test]
    fn resized_luma_reads_the_luma_plane() {
        let frame = nv12_640x400();
        let (bars, same) = luma(&frame, 640, 640, 400);
        assert_eq!(bars, vec![50; 8]);
        assert_eq!(same, 400);
    }

    #[test]
    fn the_full_size_readout_of_a_resized_frame_reads_chroma() {
        // The board run's bug: the readout used the 1280x800 size for a 640x400 frame. Its middle
        // row (400) is the first chroma row, so the eighths read 128, not the luma.
        let frame = nv12_640x400();
        let (bars, _) = luma(&frame, 640, 1280, 800);
        assert_eq!(bars, vec![128; 8]);
    }

    #[test]
    fn a_1280_wide_readout_keeps_its_margins() {
        // The same bars as before the change at 1280: a frame of 50 with 128 beside each bar's edge.
        let mut row = vec![128u8; 1280];
        for b in 0..8 {
            row[b * 160 + 40..b * 160 + 120].fill(50);
        }
        let mut frame = row.clone();
        frame.extend(std::iter::repeat_n(50u8, 1280 * 799));
        let (bars, _) = luma(&frame, 1280, 1280, 800);
        assert_eq!(bars, vec![50; 8]);
    }

    #[test]
    fn a_short_frame_reads_as_no_lines_not_as_zero_luma_from_a_wrong_size() {
        let frame = nv12_640x400();
        let (bars, same) = luma(&frame[..1000], 640, 640, 400);
        assert_eq!(bars, vec![0; 8]);
        assert_eq!(same, 0);
    }
}
