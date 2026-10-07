//! Checks board definitions against the built-in drivers; for generators'
//! CI (`cargo run -p lemnos-board --example validate -- board.toml ...`).
#![allow(clippy::print_stdout, clippy::print_stderr)]

use lemnos_board::{BoardDefinition, DriverRegistry};

fn main() {
    let registry = DriverRegistry::builtin();
    let mut failed = false;
    for path in std::env::args().skip(1) {
        match BoardDefinition::from_path(&path).and_then(|b| b.validate(&registry).map(|_| b)) {
            Ok(board) => println!("{path}: ok ({} devices)", board.devices.len()),
            Err(error) => {
                eprintln!("{path}: {error}");
                failed = true;
            }
        }
    }
    std::process::exit(i32::from(failed));
}
