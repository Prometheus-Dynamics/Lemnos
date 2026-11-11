//! `lemnos-usb-echo SOCKET [DEVICE]`: see the library.

use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(socket) = args.next() else {
        let _ = writeln!(std::io::stderr(), "usage: lemnos-usb-echo SOCKET [DEVICE]");
        return ExitCode::from(2);
    };
    let name = args.next().unwrap_or_default();
    match lemnos_usb_echo::serve(&socket, &name) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "lemnos-usb-echo: {error}");
            ExitCode::FAILURE
        }
    }
}
