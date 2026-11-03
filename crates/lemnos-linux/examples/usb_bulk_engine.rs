#![allow(clippy::print_stdout, clippy::print_stderr)]

//! Runs `UsbBulkEngine` on a real bulk endpoint: transport only.
//!
//! ```text
//! usb_bulk_engine LOC [SECONDS]   # discover the bulk endpoints, claim their interface,
//!                                 # a zero-length OUT, a bounded IN read, and a cancel
//! ```
//!
//! Nothing is sent but a zero-length packet on each bulk OUT endpoint: the
//! payload protocol of a device is not this tool's business. A bulk IN read
//! is started and waited for at most `SECONDS` (default 2), then cancelled
//! when nothing arrived. The interface is claimed with a detach, so the kernel
//! driver (if any) is unbound for the run and released afterwards.

#[cfg(feature = "usb")]
fn main() {
    if let Err(message) = run() {
        eprintln!("usb_bulk_engine: {message}");
        std::process::exit(1);
    }
}

#[cfg(not(feature = "usb"))]
fn main() {
    eprintln!("usb_bulk_engine needs the `usb` feature");
    std::process::exit(2);
}

#[cfg(feature = "usb")]
fn run() -> Result<(), String> {
    use lemnos_bus::{Transfer, TransferEngine};
    use lemnos_linux::transfer::UsbBulkEngine;
    use lemnos_usb::{Direction, TransferType, UsbDevice, UsbLocation};
    use std::time::{Duration, Instant};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let location: UsbLocation = args
        .first()
        .ok_or("a location is needed (for example 1-1)")?
        .parse()
        .map_err(|error| format!("{error}"))?;
    let seconds: u64 = match args.get(1) {
        Some(value) => value.parse().map_err(|_| "SECONDS is a number")?,
        None => 2,
    };
    let device = UsbDevice::open_location(&location)
        .map_err(|error| format!("opening {location}: {error}"))?;
    let config = device
        .active_configuration()
        .map_err(|error| error.to_string())?;

    // Discovery: every bulk endpoint of the active configuration, with its interface.
    let mut bulk = Vec::new();
    for interface in &config.interfaces {
        for setting in &interface.alternates {
            for endpoint in &setting.endpoints {
                if endpoint.transfer_type == TransferType::Bulk {
                    bulk.push((interface.number, *endpoint));
                }
            }
        }
    }
    if bulk.is_empty() {
        println!("bulk: no bulk endpoint in the active configuration");
        return Ok(());
    }
    for (interface, endpoint) in &bulk {
        println!(
            "bulk endpoint {:#04x} {:?} max {} on interface {interface}",
            endpoint.address, endpoint.direction, endpoint.max_packet_size
        );
    }
    let interfaces: std::collections::BTreeSet<u8> = bulk.iter().map(|(n, _)| *n).collect();
    let mut claims = Vec::new();
    for number in interfaces {
        let claim = device
            .detach_and_claim_interface(number)
            .map_err(|error| format!("claiming interface {number}: {error}"))?;
        println!("claimed interface {number}");
        claims.push(claim);
    }

    for (_, endpoint) in &bulk {
        let mut engine = UsbBulkEngine::new(device.clone(), endpoint.address);
        match endpoint.direction {
            Direction::Out => {
                let started = Instant::now();
                let mut ticket = engine
                    .submit(Transfer::write(&[]))
                    .map_err(|error| format!("submitting a zero-length OUT: {error}"))?;
                let done = engine
                    .wait(&mut ticket, Some(Duration::from_secs(2)))
                    .map_err(|error| format!("waiting for the zero-length OUT: {error}"))?;
                println!(
                    "OUT {:#04x} zero-length: {:?} in {:?}",
                    endpoint.address,
                    done,
                    started.elapsed()
                );
            }
            Direction::In => {
                let mut buffer = vec![0u8; usize::from(endpoint.max_packet_size.max(64))];
                let started = Instant::now();
                let mut ticket = engine
                    .submit(Transfer::read(&mut buffer))
                    .map_err(|error| format!("submitting an IN read: {error}"))?;
                let done = engine
                    .wait(&mut ticket, Some(Duration::from_secs(seconds)))
                    .map_err(|error| format!("waiting for the IN read: {error}"))?;
                match done {
                    Some(completion) => println!(
                        "IN {:#04x}: {} bytes in {:?}",
                        endpoint.address,
                        completion.bytes,
                        started.elapsed()
                    ),
                    None => {
                        println!(
                            "IN {:#04x}: no data in {seconds} s; cancelling",
                            endpoint.address
                        );
                        engine
                            .cancel(ticket)
                            .map_err(|error| format!("cancelling the IN read: {error}"))?;
                        println!("IN {:#04x}: cancelled", endpoint.address);
                    }
                }
            }
        }
    }
    drop(claims);
    Ok(())
}
