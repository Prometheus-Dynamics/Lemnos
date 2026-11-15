#![allow(clippy::print_stdout, clippy::print_stderr)]

//! The guest's probe for the crash-log proof (`testing/qemu/run-crash.sh`): one boot's history
//! record, through the driver's own `crash::record`, on the guest's state directory and pstore.
//!
//!   crash_probe <data dir> <boot dir> [pstore dir | --no-pstore] [tail bytes]
//!
//! The boot id is the kernel's. Prints the record step's words, and the index row it wrote.

use std::process::ExitCode;

use lemnos_driver_system_slots::crash::{self, FsPstore, Pstore, Start};
use lemnos_slot_store::store::Layout;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(data), Some(boot_dir)) = (args.first(), args.get(1)) else {
        eprintln!(
            "usage: crash_probe <data dir> <boot dir> [pstore dir | --no-pstore] [tail bytes]"
        );
        return ExitCode::from(2);
    };
    let boot = match std::fs::read_to_string("/proc/sys/kernel/random/boot_id") {
        Ok(text) => text.trim().to_string(),
        Err(err) => {
            eprintln!("crash_probe: no boot id: {err}");
            return ExitCode::FAILURE;
        }
    };
    let pstore = match args.get(2).map(String::as_str) {
        Some("--no-pstore") => None,
        Some(dir) => Some(dir.to_string()),
        None => Some("/sys/fs/pstore".to_string()),
    };
    let bytes = args
        .get(3)
        .and_then(|text| text.parse().ok())
        .unwrap_or(crash::DEFAULT_BYTES);
    let layout = Layout::new(data, boot_dir);
    let source = pstore.map(FsPstore::new);
    let start = Start {
        boot: &boot,
        slot: "-",
        outcome: "unknown",
        reset: "none",
        requested: "-",
        keep: 16,
    };
    let recorded = crash::record(
        &layout,
        start,
        source
            .as_ref()
            .map(|source| (source as &dyn crash::Pstore, bytes)),
    );
    if recorded.note.is_empty() {
        println!("crash_probe: boot {boot}: no crash log declared");
    } else {
        println!("crash_probe: boot {boot}: {}", recorded.note);
    }
    println!("crash_probe: history appended: {}", recorded.appended);
    if let Some(source) = &source {
        let left = source.records().map(|records| records.len()).unwrap_or(0);
        println!("crash_probe: pstore records now: {left}");
    }
    for row in std::fs::read_to_string(layout.marker(crash::INDEX))
        .unwrap_or_default()
        .lines()
    {
        println!("crash_probe: index: {row}");
    }
    if recorded.appended {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
