//! `lemnos-camera-proof`: the camera proof on a real board, through `lemnosd`'s socket.
//!
//! ```text
//! lemnos-camera-proof [--socket PATH] [--sensor ID] [--mode N] [--pattern CODE]
//!                     [--exposure N] [--frame-length N] [--frames N] [--save DIR]
//!                     [--hold-ms N] [--attach-late-ms N] [--reattach N] [--stop-first]
//! ```
//!
//! Assembles the camera from the board's links, starts it, takes `--frames` frames off the
//! receiver's stream and checks what is in them, then stops it. Every frame's bytes are read in
//! place. It prints:
//!
//! - the frames taken, the sequence gaps, the receiver's own drop count, and the frame rate from
//!   the frames' timestamps;
//! - the intervals between frame starts, and from them the frames the block never saw: a frame
//!   that starts while the receiver has no buffer armed makes no frame start at all, so it has
//!   no sequence number and is not in the receiver's count;
//! - the time from a frame's timestamp to this program holding it (both on the monotonic clock);
//! - the content: for packed RAW10 or RAW8 the 8 most significant bits of every pixel, as the
//!   mean, minimum and maximum of each frame, the share of zero bytes, how many frames differed
//!   from the one before, and whether rows of the same parity are identical (what the sensor's
//!   colour bars give);
//! - with `--save`, the 10th frame and the last as raw files, and their statistics.
//!
//! `--hold-ms` keeps the first frame unreleased that long while the rest flow, and then checks
//! its bytes did not change (a held buffer is never rewritten). `--attach-late-ms` starts the
//! camera with no consumer and takes the stream that much later. `--reattach` then gives the
//! stream back and takes it again that many times while the camera runs, reading 10 frames each
//! time. `--stop-first` stops the camera before starting it: the camera drivers don't track who
//! started them, so a camera whose caller died keeps running until a caller stops it.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use lemnos_camera_proof::{Error, Options, SocketBus, assemble};
use lemnos_client::Client;
use lemnos_contract_camera_sensor::Controls;

#[derive(Debug, Default)]
struct Args {
    socket: Option<String>,
    sensor: Option<String>,
    mode: u16,
    pattern: Option<u16>,
    exposure: Option<u32>,
    frames: u64,
    save: Option<PathBuf>,
    hold_ms: u64,
    frame_length: Option<u32>,
    attach_late_ms: u64,
    reattach: u32,
    stop_first: bool,
}

fn parse() -> Option<Args> {
    let mut args = Args {
        frames: 100,
        ..Args::default()
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        if flag == "--stop-first" {
            args.stop_first = true;
            continue;
        }
        let value = it.next()?;
        match flag.as_str() {
            "--socket" => args.socket = Some(value),
            "--sensor" => args.sensor = Some(value),
            "--mode" => args.mode = value.parse().ok()?,
            "--pattern" => args.pattern = Some(value.parse().ok()?),
            "--exposure" => args.exposure = Some(value.parse().ok()?),
            "--frames" => args.frames = value.parse().ok()?,
            "--save" => args.save = Some(PathBuf::from(value)),
            "--hold-ms" => args.hold_ms = value.parse().ok()?,
            "--frame-length" => args.frame_length = Some(value.parse().ok()?),
            "--attach-late-ms" => args.attach_late_ms = value.parse().ok()?,
            "--reattach" => args.reattach = value.parse().ok()?,
            _ => return None,
        }
    }
    Some(args)
}

fn monotonic_ns() -> u64 {
    lemnos_linux_sys::time::monotonic_now()
        .map_or(0, |now| u64::try_from(now.as_nanos()).unwrap_or(u64::MAX))
}

/// What one frame holds, from the 8 most significant bits of each pixel.
#[derive(Debug, Clone, Copy, Default)]
struct Content {
    mean_milli: u64,
    min: u8,
    max: u8,
    zero_bytes: usize,
    hash: u64,
}

/// `packed10`: every fifth byte holds the low bits of the four before it.
fn content(bytes: &[u8], packed10: bool) -> Content {
    let mut sum = 0u64;
    let mut count = 0u64;
    let mut min = u8::MAX;
    let mut max = 0u8;
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut zero_bytes = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        hash = (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        zero_bytes += usize::from(b == 0);
        if packed10 && i % 5 == 4 {
            continue;
        }
        sum += u64::from(b);
        count += 1;
        min = min.min(b);
        max = max.max(b);
    }
    Content {
        mean_milli: (sum * 1000).checked_div(count).unwrap_or(0),
        min,
        max,
        zero_bytes,
        hash,
    }
}

/// Whether every row equals the first row of its parity, and how many runs of one value the
/// first row has (in its most significant bytes).
fn rows(bytes: &[u8], stride: usize, height: usize, packed10: bool) -> (bool, usize) {
    let row = |r: usize| bytes.get(r * stride..(r + 1) * stride).unwrap_or(&[]);
    let same = (2..height).all(|r| row(r) == row(r % 2));
    let mut runs = 0;
    let mut last = None;
    for (i, &b) in row(0).iter().enumerate() {
        if packed10 && i % 5 == 4 {
            continue;
        }
        if last != Some(b) {
            runs += 1;
            last = Some(b);
        }
    }
    (same, runs)
}

fn percentile(sorted: &[u64], p: usize) -> u64 {
    sorted
        .get((sorted.len() * p / 100).min(sorted.len().saturating_sub(1)))
        .copied()
        .unwrap_or(0)
}

fn out(text: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stdout(), "{text}");
}

#[allow(clippy::too_many_lines)]
fn run(args: &Args) -> Result<bool, Error> {
    let socket = args
        .socket
        .clone()
        .or_else(|| std::env::var("LEMNOSD_SOCKET").ok())
        .unwrap_or_else(|| "/run/lemnos/lemnosd.sock".to_string());
    let client = Client::connect(&socket, "lemnos-camera-proof")
        .map_err(|e| Error::Save(format!("{socket}: {e}")))?;
    client.set_timeout(Duration::from_secs(5));
    let bus = SocketBus::new(client);
    let mut camera = assemble(
        &bus,
        &Options {
            sensor: args.sensor.clone(),
            mode: args.mode,
        },
    )?;
    let mode = *camera.mode();
    out(format_args!(
        "camera: sensor {} -> receiver {}{}, stream {:?}; mode {}: {}x{} {}-bit, {} lanes at {} Mbit/s, max {} mfps",
        camera.sensor().id,
        camera.receiver().id,
        camera
            .isp()
            .map(|d| format!(" -> isp {}", d.id))
            .unwrap_or_default(),
        camera.stream(),
        args.mode,
        mode.link.width,
        mode.link.height,
        mode.link.bit_depth,
        mode.link.lanes,
        mode.link.link_rate_mbps,
        mode.max_fps_milli,
    ));
    let receiver = camera.receiver().clone();
    let stream_name = camera.stream().to_string();
    let mut early = if args.attach_late_ms == 0 {
        Some(bus.take_stream(&receiver, &stream_name, false)?)
    } else {
        None
    };
    if args.stop_first {
        // A camera left running by a caller that went away runs until someone stops it.
        let stopped = camera.stop();
        out(format_args!("stopped first: {stopped:?}"));
    }
    if let Some(code) = args.pattern {
        camera.set_test_pattern(code)?;
    }
    if args.exposure.is_some() || args.frame_length.is_some() {
        camera.set_controls(Controls {
            exposure: args.exposure,
            frame_length: args.frame_length,
            ..Controls::default()
        })?;
    }
    let started = Instant::now();
    let layout = camera.start()?;
    out(format_args!(
        "started in {} us: layout {}x{} stride {} size {} bit depth {}",
        started.elapsed().as_micros(),
        layout.width,
        layout.height,
        layout.stride,
        layout.size,
        layout.bit_depth,
    ));
    let mut stream = match early.take() {
        Some(stream) => stream,
        None => {
            std::thread::sleep(Duration::from_millis(args.attach_late_ms));
            let before = camera.counters()?;
            out(format_args!(
                "{} ms started with no consumer: {before:?}",
                args.attach_late_ms
            ));
            bus.take_stream(&receiver, &stream_name, false)?
        }
    };
    let packed10 =
        layout.bit_depth == 10 && u64::from(layout.stride) * 8 < u64::from(layout.width) * 16;
    let mut latencies = Vec::with_capacity(usize::try_from(args.frames).unwrap_or(0));
    let mut intervals = Vec::with_capacity(usize::try_from(args.frames).unwrap_or(0));
    let mut previous_ts = None;
    let mut first_seq = None;
    let mut last_seq = 0u64;
    let (mut first_ts, mut last_ts) = (0u64, 0u64);
    let mut taken = 0u64;
    let mut differing = 0u64;
    let mut previous_hash = None;
    let mut all_bars = true;
    let mut runs_seen = Vec::new();
    let mut buffers_seen = 0u64;
    let mut held = None;
    let mut held_copy = Vec::new();
    let mut held_since = Instant::now();
    let mut zero_frames = 0u64;
    let mut stalls = 0u32;
    let mut summary = Vec::new();
    while taken < args.frames {
        let Some(frame) = stream
            .next_frame(Duration::from_secs(1))
            .map_err(|_| Error::ReadFailed)?
        else {
            stalls += 1;
            out(format_args!("no frame for 1 s (after {taken})"));
            if stalls >= 5 {
                break;
            }
            continue;
        };
        let now = monotonic_ns();
        latencies.push(now.saturating_sub(frame.timestamp_ns()));
        if let Some(before) = previous_ts {
            intervals.push(frame.timestamp_ns().saturating_sub(before));
        }
        previous_ts = Some(frame.timestamp_ns());
        let bytes = stream.bytes(&frame);
        let c = content(bytes, packed10);
        let (bars, runs) = rows(
            bytes,
            layout.stride as usize,
            layout.height as usize,
            packed10,
        );
        all_bars &= bars;
        if runs_seen.len() < 4 && !runs_seen.contains(&runs) {
            runs_seen.push(runs);
        }
        if c.max == 0 && zero_frames < 4 {
            out(format_args!(
                "all-zero frame: taken {taken} seq {} buffer {}, {} us after the frame before",
                frame.sequence(),
                frame.buffer(),
                intervals.last().copied().unwrap_or(0) / 1000
            ));
        }
        zero_frames += u64::from(c.max == 0);
        if previous_hash.is_some_and(|h| h != c.hash) {
            differing += 1;
        }
        previous_hash = Some(c.hash);
        buffers_seen |= 1 << frame.buffer().min(63);
        if first_seq.is_none() {
            first_seq = Some(frame.sequence());
            first_ts = frame.timestamp_ns();
        }
        last_seq = frame.sequence();
        last_ts = frame.timestamp_ns();
        if taken < 3 || taken + 1 == args.frames || taken.is_multiple_of(600) {
            summary.push(format!(
                "frame {taken}: seq {} buffer {} bytes {} mean {}.{:03} min {} max {} zero bytes {} hash {:016x} rows same by parity {bars} runs {runs}",
                frame.sequence(),
                frame.buffer(),
                frame.len(),
                c.mean_milli / 1000,
                c.mean_milli % 1000,
                c.min,
                c.max,
                c.zero_bytes,
                c.hash
            ));
        }
        if let Some(dir) = &args.save
            && (taken == 10 || taken + 1 == args.frames)
        {
            let path = dir.join(format!("frame-{taken:05}-seq{}.raw", frame.sequence()));
            std::fs::write(&path, bytes).map_err(|e| Error::Save(e.to_string()))?;
            out(format_args!("saved {}", path.display()));
        }
        taken += 1;
        if args.hold_ms > 0 && held.is_none() && taken == 1 {
            held_copy = bytes.to_vec();
            held = Some(frame);
            held_since = Instant::now();
            continue;
        }
        stream.release(frame).map_err(|_| Error::ReturnQueueFull)?;
        if let Some(h) =
            held.take_if(|_| held_since.elapsed() >= Duration::from_millis(args.hold_ms))
        {
            let unchanged = stream.bytes(&h) == held_copy.as_slice();
            out(format_args!(
                "held frame (buffer {}) unchanged after {} ms while {} others flowed: {unchanged}",
                h.buffer(),
                held_since.elapsed().as_millis(),
                taken - 1
            ));
            stream.release(h).map_err(|_| Error::ReturnQueueFull)?;
        }
    }
    for round in 0..args.reattach {
        bus.client()
            .close_stream(stream)
            .map_err(|_| Error::ReadFailed)?;
        stream = bus.take_stream(&receiver, &stream_name, false)?;
        let attached = Instant::now();
        let mut good = 0;
        let mut first_ms = None;
        for _ in 0..10 {
            if let Some(frame) = stream
                .next_frame(Duration::from_secs(1))
                .map_err(|_| Error::ReadFailed)?
            {
                first_ms.get_or_insert(attached.elapsed().as_millis());
                good += u32::from(content(stream.bytes(&frame), packed10).max > 0);
                stream.release(frame).map_err(|_| Error::ReturnQueueFull)?;
            }
        }
        out(format_args!(
            "reattach {round}: first frame after {first_ms:?} ms, {good} of 10 frames with content; {:?}",
            camera.counters()?
        ));
    }
    let counters = camera.counters()?;
    let stop_started = Instant::now();
    camera.stop()?;
    let stop_us = stop_started.elapsed().as_micros();
    for line in &summary {
        out(format_args!("{line}"));
    }
    latencies.sort_unstable();
    let span = last_seq.saturating_sub(first_seq.unwrap_or(0)) + 1;
    let fps_milli = if taken > 1 && last_ts > first_ts {
        u128::from(taken - 1) * 1_000_000_000_000 / u128::from(last_ts - first_ts)
    } else {
        0
    };
    out(format_args!(
        "frames {taken} in {} ms; sequence gaps {}; receiver counters: {counters:?}",
        started.elapsed().as_millis(),
        span.saturating_sub(taken)
    ));
    out(format_args!(
        "frame rate from timestamps: {}.{:03} fps",
        fps_milli / 1000,
        fps_milli % 1000
    ));
    // A frame that starts while the channel has nothing armed is never seen by the block: it has
    // no sequence number and no drop is counted. Only the time between frame starts shows it.
    intervals.sort_unstable();
    let period = percentile(&intervals, 50);
    let unseen: u64 = intervals
        .iter()
        .filter(|&&gap| period > 0 && gap > period + period / 2)
        .map(|&gap| (gap + period / 2) / period - 1)
        .sum();
    out(format_args!(
        "frame start intervals: median {} us, max {} us; frames the block never saw, from the intervals: {unseen}",
        period / 1000,
        intervals.last().copied().unwrap_or(0) / 1000
    ));
    out(format_args!(
        "timestamp to client: median {} us, p99 {} us, max {} us",
        percentile(&latencies, 50) / 1000,
        percentile(&latencies, 99) / 1000,
        latencies.last().copied().unwrap_or(0) / 1000
    ));
    out(format_args!(
        "content: {zero_frames} all-zero frames; {differing} of {} frames differ from the one before; rows same by parity in every frame: {all_bars}; runs in row 0: {runs_seen:?}; buffers seen: {buffers_seen:#x}",
        taken.saturating_sub(1)
    ));
    out(format_args!("stop took {stop_us} us"));
    Ok(taken == args.frames && zero_frames == 0)
}

fn main() -> ExitCode {
    let Some(args) = parse() else {
        let _ = writeln!(
            std::io::stderr(),
            "usage: lemnos-camera-proof [--socket PATH] [--sensor ID] [--mode N] [--pattern CODE] [--exposure N] [--frame-length N] [--frames N] [--save DIR] [--hold-ms N] [--attach-late-ms N] [--reattach N] [--stop-first]"
        );
        return ExitCode::from(2);
    };
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "lemnos-camera-proof: {e}");
            ExitCode::FAILURE
        }
    }
}
