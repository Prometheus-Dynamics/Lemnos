//! `lemnos-identity`: prints a board's identity facts as `name=value` lines, one per line, in the
//! order `docs/identity.md` lists them. For images with no Orion, and for scripts.
//!
//! ```text
//! lemnos-identity [--board FILE] [--data DIR]
//! ```
//!
//! `FILE` defaults to `/etc/lemnos/board.toml` and `DIR` (the data partition, for the slot facts)
//! to `/data`; `--data -` leaves the slot facts out. The exit status is 0 when `family` and
//! `serial` are both known, 1 when either is not (the reasons go to standard error), and 2 for a
//! bad command line or a board file that cannot be read.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use lemnos_identity::{gather, parse_board};

const DEFAULT_BOARD: &str = "/etc/lemnos/board.toml";
const DEFAULT_DATA: &str = "/data";

/// Writes one line to standard error. Diagnostics only: nothing a script reads.
fn say(line: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut board = PathBuf::from(DEFAULT_BOARD);
    let mut data = Some(PathBuf::from(DEFAULT_DATA));
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let value = rest.next();
        match (arg.as_str(), value) {
            ("--board", Some(path)) => board = PathBuf::from(path),
            ("--data", Some(path)) if path == "-" => data = None,
            ("--data", Some(path)) => data = Some(PathBuf::from(path)),
            _ => {
                say(format_args!(
                    "usage: lemnos-identity [--board FILE] [--data DIR|-]"
                ));
                return ExitCode::from(2);
            }
        }
    }
    let text = match std::fs::read_to_string(&board) {
        Ok(text) => text,
        Err(e) => {
            say(format_args!("cannot read {}: {e}", board.display()));
            return ExitCode::from(2);
        }
    };
    let file = match parse_board(&text) {
        Ok(file) => file,
        Err(e) => {
            say(format_args!("{}: {e}", board.display()));
            return ExitCode::from(2);
        }
    };
    let gathered = gather(&file, data.as_deref());
    let mut out = std::io::stdout().lock();
    for (name, value) in gathered.facts.iter() {
        if writeln!(out, "{name}={value}").is_err() {
            return ExitCode::from(2);
        }
    }
    for note in &gathered.notes {
        say(format_args!("note: {note}"));
    }
    let complete = gathered.facts.get("family").is_some() && gathered.facts.get("serial").is_some();
    if complete {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
