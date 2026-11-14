//! The off-board daemon: the real `lemnosd` service over the simulated camera chain
//! ([`lemnos_camera_sim`]).
//!
//! usage: offboard-daemon --board FILE --socket PATH [--claims on|off] [--model B1|A|M0] [--check]

use std::io::Write;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, PoisonError};

use lemnos_camera_sim::{Board, Model, SimBuffers, WORLD, drivers, new_world, world};
use lemnos_host::Options;
use lemnos_linux_sys::poll::{POLLIN, poll_one};
use lemnos_linux_sys::signal::{SIGINT, SIGTERM, SignalFd};
use lemnos_sim::SimBoard;
use lemnosd::{Config as ServiceConfig, Service};

fn say(text: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stderr(), "{text}");
}

fn complain(text: &str) -> ExitCode {
    say(format_args!("offboard-daemon: {text}"));
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let (mut board, mut socket, mut model) = (String::new(), String::new(), Model::B1);
    // The kernel module's per-address claim is the board's behaviour: on unless a run says otherwise.
    let mut claims = std::env::var("OFFBOARD_CLAIMS").map_or(true, |v| v != "off");
    let mut check = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--board" => board = args.next().unwrap_or_default(),
            "--socket" => socket = args.next().unwrap_or_default(),
            "--claims" => claims = args.next().as_deref() == Some("on"),
            "--check" => check = true,
            "--model" => {
                model = match args.next().as_deref() {
                    Some("M0") => Model::M0,
                    Some("A") => Model::A,
                    _ => Model::B1,
                }
            }
            _ => {
                return complain(
                    "usage: offboard-daemon --board FILE --socket PATH [--claims on|off] [--model B1|A|M0]",
                );
            }
        }
    }
    let text = match std::fs::read_to_string(&board) {
        Ok(t) => t,
        Err(e) => return complain(&format!("{board}: {e}")),
    };
    if check {
        // As `lemnosd --check`: the board file against EVERY driver compiled into the real daemon.
        return match lemnosd::check(&text, &lemnosd::builtin::drivers()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(reason) => complain(&format!("{board}: {reason}")),
        };
    }
    let board_file = match lemnos_host::board::parse_lenient(&text) {
        Ok(b) => b,
        Err(e) => return complain(&format!("{board}: {e}")),
    };
    *WORLD.lock().unwrap_or_else(PoisonError::into_inner) = Some(new_world(claims, model));
    // The sensor stays powered between daemon starts on the board, so its registers persist: a state file (OFFBOARD_STATE) carries
    // the model's registers from one daemon run to the next, as the chip would.
    let state = std::env::var_os("OFFBOARD_STATE").map(PathBuf::from);
    if let Some(path) = &state
        && let Ok(text) = std::fs::read_to_string(path)
    {
        world(|w| {
            let regs = &mut w
                .buses
                .get_mut("i2c-10")
                .expect("bus")
                .targets
                .get_mut(&0x60)
                .expect("target")
                .regs;
            for line in text.lines() {
                let mut it = line.split_whitespace();
                if let (Some(r), Some(v)) = (it.next(), it.next())
                    && let (Ok(r), Ok(v)) = (u16::from_str_radix(r, 16), u8::from_str_radix(v, 16))
                {
                    regs.insert(r, v);
                }
            }
            w.streaming = regs.get(&0x0100).is_some_and(|v| v & 1 == 1);
        });
    }
    let lines = SimBoard::new();
    lines.add_line("gpio:pinctrl-rp1:34");
    let signals = match SignalFd::new(&[SIGTERM, SIGINT]) {
        Ok(s) => s,
        Err(e) => return complain(&format!("signals: {e}")),
    };
    let config = ServiceConfig {
        socket: PathBuf::from(&socket),
        name: board_file.board.id.clone(),
        board: board_file,
        drivers: drivers(),
        backend: Box::new(Board { lines }),
        options: Options::default(),
    };
    if !lemnos_camera_sim::pisp::install() {
        return complain("a simulated block was installed already");
    }
    let buffers = SimBuffers::probe();
    say(format_args!(
        "offboard-daemon: stream buffers are {}",
        buffers.kind()
    ));
    let service = match Service::start_with_buffers(config, Arc::new(buffers)) {
        Ok(s) => s,
        Err(e) => return complain(&e.to_string()),
    };
    loop {
        if poll_one(signals.as_fd(), POLLIN, None).is_err() || matches!(signals.read(), Ok(Some(_)))
        {
            break;
        }
    }
    service.stop();
    if let Some(path) = &state {
        let text = world(|w| {
            let regs = &w.buses["i2c-10"].targets[&0x60].regs;
            regs.iter()
                .map(|(r, v)| format!("{r:04x} {v:02x}\n"))
                .collect::<String>()
        });
        let _ = std::fs::write(path, text);
    }
    ExitCode::SUCCESS
}
