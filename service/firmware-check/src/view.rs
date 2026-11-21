//! One view of a boot partition, checked rule by rule.

use crate::config::{self, Filters, Settings, Source};
use crate::{CMDLINE_MAX, fdt, stream};
use lemnos_slot_store::bootfat::{self, ReadAt, Refusal};

/// The largest file the check reads.
const READ_LIMIT: u64 = 256 << 20;

/// What to check: which firmware text starts the view, and what the board's firmware loads.
#[derive(Debug, Clone)]
pub struct Request {
    /// The firmware text the view starts from: `config.txt` (a normal boot) or `tryboot.txt`.
    pub config: String,
    /// `Some(dir)`: `os_prefix` is taken as `dir` whatever the text says (a named directory read
    /// as the default one would be).
    pub prefix: Option<String>,
    /// The section filters that are on.
    pub filters: Filters,
    /// The kernel names the firmware tries, in order, when `kernel=` is not set.
    pub kernels: Vec<String>,
    /// The device tree names the board's firmware loads by default (each must be there).
    pub dtbs: Vec<String>,
    /// Overlays the firmware applies by itself (such as a stepping overlay): each must be there and
    /// exact, and apply.
    pub needs_overlays: Vec<String>,
}

/// One verdict line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Whether the rule holds.
    pub ok: bool,
    /// The rule's name.
    pub rule: &'static str,
    /// What it found.
    pub text: String,
}

/// A view checked.
#[derive(Debug, Clone)]
pub struct Report {
    /// The prefix the view resolved to.
    pub prefix: String,
    /// What the firmware text set.
    pub settings: Settings,
    /// The verdicts, in rule order.
    pub findings: Vec<Finding>,
}

impl Report {
    /// Whether every rule holds.
    pub fn pass(&self) -> bool {
        self.findings.iter().all(|f| f.ok)
    }
}

struct Fat<'a, D: ReadAt>(&'a mut D);

impl<D: ReadAt> Source for Fat<'_, D> {
    fn fetch(&mut self, path: &str) -> Option<Vec<u8>> {
        bootfat::read_file(self.0, path, READ_LIMIT).ok()
    }
}

struct Out(Vec<Finding>);

impl Out {
    fn push(&mut self, ok: bool, rule: &'static str, text: impl Into<String>) {
        self.0.push(Finding {
            ok,
            rule,
            text: text.into(),
        });
    }
}

/// Checks one view of the boot partition on `dev`, reading only.
pub fn check<D: ReadAt>(dev: &mut D, req: &Request) -> Report {
    let mut settings = Settings::default();
    config::read(&mut Fat(dev), &req.config, &req.filters, &mut settings, 0);
    let prefix = req
        .prefix
        .clone()
        .or_else(|| settings.get("os_prefix").map(str::to_string))
        .unwrap_or_default();
    let mut out = Out(Vec::new());

    // Default names.
    for key in ["kernel", "cmdline", "device_tree"] {
        match settings.get(key) {
            Some(v) => out.push(
                false,
                "names",
                format!("{key}={v} is set: a bay is read under the firmware's default names only"),
            ),
            None => out.push(true, "names", format!("{key}= not set")),
        }
    }

    let mut loaded: Vec<String> = Vec::new();

    // Kernel.
    let kernel_names: Vec<String> = match settings.get("kernel") {
        Some(k) => vec![k.to_string()],
        None => req.kernels.clone(),
    };
    let mut kernel = None;
    for k in &kernel_names {
        let path = format!("{prefix}{k}");
        if let Some(d) = Fat(dev).fetch(&path) {
            kernel = Some((path, d));
            break;
        }
    }
    match kernel {
        None => out.push(
            false,
            "kernel",
            format!("none of {} under {prefix:?}", kernel_names.join(", ")),
        ),
        Some((path, d)) => {
            check_kernel(&mut out, &path, &d);
            loaded.push(path);
        }
    }

    // Device trees.
    let mut trees: Vec<(String, fdt::Tree)> = Vec::new();
    let dtb_names: Vec<String> = match settings.get("device_tree") {
        Some(t) => vec![t.to_string()],
        None => req.dtbs.clone(),
    };
    if dtb_names.is_empty() {
        out.push(
            false,
            "dtb",
            "no default device tree name given for this board",
        );
    }
    for name in &dtb_names {
        let path = format!("{prefix}{name}");
        let Some(d) = Fat(dev).fetch(&path) else {
            out.push(false, "dtb", format!("{path}: not there"));
            continue;
        };
        loaded.push(path.clone());
        match fdt::read(&d) {
            Err(e) => out.push(false, "dtb", format!("{path}: {e}")),
            Ok(t) => {
                exact(&mut out, "dtb", &path, t.total, d.len());
                let n = t.names("/__symbols__").len();
                out.push(
                    n > 0,
                    "dtb",
                    if n > 0 {
                        format!("{path}: __symbols__ with {n} labels")
                    } else {
                        format!("{path}: no __symbols__: overlays cannot resolve labels on it")
                    },
                );
                trees.push((path, t));
            }
        }
    }

    // Overlays.
    let readme = format!("{prefix}overlays/README");
    let own = Fat(dev).fetch(&readme).is_some();
    out.push(
        own,
        "overlays",
        if own {
            format!("{readme} there: overlays load from the prefix")
        } else {
            format!("{readme} not there: overlays would load from the shared overlays/")
        },
    );
    if own {
        loaded.push(readme);
        // The directory itself is looked up by name too.
        loaded.push(format!("{prefix}overlays"));
    }
    let base = if own {
        format!("{prefix}overlays/")
    } else {
        "overlays/".to_string()
    };
    for map in ["overlay_map.dtb", "hat_map.dtb"] {
        let path = format!("{base}{map}");
        match Fat(dev).fetch(&path) {
            None => out.push(false, "overlays", format!("{path}: not there")),
            Some(d) => {
                match fdt::read(&d) {
                    Ok(t) => exact(&mut out, "overlays", &path, t.total, d.len()),
                    Err(e) => out.push(false, "overlays", format!("{path}: {e}")),
                }
                loaded.push(path);
            }
        }
    }
    let mut names: Vec<String> = req.needs_overlays.clone();
    for o in &settings.overlays {
        let n = o.split(',').next().unwrap_or("").trim().to_string();
        if !n.is_empty() && !names.contains(&n) {
            names.push(n);
        }
    }
    for name in &names {
        let path = format!("{base}{name}.dtbo");
        let Some(d) = Fat(dev).fetch(&path) else {
            out.push(false, "overlays", format!("{path}: not there"));
            continue;
        };
        loaded.push(path.clone());
        let ov = match fdt::read(&d) {
            Ok(t) => t,
            Err(e) => {
                out.push(false, "overlays", format!("{path}: {e}"));
                continue;
            }
        };
        exact(&mut out, "overlays", &path, ov.total, d.len());
        for (tpath, tree) in &trees {
            let misses = fdt::overlay_misses(&ov, tree);
            out.push(
                misses.is_empty(),
                "overlays",
                if misses.is_empty() {
                    format!("{path} applies to {tpath}")
                } else {
                    format!("{path} does not apply to {tpath}: {}", misses.join("; "))
                },
            );
        }
    }

    // Command line.
    let cmd = format!(
        "{prefix}{}",
        settings.get("cmdline").unwrap_or("cmdline.txt")
    );
    match Fat(dev).fetch(&cmd) {
        None => out.push(false, "cmdline", format!("{cmd}: not there")),
        Some(d) => {
            loaded.push(cmd.clone());
            check_cmdline(&mut out, &cmd, &d);
        }
    }

    // Initramfs.
    if let Some(name) = &settings.initramfs {
        let path = format!("{prefix}{name}");
        match Fat(dev).fetch(&path) {
            None => out.push(false, "initramfs", format!("{path}: not there")),
            Some(d) => {
                loaded.push(path.clone());
                check_initramfs(&mut out, &path, &d);
            }
        }
    } else {
        out.push(true, "initramfs", "no initramfs line: none loaded");
    }

    // Case of every name loaded.
    for path in &loaded {
        check_case(dev, &mut out, path);
    }

    Report {
        prefix,
        settings,
        findings: out.0,
    }
}

fn exact(out: &mut Out, rule: &'static str, path: &str, total: usize, len: usize) {
    out.push(
        total == len,
        rule,
        if total == len {
            format!("{path}: totalsize {total} is the file size")
        } else {
            format!(
                "{path}: totalsize {total}, file {len}: {} bytes after the tree",
                len.saturating_sub(total)
            )
        },
    );
}

fn check_kernel(out: &mut Out, path: &str, d: &[u8]) {
    let arm64 = |b: &[u8]| b.get(0x38..0x3c) == Some(b"ARMd".as_slice());
    if d.starts_with(&[0x1f, 0x8b]) {
        match stream::gzip(d) {
            Err(e) => out.push(false, "kernel", format!("{path}: gzip, {e}")),
            Ok(g) => {
                out.push(
                    g.end == d.len(),
                    "kernel",
                    if g.end == d.len() {
                        format!(
                            "{path}: one gzip member ending at the end of the file ({} bytes)",
                            d.len()
                        )
                    } else {
                        format!(
                            "{path}: the gzip stream ends at byte {} of {}: {} bytes after it",
                            g.end,
                            d.len(),
                            d.len() - g.end
                        )
                    },
                );
                let real = (g.plain_len & 0xFFFF_FFFF) as u32;
                let last4 = u32::from_le_bytes([
                    d[d.len() - 4],
                    d[d.len() - 3],
                    d[d.len() - 2],
                    d[d.len() - 1],
                ]);
                out.push(
                    last4 == real && g.isize == real,
                    "kernel",
                    format!(
                        "{path}: decompressed {} bytes; the trailer's ISIZE {}; the file's last 4 bytes read {last4}",
                        g.plain_len, g.isize
                    ),
                );
                out.push(
                    g.crc_ok,
                    "kernel",
                    format!(
                        "{path}: the trailer's CRC-32 {}",
                        if g.crc_ok {
                            "matches"
                        } else {
                            "does not match"
                        }
                    ),
                );
                out.push(
                    arm64(&g.head),
                    "kernel",
                    format!(
                        "{path}: arm64 Image magic {}",
                        if arm64(&g.head) {
                            "after decompression"
                        } else {
                            "missing after decompression"
                        }
                    ),
                );
            }
        }
    } else {
        out.push(
            arm64(d),
            "kernel",
            format!(
                "{path}: uncompressed, {} bytes; arm64 Image magic {}",
                d.len(),
                if arm64(d) { "there" } else { "missing" }
            ),
        );
    }
}

fn check_cmdline(out: &mut Out, path: &str, d: &[u8]) {
    let body = d.strip_suffix(b"\n").unwrap_or(d);
    let lines_ok = !body.contains(&b'\n') && !body.contains(&b'\r') && !body.contains(&0);
    let trailing = body.len() - body.trim_ascii_end().len();
    out.push(
        lines_ok && trailing == 0 && !body.is_empty(),
        "cmdline",
        format!(
            "{path}: {} bytes; {}{}",
            d.len(),
            if lines_ok {
                "one line"
            } else {
                "more than one line, or NUL bytes"
            },
            if trailing > 0 {
                format!(", {trailing} blank bytes at its end")
            } else {
                String::new()
            }
        ),
    );
    out.push(
        d.len() <= CMDLINE_MAX,
        "cmdline",
        format!("{path}: {} bytes, at most {CMDLINE_MAX}", d.len()),
    );
}

fn check_initramfs(out: &mut Out, path: &str, d: &[u8]) {
    let (form, end) = match d.get(..6) {
        Some([0x1f, 0x8b, ..]) => ("gzip", stream::gzip(d).map(|g| g.end)),
        Some([0x28, 0xb5, 0x2f, 0xfd, ..]) => ("zstd", stream::zstd_end(d)),
        Some([0xfd, b'7', b'z', b'X', b'Z', 0]) => (
            "xz",
            if stream::xz_ends(d) {
                Ok(d.len())
            } else {
                Err("the file does not end with the xz footer magic".to_string())
            },
        ),
        Some(m) if m.starts_with(b"07070") => ("plain cpio", stream::cpio_end(d)),
        _ => {
            out.push(
                false,
                "initramfs",
                format!(
                    "{path}: not cpio, gzip, zstd or xz (starts {:02x?})",
                    &d[..d.len().min(6)]
                ),
            );
            return;
        }
    };
    match end {
        Err(e) => out.push(false, "initramfs", format!("{path}: {form}: {e}")),
        Ok(end) => {
            let after = d.len().saturating_sub(end);
            // A cpio tool pads its archive to a 512-byte block with zeros, which the kernel skips.
            let ok = after == 0
                || (form == "plain cpio" && after < 512 && d[end..].iter().all(|&b| b == 0));
            out.push(
                ok,
                "initramfs",
                if after == 0 {
                    format!(
                        "{path}: {form} ending at the end of the file ({} bytes)",
                        d.len()
                    )
                } else {
                    format!(
                        "{path}: {form} ends at byte {end} of {}: {after} bytes after it",
                        d.len()
                    )
                },
            );
        }
    }
}

fn check_case<D: ReadAt>(dev: &mut D, out: &mut Out, path: &str) {
    let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
    let entries = match bootfat::list_dir(dev, dir) {
        Ok(e) => e,
        Err(e) => {
            out.push(
                false,
                "case",
                format!("{path}: its directory: {}", refusal(&e)),
            );
            return;
        }
    };
    match entries.iter().find(|e| e.is_named(name)) {
        None => out.push(false, "case", format!("{path}: no entry")),
        Some(e) if e.dir => {
            let shown = e.shown();
            out.push(
                shown == name,
                "case",
                format!("{path}/: directory stored as {shown}"),
            );
        }
        Some(e) => {
            let shown = e.shown();
            out.push(
                shown == name,
                "case",
                if shown == name {
                    format!("{path}: stored as {shown}")
                } else {
                    format!("{path}: stored as {shown}, not {name}")
                },
            );
        }
    }
}

fn refusal(e: &Refusal) -> String {
    e.to_string()
}
