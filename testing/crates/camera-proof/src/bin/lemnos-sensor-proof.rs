//! `lemnos-sensor-proof`: the OV9782 driver's bus traffic, and when posted values land.
//!
//! ```text
//! lemnos-sensor-proof trace [--bus i2c-10] [--continuous-clock 1]
//! lemnos-sensor-proof latch [--socket PATH] [--sensor ID] [--trials N]
//! ```
//!
//! `trace` runs the driver in this process on the board's bus (no daemon; the sensor's power
//! must already be on), with every transaction recorded: it binds, reads every mode, selects
//! mode 0, sets controls, starts, posts values, sets every flip and every test pattern, and
//! stops. It prints the transactions and the registers written, and whether any transaction
//! named OTP_PROGRAM (`0x3D80`), as a register written, a burst covering it, or a register read.
//!
//! `latch` runs the camera through `lemnosd` (mode 0, the scene) at a short exposure, and for each
//! offset of 0.5 to 16.5 ms after a frame's start posts a long exposure (`post_values`), then the
//! short one again. The frame a post lands on is the first whose brightness is past halfway to
//! the long exposure's. The prediction is the frame the call completed in plus 2: each post is
//! counted as landing on it, earlier or later. Then two posts in one frame, a middle exposure at
//! 4 ms and the long one at 12 ms: which shows.

use std::io::Write;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lemnos_camera_proof::{Error, Options, SocketBus, assemble};
use lemnos_client::{Client, FrameStream, HeldFrame};
use lemnos_contract_camera_sensor::{CameraSensor, Controls, FrameValues};
use lemnos_core::{Config, ConfigValue, I2c, Lifecycle, Provider};
use lemnos_driver_ov9782::chip::OTP_PROGRAM;
use lemnos_driver_ov9782::{Ov9782, Ov9782Needs};
use lemnos_hal::ErrorKind;
use lemnos_hal::i2c::{ErrorType, I2c as HalI2c, Operation};
use lemnos_linux::LinuxBoard;

fn out(text: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stdout(), "{text}");
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

/// One transaction: the target, and each operation as written bytes or a read's length.
#[derive(Debug, Clone)]
struct Transfer {
    address: u8,
    ops: Vec<Op>,
}

#[derive(Debug, Clone)]
enum Op {
    Write(Vec<u8>),
    Read(usize),
}

/// The board's bus with every transaction recorded.
struct Recording {
    inner: I2c,
    log: Arc<Mutex<Vec<Transfer>>>,
}

impl ErrorType for Recording {
    type Error = ErrorKind;
}

impl HalI2c for Recording {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), ErrorKind> {
        let ops = operations
            .iter()
            .map(|op| match op {
                Operation::Write(bytes) => Op::Write(bytes.to_vec()),
                Operation::Read(buf) => Op::Read(buf.len()),
            })
            .collect();
        if let Ok(mut log) = self.log.lock() {
            log.push(Transfer { address, ops });
        }
        self.inner.0.transaction(address, operations)
    }
}

/// Whether a transaction names `register`: a write starting at it or covering it, or a read
/// after a write of its address.
fn names(t: &Transfer, register: u16) -> bool {
    let mut previous: Option<u16> = None;
    for op in &t.ops {
        match op {
            Op::Write(bytes) if bytes.len() >= 2 => {
                let start = u16::from_be_bytes([bytes[0], bytes[1]]);
                let data = (bytes.len() - 2) as u16;
                if start == register || (data > 0 && (start..start + data).contains(&register)) {
                    return true;
                }
                previous = Some(start);
            }
            Op::Read(_) if previous == Some(register) => return true,
            _ => {}
        }
    }
    false
}

fn now_ns() -> u64 {
    lemnos_linux_sys::time::monotonic_now()
        .map(|t| u64::try_from(t.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[allow(clippy::too_many_lines)]
fn trace(args: &[String]) -> Result<bool, String> {
    let bus_name = flag(args, "--bus").unwrap_or_else(|| "i2c-10".into());
    let continuous = flag(args, "--continuous-clock").is_some_and(|v| v != "0");
    let mut board = LinuxBoard::new();
    let inner = board
        .i2c(&bus_name)
        .map_err(|s| format!("{bus_name}: {s:?}"))?;
    let log = Arc::new(Mutex::new(Vec::new()));
    let bus = I2c(lemnos_core::Held::boxed(Box::new(Recording {
        inner,
        log: log.clone(),
    })));
    let entries = [("continuous_clock", ConfigValue::Bool(continuous))];
    let mut s = <Ov9782 as Lifecycle>::start(
        Ov9782Needs {
            bus,
            values: None,
            _receiver: None,
        },
        &Config::new(&entries),
    )
    .map_err(|e| format!("bind: {e:?}"))?;
    let mut steps = Vec::new();
    let mut step = |name: &str, r: lemnos_core::Result<()>| {
        steps.push(format!("{name}: {r:?}"));
    };
    let info = s.info();
    step("info", info.map(|_| ()));
    let mut modes = 0u16;
    while s.mode(modes).is_ok() {
        modes += 1;
    }
    for m in 0..modes {
        step(&format!("select_mode({m})"), s.select_mode(m));
    }
    step("select_mode(0)", s.select_mode(0));
    let mode = s.mode(0).map_err(|e| format!("mode 0: {e:?}"))?;
    step(
        "set_controls",
        s.set_controls(Controls {
            exposure: Some(400 * 16),
            analogue_gain: Some(32),
            digital_gain: None,
            frame_length: Some(mode.frame_length_min + 100),
        }),
    );
    step("start", s.start());
    for i in 0..30u64 {
        step(
            "post_values",
            s.post_values(FrameValues {
                identity: i,
                controls: Controls {
                    exposure: Some(u32::try_from(100 + i * 10).unwrap_or(100) * 16),
                    analogue_gain: Some(16 + u32::try_from(i).unwrap_or(0)),
                    digital_gain: None,
                    frame_length: None,
                },
            }),
        );
        let _ = <Ov9782 as Lifecycle>::tick(&mut s, now_ns());
        std::thread::sleep(Duration::from_millis(5));
    }
    for (h, v) in [(false, false), (true, false), (false, true), (true, true)] {
        step(
            &format!("set_flips({h}, {v})"),
            s.set_flips(h, v).map(|_| ()),
        );
    }
    let mut code = 0u16;
    loop {
        let r = s.set_test_pattern(code);
        let done = r.is_err();
        step(&format!("set_test_pattern({code})"), r);
        if done || code > 16 {
            break;
        }
        code += 1;
    }
    step("set_test_pattern(0)", s.set_test_pattern(0));
    step("stop", s.stop());
    drop(s);

    let log = log.lock().map_err(|_| "log".to_string())?;
    let mut written = std::collections::BTreeMap::new();
    let (mut reads, mut writes, mut read_bytes) = (0u32, 0u32, 0usize);
    let mut targets = std::collections::BTreeMap::new();
    for t in log.iter() {
        *targets.entry(t.address).or_insert(0u32) += 1;
        let mut previous = None;
        for op in &t.ops {
            match op {
                Op::Write(b) if b.len() >= 2 => {
                    let reg = u16::from_be_bytes([b[0], b[1]]);
                    previous = Some(reg);
                    if b.len() > 2 {
                        writes += 1;
                        *written.entry(reg).or_insert(0u32) += 1;
                    }
                }
                Op::Read(n) if previous.is_some() => {
                    reads += 1;
                    read_bytes += n;
                }
                _ => {}
            }
        }
    }
    let otp: Vec<&Transfer> = log.iter().filter(|t| names(t, OTP_PROGRAM)).collect();
    for line in &steps {
        out(format_args!("{line}"));
    }
    out(format_args!(
        "{} transactions on {bus_name} (continuous clock {continuous}), by target {targets:02x?}: {writes} register writes to {} registers, {reads} register reads ({read_bytes} bytes); registers written {:04x?}",
        log.len(),
        written.len(),
        written.keys().collect::<Vec<_>>()
    ));
    out(format_args!(
        "transactions naming OTP_PROGRAM ({OTP_PROGRAM:#06x}): {}",
        otp.len()
    ));
    for t in otp.iter().take(4) {
        out(format_args!("  {t:?}"));
    }
    Ok(otp.is_empty() && !log.is_empty())
}

/// Mean of the high bytes of a packed RAW10 frame, every 61st sample.
fn brightness(bytes: &[u8]) -> f64 {
    let (mut sum, mut n) = (0u64, 0u64);
    for (i, &b) in bytes.iter().enumerate().step_by(61) {
        if i % 5 != 4 {
            sum += u64::from(b);
            n += 1;
        }
    }
    sum as f64 / n.max(1) as f64
}

struct Seen {
    sequence: u64,
    start_ns: u64,
    level: f64,
}

fn next(stream: &mut FrameStream) -> Result<Seen, String> {
    let frame: HeldFrame = stream
        .next_frame(Duration::from_secs(1))
        .map_err(|e| format!("frames: {e:?}"))?
        .ok_or("no frame for 1 s")?;
    let seen = Seen {
        sequence: frame.sequence(),
        start_ns: frame.timestamp_ns(),
        level: brightness(stream.bytes(&frame)),
    };
    stream
        .release(frame)
        .map_err(|e| format!("release: {e:?}"))?;
    Ok(seen)
}

#[allow(clippy::too_many_lines)]
fn latch(args: &[String]) -> Result<bool, String> {
    let socket = flag(args, "--socket").unwrap_or_else(|| "/run/lemnos/lemnosd.sock".into());
    let trials: u32 = flag(args, "--trials")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let client =
        Client::connect(&socket, "lemnos-sensor-proof").map_err(|e| format!("{socket}: {e}"))?;
    client.set_timeout(Duration::from_secs(5));
    let bus = SocketBus::new(client);
    let mut camera = assemble(
        &bus,
        &Options {
            sensor: flag(args, "--sensor"),
            mode: 0,
        },
    )
    .map_err(|e: Error| format!("{e}"))?;
    let frame_length = camera.mode().frame_length_min.max(1822);
    let max_exposure = (frame_length - 25) * 16;
    let (short, middle, long) = (max_exposure / 8, max_exposure / 3, max_exposure);
    let controls = |exposure| Controls {
        exposure: Some(exposure),
        frame_length: Some(frame_length),
        ..Controls::default()
    };
    // The scene: the sensor keeps a test pattern an earlier caller chose.
    camera.set_test_pattern(0).map_err(|e| format!("{e}"))?;
    camera
        .set_controls(controls(short))
        .map_err(|e| format!("{e}"))?;
    let receiver = camera.receiver().clone();
    let name = camera.stream().to_string();
    let mut stream = bus
        .take_stream(&receiver, &name, false)
        .map_err(|e| format!("{e}"))?;
    camera.start().map_err(|e| format!("{e}"))?;
    let mut identity = 0u64;
    let mut post = |camera: &mut lemnos_camera_proof::Camera<'_, SocketBus>, exposure| {
        identity += 1;
        camera.post_values(FrameValues {
            identity,
            controls: controls(exposure),
        })
    };
    // Settle, and the three levels.
    let mut level = |stream: &mut FrameStream, camera: &mut _, exposure| -> Result<f64, String> {
        post(camera, exposure).map_err(|e| format!("{e}"))?;
        let mut last = 0.0;
        for _ in 0..8 {
            last = next(stream)?.level;
        }
        Ok(last)
    };
    let l_long = level(&mut stream, &mut camera, long)?;
    let l_middle = level(&mut stream, &mut camera, middle)?;
    let l_short = level(&mut stream, &mut camera, short)?;
    let period = {
        let a = next(&mut stream)?;
        let b = next(&mut stream)?;
        b.start_ns.saturating_sub(a.start_ns)
    };
    out(format_args!(
        "frame length {frame_length}, period {} us; exposures short {short} middle {middle} long {long}: brightness {l_short:.1}, {l_middle:.1}, {l_long:.1}",
        period / 1000
    ));
    // A landing is told by brightness: with no light on the sensor the exposures look alike, and
    // every count below would be noise.
    if l_long - l_short < 8.0 {
        let _ = camera.stop();
        return Err(format!(
            "the scene does not change with exposure (brightness {l_short:.1} short, {l_long:.1} long): light it and run again"
        ));
    }
    let halfway = (l_short + l_long) / 2.0;
    let mut deadline_ok = None;
    let mut all_ok = true;
    for offset_us in [
        500u64, 2000, 4000, 6000, 8000, 10000, 12000, 14000, 15000, 16000, 16500,
    ] {
        let (mut predicted, mut early, mut late, mut lost) = (0u32, 0u32, 0u32, 0u32);
        let mut wire = Vec::new();
        for _ in 0..trials {
            let f = next(&mut stream)?;
            // The frame start the offset counts from is this frame's; the post goes in that long
            // after it, which may already be into the next frame.
            let due = f.start_ns + offset_us * 1000;
            while now_ns() < due {
                std::hint::spin_loop();
            }
            let posted = now_ns();
            post(&mut camera, long).map_err(|e| format!("{e}"))?;
            let done = now_ns();
            wire.push(done - posted);
            // Frame sequence numbers count frame starts: the frame the call completed in.
            let completed_in = f.sequence + (done - f.start_ns) / period.max(1);
            let mut landed = None;
            for _ in 0..8 {
                let g = next(&mut stream)?;
                if g.level > halfway && landed.is_none() {
                    landed = Some(g.sequence);
                }
            }
            match landed {
                Some(s) if s == completed_in + 2 => predicted += 1,
                Some(s) if s < completed_in + 2 => early += 1,
                Some(_) => late += 1,
                None => lost += 1,
            }
            post(&mut camera, short).map_err(|e| format!("{e}"))?;
            for _ in 0..5 {
                next(&mut stream)?;
            }
        }
        wire.sort_unstable();
        out(format_args!(
            "post at {:.1} ms: {trials} posts, {predicted} on the predicted frame (completed in + 2), {early} earlier, {late} later, {lost} never; call median {} us, max {} us",
            offset_us as f64 / 1000.0,
            wire.get(wire.len() / 2).copied().unwrap_or(0) / 1000,
            wire.last().copied().unwrap_or(0) / 1000
        ));
        all_ok &= early == 0 && lost == 0;
        if predicted == trials {
            deadline_ok = Some(offset_us);
        }
    }
    out(format_args!(
        "largest offset with every post on the predicted frame: {:?} us",
        deadline_ok
    ));

    // Two posts in one frame: middle at 4 ms, long at 12 ms.
    let (mut second, mut first_seen) = (0u32, 0u32);
    for _ in 0..trials {
        let f = next(&mut stream)?;
        for (at, exposure) in [(4000u64, middle), (12000, long)] {
            let due = f.start_ns + at * 1000;
            while now_ns() < due {
                std::hint::spin_loop();
            }
            post(&mut camera, exposure).map_err(|e| format!("{e}"))?;
        }
        let (mut saw_middle, mut saw_long) = (false, false);
        for _ in 0..6 {
            let g = next(&mut stream)?;
            let to_long = (g.level - l_long).abs();
            let to_middle = (g.level - l_middle).abs();
            let to_short = (g.level - l_short).abs();
            saw_long |= to_long < to_middle && to_long < to_short;
            saw_middle |= to_middle < to_long && to_middle < to_short;
        }
        second += u32::from(saw_long);
        first_seen += u32::from(saw_middle);
        post(&mut camera, short).map_err(|e| format!("{e}"))?;
        for _ in 0..5 {
            next(&mut stream)?;
        }
    }
    out(format_args!(
        "two posts in one frame (middle at 4 ms, long at 12 ms), {trials} times: the long one showed {second} times, the middle one {first_seen} times"
    ));
    let counters = camera.counters().map_err(|e| format!("{e}"))?;
    out(format_args!("receiver counters: {counters:?}"));
    camera.stop().map_err(|e| format!("{e}"))?;
    Ok(all_ok && second == trials && first_seen == 0)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("trace") => trace(&args),
        Some("latch") => latch(&args),
        _ => {
            out(format_args!(
                "usage: lemnos-sensor-proof trace [--bus NAME] [--continuous-clock 1] | latch [--socket PATH] [--sensor ID] [--trials N]"
            ));
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            out(format_args!("lemnos-sensor-proof: {e}"));
            ExitCode::FAILURE
        }
    }
}
