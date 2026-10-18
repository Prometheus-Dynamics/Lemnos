//! `lemnos-orion`: lemnosd's devices as Orion resources (`docs/orion.md`).
//! Build with `--features orion`; configured by its environment (see
//! `lemnosd::orion::bridge::Config::from_env`).

use std::process::ExitCode;

#[allow(clippy::print_stderr)]
fn main() -> ExitCode {
    let config = match lemnosd::orion::bridge::Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("lemnos-orion: {error}");
            return ExitCode::FAILURE;
        }
    };
    // The bridge is I/O bound: one thread runs the loop, and the blocking
    // lemnosd calls go to tokio's blocking pool.
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("lemnos-orion: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(lemnosd::orion::bridge::run(config)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lemnos-orion: {error}");
            ExitCode::FAILURE
        }
    }
}
