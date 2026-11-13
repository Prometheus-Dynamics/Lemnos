//! `lemnos-orion`: publishes every device lemnosd serves to Orion.

use std::process::ExitCode;
use std::sync::{Arc, mpsc};

use lemnos_orion::config::Config;
use lemnos_orion::{Shared, calls, log, publish, source};

fn main() -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            log(&error);
            return ExitCode::FAILURE;
        }
    };
    let shared = match Shared::new() {
        Ok(shared) => Arc::new(shared),
        Err(error) => {
            log(&format!("cannot make the wake-up socket: {error}"));
            return ExitCode::FAILURE;
        }
    };
    let (jobs, queue) = mpsc::channel();
    let threads = [
        {
            let socket = config.lemnosd_socket.clone();
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("lemnosd".to_owned())
                .spawn(move || source::run(&socket, &shared))
        },
        {
            let socket = config.lemnosd_socket.clone();
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("calls".to_owned())
                .spawn(move || calls::run(&socket, &queue, &shared))
        },
    ];
    for thread in threads {
        if let Err(error) = thread {
            log(&format!("cannot start a thread: {error}"));
            return ExitCode::FAILURE;
        }
    }
    publish::run(&config, &shared, &jobs);
    ExitCode::SUCCESS
}
