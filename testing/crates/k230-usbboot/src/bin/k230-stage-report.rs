//! The host's command for Stage L: one attached process from the identify to the return.
//!
//! ```text
//! k230-stage-report --port 3-4.2.1.4.1 --image <stage.bin> --record <run.txt> \
//!     [--sd-controller 0|1] \
//!     [--console /dev/serial/by-id/usb-1a86_USB_Dual_Serial_5B08068863-if00] \
//!     [--console /dev/serial/by-id/usb-1a86_USB_Dual_Serial_5B08068863-if02] \
//!     [--reference <other-board pad table>]
//! ```
//!
//! `--port` is required: nothing is selected by VID:PID alone. The device there must be `29f1:0230`. The consoles
//! are opened READ-ONLY and never written. The record is written to `--record` (keep it on NVMe, not the Github
//! HDD). `--reference` is printed in the record beside the pad dump, marked as another board model, for comparison.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use lemnos_k230_usbboot::console::Console;
use lemnos_k230_usbboot::stage_report::{Options, RealTimer, run};
use lemnos_k230_usbboot::usb::RusbIo;
use sha2::{Digest, Sha256};

fn usage(message: &str) -> ! {
    eprintln!("k230-stage-report: {message}");
    eprintln!("usage: k230-stage-report --port BUS-PORT.PORT --image FILE --record FILE [--sd-controller 0|1] [--console DEV]... [--reference FILE]");
    std::process::exit(2);
}

fn main() {
    let mut port: Option<String> = None;
    let mut image: Option<PathBuf> = None;
    let mut record: Option<PathBuf> = None;
    let mut sd: Option<u32> = None;
    let mut consoles: Vec<String> = Vec::new();
    let mut reference: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage(&format!("{arg} needs a value")));
        match arg.as_str() {
            "--port" => port = Some(value()),
            "--image" => image = Some(PathBuf::from(value())),
            "--record" => record = Some(PathBuf::from(value())),
            "--sd-controller" => {
                let v = value();
                sd = Some(v.parse().unwrap_or_else(|_| usage("--sd-controller is 0 or 1")));
            }
            "--console" => consoles.push(value()),
            "--reference" => reference = Some(PathBuf::from(value())),
            other => usage(&format!("unknown argument {other}")),
        }
    }
    let port = port.unwrap_or_else(|| usage("--port is required (a port path, never VID:PID alone)"));
    let image_path = image.unwrap_or_else(|| usage("--image is required"));
    let record_path = record.unwrap_or_else(|| usage("--record is required"));

    let bytes = std::fs::read(&image_path).unwrap_or_else(|e| usage(&format!("reading the image: {e}")));
    let digest: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();

    // The consoles, read-only, from before the run's first transfer.
    let started = Instant::now();
    let mut opened: Vec<(String, Console)> = Vec::new();
    for dev in &consoles {
        match Console::open_readonly(dev) {
            Ok(c) => opened.push((dev.clone(), c)),
            Err(e) => eprintln!("console {dev}: {e} (recorded as absent)"),
        }
    }

    let mut io = match RusbIo::open_port(Some(&port)) {
        Ok(io) => io,
        Err(e) => usage(&format!("opening the device at {port}: {e}")),
    };
    let opts = Options {
        image: bytes,
        sd_controller: sd,
        identify_wait: Duration::from_secs(10),
        return_wait: Duration::from_secs(300),
    };
    let rec = run(&mut io, &mut RealTimer(started), &opts);

    let mut text = String::new();
    text.push_str(&format!("port {port}\nimage {} sha256 {digest}\n", image_path.display()));
    text.push_str(&format!("consoles read-only: {:?}\n", consoles));
    match &rec {
        Ok(r) => {
            let mut console_lines = Vec::new();
            for (_, c) in &opened {
                for line in c.drain() {
                    console_lines.push((line.at.saturating_duration_since(started).as_secs_f64(), line.text));
                }
            }
            console_lines.sort_by(|a, b| a.0.total_cmp(&b.0));
            text.push_str(&r.render(&console_lines));
        }
        Err(e) => text.push_str(&format!("the flow stopped: {e}\n")),
    }
    if let Some(path) = reference {
        match std::fs::read_to_string(&path) {
            Ok(table) => {
                text.push_str("--- other board model, for comparison only (not this board)\n");
                text.push_str(&table);
            }
            Err(e) => text.push_str(&format!("reference {}: {e}\n", path.display())),
        }
    }
    if let Err(e) = std::fs::write(&record_path, &text) {
        eprintln!("writing the record: {e}");
        std::process::exit(1);
    }
    println!("record written: {}", record_path.display());
    if let Err(e) = rec {
        eprintln!("the flow stopped: {e}");
        std::process::exit(1);
    }
}
