//! Resident memory of one Linux refresh: RSS and PSS from `/proc/self/smaps_rollup`
//! at each phase, for a bind policy chosen on the command line:
//!
//! - `none` (the default): the refresh binds nothing (`RuntimeBindPolicy::new`).
//! - `all`: the second refresh binds every discovered device the registered
//!   drivers support (`RuntimeBindPolicy::all`).
//! - `all-except`: the second refresh binds every device whose id is not in
//!   `--except a,b,...`. The policy has no exclusions, so this binds each
//!   remaining device by id after the refresh, the same drivers and state.
//!
//! The phases: start, facade built, the first refresh without binds, and (for
//! any other policy) the refresh with its binds. Each line is the phase's RSS
//! and PSS in kB; `--list` prints the discovered device ids and exits.
#![allow(clippy::print_stdout)]

use lemnos::core::DeviceId;
use lemnos::linux::LinuxBackend;
use lemnos::prelude::*;
use std::env;
use std::error::Error;
use std::fs;
use std::time::Instant;

fn memory(label: &str) {
    let rollup = fs::read_to_string("/proc/self/smaps_rollup").unwrap_or_default();
    let field = |key: &str| {
        rollup
            .lines()
            .find(|line| line.starts_with(key))
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("?")
            .to_owned()
    };
    println!("{label}: rss_kb={} pss_kb={}", field("Rss:"), field("Pss:"));
}

/// The policy named on the command line, and the ids it excludes.
struct Options {
    policy: String,
    except: Vec<String>,
    list: bool,
}

fn options() -> Options {
    let mut options = Options {
        policy: "none".into(),
        except: Vec::new(),
        list: false,
    };
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--list" => options.list = true,
            "--except" => {
                options.except = args
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            policy => options.policy = policy.to_owned(),
        }
    }
    options
}

fn main() -> Result<(), Box<dyn Error>> {
    let options = options();
    memory("start");
    let backend = LinuxBackend::new();
    let mut lemnos = Lemnos::builder()
        .with_linux_backend_ref(&backend)
        .with_builtin_drivers()?
        .build();
    memory("built");
    if options.list {
        // A refresh without binds, then the ids and interfaces it found.
        lemnos.refresh_with_linux_default(&backend)?;
        for device in lemnos.inventory().iter() {
            println!(
                "{} {:?} {:?}",
                device.id.as_str(),
                device.interface,
                device.kind
            );
        }
        return Ok(());
    }
    let started = Instant::now();
    let report = lemnos.refresh_with_linux_default(&backend)?;
    println!("refresh_ms={} policy=none", started.elapsed().as_millis());
    println!("report: {report:?}");
    memory("after refresh without binds");
    match options.policy.as_str() {
        "none" => return Ok(()),
        "all" => lemnos.set_bind_policy(RuntimeBindPolicy::all()),
        "all-except" => lemnos.set_bind_policy(RuntimeBindPolicy::new()),
        other => return Err(format!("unknown bind policy {other}").into()),
    }
    let started = Instant::now();
    let report = lemnos.refresh_with_linux_default(&backend)?;
    let mut bound = 0usize;
    let mut failed = 0usize;
    if options.policy == "all-except" {
        let ids: Vec<DeviceId> = lemnos
            .inventory()
            .iter()
            .map(|device| device.id.clone())
            .filter(|id| !options.except.iter().any(|e| e == id.as_str()))
            .collect();
        for id in &ids {
            match lemnos.bind(id) {
                Ok(()) => bound += 1,
                Err(_) => failed += 1,
            }
        }
    }
    println!(
        "refresh_ms={} policy={} except={:?} bound={bound} failed={failed}",
        started.elapsed().as_millis(),
        options.policy,
        options.except
    );
    println!("report: {report:?}");
    memory(&format!("after refresh with {} binds", options.policy));
    Ok(())
}
