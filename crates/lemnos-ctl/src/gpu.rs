//! `lemnos-ctl gpu`: the advisory GPU lease (`docs/ipc.md`, section 6.6).
//! `gpu run` holds a lease while a command runs: the cooperative lock a
//! benchmark or a board test takes so that runs that also ask do not overlap.

use super::{Args, fail};
use lemnos_ipc::{ClientOptions, GpuAcquire, GpuHolder, GpuLeaseState, GpuMode};
use lemnos_linux_sys::process::Exit;
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "gpu [status] | gpu run [--shared] [--gpu NAME] [--wait S] [--max-hold S] [--label L] -- <command> [args...]";

/// `lemnos-ctl gpu ...`.
pub(super) fn gpu(mut args: Args, options: ClientOptions, priority: u8) -> ExitCode {
    match args.next().as_deref() {
        None | Some("status") => status(options),
        Some("run") => run(args, options, priority),
        Some(other) => fail(format!("{USAGE} (not {other:?})")),
    }
}

fn connect(options: ClientOptions) -> Result<lemnos_ipc::GpuClient, ExitCode> {
    let client = options.gpu().map_err(fail)?;
    if client.supported() {
        Ok(client)
    } else {
        Err(fail("this lemnosd has no GPU lease"))
    }
}

fn line(h: &GpuHolder, verb: &str) -> String {
    let limit = if h.max_hold_ms > 0 {
        format!(" of {:.1} s", f64::from(h.max_hold_ms) / 1000.0)
    } else {
        String::new()
    };
    format!(
        "  lease {} {} {} (client {}, {}, priority {}) {} {:.1} s{limit}{}",
        h.lease,
        h.mode.name(),
        h.client,
        h.client_id,
        h.label,
        h.priority,
        verb,
        h.for_ms as f64 / 1000.0,
        if h.overdue { ", overdue" } else { "" }
    )
}

fn status(options: ClientOptions) -> ExitCode {
    let mut client = match connect(options) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let gpus = match client.status() {
        Ok(g) => g,
        Err(e) => return fail(e),
    };
    if gpus.is_empty() {
        println!("no GPU (no render node under /sys/class/drm)");
    }
    for g in gpus {
        println!("{} ({}, {})", g.name, g.driver, g.node);
        if g.holders.is_empty() {
            println!("  free");
        }
        for h in &g.holders {
            println!("{}", line(h, "held"));
        }
        for h in &g.waiters {
            println!("{}", line(h, "waiting"));
        }
        if let Some(h) = &g.last_hang {
            println!(
                "  hangs {}, last {:.1} s ago by {} ({}): {}",
                g.hangs,
                h.ago_ms as f64 / 1000.0,
                h.client,
                h.label,
                h.detail
            );
        }
    }
    ExitCode::SUCCESS
}

fn seconds(text: Option<String>, default: f64) -> Result<f64, ExitCode> {
    match text {
        None => Ok(default),
        Some(t) => t
            .parse::<f64>()
            .ok()
            .filter(|s| s.is_finite() && *s >= 0.0)
            .ok_or_else(|| fail(format!("not a number of seconds: {t:?}"))),
    }
}

fn run(mut args: Args, options: ClientOptions, priority: u8) -> ExitCode {
    // The command after `--` is not ours to parse.
    let Some(split) = args.words.iter().position(|w| w == "--") else {
        return fail(USAGE);
    };
    let command: Vec<String> = args.words.drain(split..).skip(1).collect();
    if command.is_empty() {
        return fail(USAGE);
    }
    let mode = if args.flag("--shared") {
        GpuMode::Shared
    } else {
        GpuMode::Exclusive
    };
    let gpu = args.take("--gpu").unwrap_or_default();
    let label = args
        .take("--label")
        .unwrap_or_else(|| command[0].rsplit('/').next().unwrap_or("").to_string());
    let (wait, max_hold) = match (
        seconds(args.take("--wait"), 900.0),
        seconds(args.take("--max-hold"), 0.0),
    ) {
        (Ok(w), Ok(m)) => (w, m),
        (Err(code), _) | (_, Err(code)) => return code,
    };
    if let Some(extra) = args.next() {
        return fail(format!("{USAGE} (unexpected {extra:?})"));
    }
    let mut client = match connect(options) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let request = GpuAcquire {
        gpu,
        mode,
        priority,
        wait: true,
        max_hold_ms: (max_hold * 1000.0).min(f64::from(u32::MAX)) as u32,
        label,
    };
    let lease = match client.acquire(request, Some(Duration::from_secs_f64(wait))) {
        Ok(l) => l,
        Err(lemnos_ipc::ClientError::Timeout) => {
            return fail(format!("the GPU was not free within {wait} s"));
        }
        Err(e) => return fail(e),
    };
    let mut child = match lemnos_linux_sys::process::spawn(&command) {
        Ok(c) => c,
        Err(e) => return fail(format!("{}: {e}", command[0])),
    };
    // The lease is held while the connection is open; notices are reported.
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return fail(e),
        }
        match client.next_notice(Duration::from_millis(200)) {
            Ok(Some(n)) if n.lease == lease => match n.state {
                GpuLeaseState::YieldAsked => {
                    eprintln!("lemnos-ctl: gpu: a waiter of higher priority asks for the GPU");
                }
                GpuLeaseState::Overdue => {
                    eprintln!("lemnos-ctl: gpu: held past --max-hold");
                }
                _ => {}
            },
            Ok(_) => {}
            Err(e) => eprintln!("lemnos-ctl: gpu: {e} (the lease is gone)"),
        }
    };
    let _ = client.release(lease);
    match status {
        Exit::Code(0) => ExitCode::SUCCESS,
        Exit::Code(code) => ExitCode::from(code.clamp(1, 255) as u8),
        Exit::Signal(_) => ExitCode::FAILURE,
    }
}
