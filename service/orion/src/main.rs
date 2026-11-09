//! `lemnos-orion`: publishes every device lemnosd serves to Orion.

use std::process::ExitCode;
use std::sync::Arc;

use lemnos_orion::config::Config;
use lemnos_orion::{Shared, log, publish, source};

fn main() -> ExitCode {
    let config = Config::from_env();
    let shared = Arc::new(Shared::new());
    let lemnosd = {
        let socket = config.lemnosd_socket.clone();
        let shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("lemnosd".to_owned())
            .spawn(move || source::run(&socket, &shared))
    };
    if let Err(error) = lemnosd {
        log(&format!("cannot start the lemnosd thread: {error}"));
        return ExitCode::FAILURE;
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            log(&format!("cannot start the runtime: {error}"));
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(publish::run(config, shared));
    ExitCode::SUCCESS
}
