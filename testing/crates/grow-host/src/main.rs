#![allow(clippy::print_stdout, clippy::print_stderr)]

//! The GROW2 liveness harness (test only, never shipped): the engine host, the same `Host::start`
//! that lemnosd calls, with `system-slots` on the real disk and `hw-watchdog` on the watchdog, run
//! inside the QEMU guest.
//!
//!   lemnos-grow-host <disk node> <data dir> <watchdog timeout s> <max run s>
//!
//! The slots driver takes its boot chain from the device tree on a board. An x86 guest has none, so
//! this harness stands the chain in with `boot_node::TEST_ROOTS` (the `testing` feature): a fake
//! `chosen` and a fake sysfs whose `class/block/<disk>` is the real disk's name. The grow itself
//! reads and writes the real disk through the real sysfs and the real mount.
//!
//! Output (on the console, one line each): `grow-host: resize started` when the slots driver first
//! reports `resizing` (the harness's kill point), each change of `grow_state`, and at the end the
//! latency probe's numbers: the calls through `host.call` to the watchdog's `self-test` method every
//! 100 ms, their longest latency and longest gap between completed calls.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use lemnos_core::{HostClock, MethodId};
use lemnos_host::board::parse;
use lemnos_host::{Host, Liveness, Options, entry};
use lemnos_slot_store::boot_node::{BootRoots, TEST_ROOTS};

/// The driver clock: nanoseconds since the host started.
struct Clock(Instant);

impl HostClock for Clock {
    fn now_ns(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// The daemon is healthy for the whole run: the watchdog is fed by the host's ticks.
struct Alive;

impl Liveness for Alive {
    fn alive(&self) -> bool {
        true
    }
}

/// The fake boot chain: `chosen/lemnos,boot-source`, and a sysfs in which the disk's block node sits
/// under a controller whose device-tree node is `/soc@1/mmc`, as the board's `boot_sources` names it.
fn stand_in_boot_chain(root: &Path, disk: &str) -> std::io::Result<BootRoots> {
    let chosen = root.join("chosen");
    fs::create_dir_all(&chosen)?;
    fs::write(chosen.join("lemnos,boot-source"), "rpi-boot-mode-1")?;
    let sys = root.join("sys");
    let under = sys.join("devices/fake/soc").join(disk);
    fs::create_dir_all(&under)?;
    fs::create_dir_all(sys.join("firmware/devicetree/base/soc@1/mmc"))?;
    symlink(
        "../../../firmware/devicetree/base/soc@1/mmc",
        sys.join("devices/fake/soc/of_node"),
    )?;
    fs::create_dir_all(sys.join("class/block"))?;
    symlink(
        format!("../../devices/fake/soc/{disk}"),
        sys.join("class/block").join(disk),
    )?;
    Ok(BootRoots {
        chosen,
        sys,
        dev: PathBuf::from("/dev"),
    })
}

/// The board file this harness runs: the slots driver on the data partition, the watchdog as a
/// keep-alive device. The boot chain names the controller the fake sysfs gives the disk.
fn board_text(data: &str, watchdog_s: u32) -> String {
    format!(
        r#"format = "lemnos.board"
schema_version = 1

[board]
id = "grow-host"

[[devices]]
id = "slots"
driver = "system-slots"
config = {{ data = "{data}", boot = "/run/grow-host/boot", record = "file", boot_sources = ["rpi-boot-mode-1=/soc@1/mmc"] }}

[[devices]]
id = "wdt"
driver = "hw-watchdog"
keepalive = true
config = {{ device = "watchdog0", timeout_s = {watchdog_s} }}
"#
    )
}

/// The index and the method ids of a device's contract, by method name: `(contract, method)`.
fn method_of(host: &Host, device: u32, name: &str) -> Option<MethodId> {
    let manifest = host.manifest(device)?;
    for (contract, info) in manifest.provides.iter().enumerate() {
        for method in info.methods {
            if method.name == name {
                return Some(MethodId {
                    contract: u8::try_from(contract).ok()?,
                    method: method.id,
                });
            }
        }
    }
    None
}

/// The arguments of a method with an out buffer: the buffer's capacity, as the generated client
/// encodes it (a `u32`, little-endian).
fn cap_args(capacity: usize) -> [u8; 4] {
    u32::try_from(capacity).unwrap_or(u32::MAX).to_le_bytes()
}

/// The index of the device named `id` on the board.
fn index_of(host: &Host, id: &str) -> Option<u32> {
    host.devices()
        .into_iter()
        .find(|device| device.id == id)
        .map(|device| device.index)
}

/// The text after `key` in a JSON reply, up to the next quote or comma.
fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(key)? + key.len();
    let rest = &json[at..];
    let end = rest.find([',', '}', '"']).unwrap_or(rest.len());
    Some(&rest[..end])
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [disk, data, watchdog_s, max_run_s] = args.as_slice() else {
        eprintln!(
            "usage: lemnos-grow-host <disk node> <data dir> <watchdog timeout s> <max run s>"
        );
        return ExitCode::from(2);
    };
    let (Ok(watchdog_s), Ok(max_run_s)) = (watchdog_s.parse::<u32>(), max_run_s.parse::<u64>())
    else {
        eprintln!("grow-host: the timeouts must be whole seconds");
        return ExitCode::from(2);
    };
    let disk_name = Path::new(disk)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    let roots_dir = PathBuf::from("/run/grow-host");
    let _ = fs::remove_dir_all(&roots_dir);
    let roots = match stand_in_boot_chain(&roots_dir, &disk_name) {
        Ok(roots) => roots,
        Err(error) => {
            eprintln!("grow-host: the boot chain can't be stood in: {error}");
            return ExitCode::FAILURE;
        }
    };
    *TEST_ROOTS.lock().unwrap() = Some(roots);

    let board = match parse(&board_text(data, watchdog_s)) {
        Ok(board) => board,
        Err(error) => {
            eprintln!("grow-host: the board file: {error}");
            return ExitCode::FAILURE;
        }
    };
    let drivers = vec![
        entry::<lemnos_driver_system_slots::SystemSlots>(),
        entry::<lemnos_driver_hw_watchdog::HwWatchdog>(),
    ];
    let started = Instant::now();
    let host = match Host::start(
        &board,
        &drivers,
        Box::new(lemnos_linux::LinuxBoard::new()),
        Arc::new(Clock(started)),
        Options::default(),
    ) {
        Ok(host) => Arc::new(host),
        Err(error) => {
            eprintln!("grow-host: the host did not start: {error:?}");
            return ExitCode::FAILURE;
        }
    };
    host.set_liveness(Arc::new(Alive));
    let (Some(slots), Some(wdt)) = (index_of(&host, "slots"), index_of(&host, "wdt")) else {
        eprintln!("grow-host: a device is missing from the board");
        return ExitCode::FAILURE;
    };
    let (Some(info), Some(selftest)) = (
        method_of(&host, slots, "info"),
        method_of(&host, wdt, "tests"),
    ) else {
        eprintln!("grow-host: a method is missing from the contracts");
        return ExitCode::FAILURE;
    };
    println!("grow-host: started, watchdog {watchdog_s} s, disk {disk}, data {data}");
    for (name, device) in [("slots", slots), ("wdt", wdt)] {
        if let Some(manifest) = host.manifest(device) {
            for (contract, info) in manifest.provides.iter().enumerate() {
                let methods: Vec<String> = info
                    .methods
                    .iter()
                    .map(|m| format!("{}={}", m.name, m.id))
                    .collect();
                println!(
                    "grow-host: {name} contract {contract}: {}",
                    methods.join(" ")
                );
            }
        }
    }
    println!(
        "grow-host: calling info=({},{}) tests=({},{})",
        info.contract, info.method, selftest.contract, selftest.method
    );

    // The latency probe: a cheap method on another device, every 100 ms, through the host.
    let stop = Arc::new(AtomicBool::new(false));
    let probe = {
        let host = Arc::clone(&host);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let (mut calls, mut errors, mut max_latency, mut max_gap) =
                (0u64, 0u64, Duration::ZERO, Duration::ZERO);
            let mut last_done: Option<Instant> = None;
            let mut reply = vec![0u8; 16 * 1024];
            while !stop.load(Ordering::Acquire) {
                let t0 = Instant::now();
                match host.call(wdt, selftest, &cap_args(reply.len()), &mut reply) {
                    Ok(_) => {
                        let done = Instant::now();
                        max_latency = max_latency.max(done - t0);
                        if let Some(last) = last_done {
                            max_gap = max_gap.max(done - last);
                        }
                        last_done = Some(done);
                        calls += 1;
                    }
                    Err(error) => {
                        if errors == 0 {
                            eprintln!("grow-host: the first call on wdt failed: {error:?}");
                        }
                        errors += 1;
                    }
                }
                thread::sleep(Duration::from_millis(100).saturating_sub(t0.elapsed()));
            }
            (calls, errors, max_latency, max_gap)
        })
    };

    // The kill point and the progress: the slots driver's `info`, every 200 ms.
    let deadline = started + Duration::from_secs(max_run_s);
    let mut reply = vec![0u8; 64 * 1024];
    let mut last_state = String::new();
    let mut resizing_seen = false;
    let mut finished = false;
    while Instant::now() < deadline {
        match host.call(slots, info, &cap_args(reply.len()), &mut reply) {
            Err(error) => {
                if last_state.is_empty() {
                    println!("grow-host: info call failed: {error:?}");
                }
            }
            Ok(n) => {
                let json = String::from_utf8_lossy(&reply[..n]).into_owned();
                let state = field(&json, "\"grow_state\":\"").unwrap_or("").to_string();
                if state != last_state {
                    let t = started.elapsed().as_secs_f64();
                    let elapsed = field(&json, "\"grow_elapsed_ms\":").unwrap_or("-");
                    println!("grow-host: t={t:.3}s grow_state={state} grow_elapsed_ms={elapsed}");
                    last_state = state.clone();
                }
                if state == "resizing" && !resizing_seen {
                    resizing_seen = true;
                    println!(
                        "grow-host: resize started at t={:.3}s",
                        started.elapsed().as_secs_f64()
                    );
                }
                // `grow-too-large` is the named refusal: terminal, nothing else will change.
                if (state == "none"
                    && (resizing_seen || started.elapsed() > Duration::from_secs(3)))
                    || state == "grow-too-large"
                {
                    finished = true;
                    break;
                }
            }
        }
        thread::sleep(Duration::from_millis(200));
    }
    stop.store(true, Ordering::Release);
    let (calls, errors, max_latency, max_gap) = probe.join().unwrap_or_default();
    println!(
        "grow-host: latency probe (self-test on wdt, every 100 ms): calls={calls} errors={errors} max_latency_ms={:.1} max_gap_ms={:.1}",
        max_latency.as_secs_f64() * 1e3,
        max_gap.as_secs_f64() * 1e3
    );
    for device in host.devices() {
        println!("grow-host: device {device:?}");
    }
    let health = host
        .devices()
        .into_iter()
        .map(|device| format!("{}={:?}", device.id, device.health))
        .collect::<Vec<_>>()
        .join(" ");
    println!(
        "grow-host: finished={finished} resizing_seen={resizing_seen} last_state={last_state} health: {health}"
    );
    if finished {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(3)
    }
}
