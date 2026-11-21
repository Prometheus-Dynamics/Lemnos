//! `lemnos-ctl firmware-check IMAGE`: a boot partition image read as the Pi firmware reads it, one
//! verdict line per rule ([`lemnos_firmware_check`]). Read-only.
//!
//! The view is `config.txt` (a normal boot) unless `--trial` (`tryboot.txt`) or `--dir PREFIX`
//! (`config.txt` with `os_prefix` taken as `PREFIX`, so a directory is read as the default one would
//! be). `--dtb NAME` (one or more) names the device trees the board's firmware loads by default;
//! `--filter NAME` a model section filter that is on for the board; `--kernel NAME` the kernel names
//! tried in order (default `kernel_2712.img`, then `kernel8.img`); `--needs-overlay NAME` an overlay
//! the firmware applies by itself. The last line is `firmware-layout rule N: PASS` or `FAIL`.

use lemnos_firmware_check::{Filters, RULE_VERSION, Report, Request, check};
use lemnos_host::board::BootSpec;
use lemnos_slot_store::bootimage::Mem;
use std::io::Write;

/// The Pi 5 firmware's kernel names when `kernel=` is not set, in the order it tries them.
const DEFAULT_KERNELS: [&str; 2] = ["kernel_2712.img", "kernel8.img"];

/// Runs `firmware-check` with its arguments.
///
/// # Errors
///
/// A bad argument, an unreadable image, or any rule failing (after every line is printed).
pub fn run(args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let mut image = None;
    let mut req = Request {
        config: "config.txt".to_string(),
        prefix: None,
        filters: Filters::default(),
        kernels: Vec::new(),
        dtbs: Vec::new(),
        needs_overlays: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        if flag == "--trial" {
            req.config = "tryboot.txt".to_string();
            req.filters.trial = true;
            i += 1;
            continue;
        }
        if !flag.starts_with("--") {
            if image.replace(flag.to_string()).is_some() {
                return Err(format!(
                    "firmware-check takes one image; {flag:?} is a second"
                ));
            }
            i += 1;
            continue;
        }
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("{flag} needs a value"))?
            .clone();
        match flag {
            "--dir" => req.prefix = Some(value),
            "--dtb" => req.dtbs.push(value),
            "--filter" => req.filters.on.push(value),
            "--kernel" => req.kernels.push(value),
            "--needs-overlay" => req.needs_overlays.push(value),
            "--boot-count" => {
                req.filters.boot_count = value
                    .parse()
                    .map_err(|_| format!("--boot-count {value:?} is not a number"))?;
            }
            _ => return Err(format!("firmware-check: unknown option {flag}")),
        }
        i += 2;
    }
    if req.prefix.is_some() && req.filters.trial {
        return Err("--trial and --dir are two views: give one".to_string());
    }
    if req.kernels.is_empty() {
        req.kernels = DEFAULT_KERNELS.iter().map(|k| k.to_string()).collect();
    }
    let image = image.ok_or("firmware-check needs an IMAGE")?;
    let bytes = std::fs::read(&image).map_err(|e| format!("{image}: {e}"))?;
    let report = check(&mut Mem(bytes), &req);
    let bad = print(&req, &report, out)?;
    if bad == 0 {
        Ok(())
    } else {
        Err(format!("{image}: {bad} rule(s) failed"))
    }
}

/// The default view of a board's image as its firmware reads it: `config.txt`, the board's model
/// filters, its default device tree names and the overlays its firmware applies by itself.
pub(crate) fn board_request(boot: &BootSpec) -> Request {
    Request {
        config: "config.txt".to_string(),
        prefix: None,
        filters: Filters {
            on: boot.firmware_filters.clone(),
            trial: false,
            boot_count: 0,
        },
        kernels: DEFAULT_KERNELS.iter().map(|k| k.to_string()).collect(),
        dtbs: boot.device_tree_names.clone(),
        needs_overlays: boot.firmware_applied.clone(),
    }
}

/// Checks `image` with `req`: the number of rules that fail, every line printed.
pub(crate) fn check_image(
    image: Vec<u8>,
    req: &Request,
    out: &mut dyn Write,
) -> Result<usize, String> {
    let report = check(&mut Mem(image), req);
    print(req, &report, out)
}

/// Prints a report, one line per rule and the verdict last: the number of rules that fail.
fn print(req: &Request, report: &Report, out: &mut dyn Write) -> Result<usize, String> {
    let w = |out: &mut dyn Write, line: String| writeln!(out, "{line}").map_err(|e| e.to_string());
    w(
        out,
        format!("view {} (prefix {:?})", req.config, report.prefix),
    )?;
    for t in &report.settings.trace {
        w(out, format!("    {t}"))?;
    }
    for f in &report.findings {
        w(
            out,
            format!(
                "{} {}: {}",
                if f.ok { "ok " } else { "BAD" },
                f.rule,
                f.text
            ),
        )?;
    }
    let bad = report.findings.iter().filter(|f| !f.ok).count();
    if bad == 0 {
        w(out, format!("firmware-layout rule {RULE_VERSION}: PASS"))?;
    } else {
        w(
            out,
            format!("firmware-layout rule {RULE_VERSION}: FAIL ({bad} BAD)"),
        )?;
    }
    Ok(bad)
}
