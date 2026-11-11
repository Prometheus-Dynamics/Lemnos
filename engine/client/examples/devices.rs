//! Lists the devices a service has, then reads the first accelerometer through its contract.
//!
//! `cargo run -p lemnos-client --example devices -- SOCKET`

use std::io::Write;

use lemnos_client::Client;
use lemnos_contract_accelerometer::Accelerometer;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket = std::env::args().nth(1).ok_or("usage: devices SOCKET")?;
    // The second argument is this program's name, as the service will know it.
    let client = Client::connect(&socket, "devices-example")?;
    let mut out = std::io::stdout().lock();

    for device in client.list()? {
        writeln!(
            out,
            "{} ({}): {:?}, {:?}, provides {}",
            device.name,
            device.driver,
            device.state,
            device.health,
            device.tags.join(" ")
        )?;
    }

    // An empty name means any device that provides the contract.
    let mut accelerometer = client.open::<dyn Accelerometer>("")?;
    let sample = accelerometer
        .read()
        .map_err(|status| format!("read refused: {status:?}"))?;
    writeln!(out, "x={} y={} z={} m/s²", sample.x, sample.y, sample.z)?;
    Ok(())
}
