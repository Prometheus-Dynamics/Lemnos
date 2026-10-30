//! Resident memory of one Linux refresh: RSS and PSS from `/proc/self/smaps_rollup`,
//! before and after `refresh_with_linux_default`. `eager` binds every discovered
//! device the registered drivers support (`RuntimeBindPolicy::all`) before the refresh.
#![allow(clippy::print_stdout)]

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

fn main() -> Result<(), Box<dyn Error>> {
    let eager = env::args().any(|arg| arg == "eager");
    memory("start");
    let backend = LinuxBackend::new();
    let mut lemnos = Lemnos::builder()
        .with_linux_backend_ref(&backend)
        .with_builtin_drivers()?
        .build();
    memory("built");
    if eager {
        lemnos.set_bind_policy(RuntimeBindPolicy::all());
    }
    let started = Instant::now();
    let report = lemnos.refresh_with_linux_default(&backend)?;
    println!("refresh_ms={} eager={eager}", started.elapsed().as_millis());
    println!("report: {report:?}");
    memory(if eager { "after eager refresh" } else { "after refresh" });
    Ok(())
}
