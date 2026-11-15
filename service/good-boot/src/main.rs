//! `lemnos-good-boot --deadline-s SECONDS [--data DIR]`: exits 0 when the boot is good within the
//! deadline, 1 when it is not (the unit then fails and the board restarts).

use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

use lemnos_good_boot::{Monotonic, Outcome, System, wait};
use lemnos_slot_store::goodboot::{MAX_DEADLINE_S, MIN_DEADLINE_S};

const USAGE: &str = "usage: lemnos-good-boot --deadline-s SECONDS [--data DIR]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(line) => {
            let _ = writeln!(std::io::stdout(), "{line}");
            ExitCode::SUCCESS
        }
        Err(why) => {
            let _ = writeln!(std::io::stderr(), "lemnos-good-boot: {why}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<String, String> {
    let mut deadline = None;
    let mut data = "/data".to_string();
    let mut words = args.iter();
    while let Some(word) = words.next() {
        match (word.as_str(), words.next()) {
            ("--deadline-s", Some(value)) => deadline = Some(value.clone()),
            ("--data", Some(dir)) => data = dir.clone(),
            _ => return Err(USAGE.to_string()),
        }
    }
    let deadline = deadline.ok_or_else(|| USAGE.to_string())?;
    let seconds: u32 = deadline.parse().ok().filter(|s| (MIN_DEADLINE_S..=MAX_DEADLINE_S).contains(s)).ok_or_else(|| {
        format!("--deadline-s is {deadline}: it must be a whole number of seconds from {MIN_DEADLINE_S} to {MAX_DEADLINE_S}")
    })?;
    let mut probe = System { data: data.into() };
    let mut clock = Monotonic::default();
    match wait(
        &mut probe,
        &mut clock,
        Duration::from_secs(u64::from(seconds)),
    ) {
        Outcome::Good { slot, after } => Ok(format!(
            "good boot on slot {slot} after {} s",
            after.as_secs()
        )),
        Outcome::Late { ready, marker } => Err(format!(
            "no good boot within {seconds} s: lemnosd ready: {}; booted marker: {}; the board is restarted",
            if ready { "yes" } else { "no" },
            if marker.is_empty() {
                "none".to_string()
            } else {
                format!("{marker:?}")
            },
        )),
    }
}
