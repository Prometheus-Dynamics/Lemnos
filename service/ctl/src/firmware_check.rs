//! `lemnos-ctl firmware-check IMAGE`: a boot partition image read as the Pi firmware reads it, one
//! verdict line per rule ([`lemnos_firmware_check`]). Read-only.
//!
//! The view is `config.txt` (a normal boot) unless `--trial` (`tryboot.txt`) or `--dir PREFIX`
//! (`config.txt` with `os_prefix` taken as `PREFIX`, so a directory is read as the default one would
//! be). `--dtb NAME` (one or more) names the device trees the board's firmware loads by default;
//! `--filter NAME` a model section filter that is on for the board; `--kernel NAME` the kernel names
//! tried in order (default `kernel_2712.img`, then `kernel8.img`); `--needs-overlay NAME` an overlay
//! the firmware applies by itself. The last line is `firmware-layout rule N: PASS` or `FAIL`.

use lemnos_firmware_check::{Filters, RULE_VERSION, Request, check};
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
        Ok(())
    } else {
        w(
            out,
            format!("firmware-layout rule {RULE_VERSION}: FAIL ({bad} BAD)"),
        )?;
        Err(format!("{image}: {bad} rule(s) failed"))
    }
}
