//! Offers a USB function's bulk endpoints to the comm core as a link.
//!
//! lemnosd lends the bulk pair to one connection at a time. This thread holds the pair, sends a
//! `LinkOffer` with it to the comm core over the board's socket, and keeps the hold while the
//! gadget is up. The descriptors go as `[read, write]`: the OUT endpoint's file first (the host's
//! writes, which the comm core reads), then the IN endpoint's. When the gadget goes down the hold
//! ends with it; this thread drops the pair and offers again once the gadget is back. Failures wait
//! at 1 s, doubling, capped at 8 s, and never give up.

use std::os::fd::AsFd;
use std::sync::Arc;
use std::time::Duration;

use lemnos_client::{Error, Session, UsbEndpoints};
use lemnos_linux_stream::LinuxStreams;
use orion_connect::{Options, Session as Comm};
use orion_msg_session::{LinkOffer, LinkTaken, LinkTransport};
use orion_wire::Body;

use crate::config::{Config, Link};
use crate::log;

/// The name this thread gives lemnosd, and the two connections it keeps there.
const LEMNOSD_NAME: &str = "orion-links";
const LEMNOSD_WATCH_NAME: &str = "orion-links-watch";
/// The name this thread gives the comm core.
const COMM_NAME: &str = "lemnos-orion-links";
/// The tag of the device's contract.
const TAG: &str = "usb.function/1";
/// The `state` method of `usb.function/1`: `up`, `configured`, `suspended`, `request_pending`.
const STATE: u16 = 4;
const FIRST: Duration = Duration::from_secs(1);
const CAP: Duration = Duration::from_secs(8);

/// Offers `link`'s device's endpoints for as long as the process runs.
pub fn run(config: &Config, link: &Link) {
    let mut wait = FIRST;
    loop {
        match hold(config, link) {
            // The gadget went down: the hold ended with it, and the next offer starts over.
            Ok(()) => wait = FIRST,
            Err(error) => {
                log(&format!("link {}: {error}", link.link));
                wait = wait.saturating_mul(2).min(CAP);
            }
        }
        std::thread::sleep(wait);
    }
}

/// Holds the pair and offers it, until the gadget goes down. `Ok` then; `Err` when the offer
/// could not be made or the comm core refused it. Either way the hold is given up with the session.
pub fn hold(config: &Config, link: &Link) -> Result<(), String> {
    let mut lemnos = Session::connect(&config.lemnosd_socket, LEMNOSD_NAME)
        .map_err(|error| format!("lemnosd: {error}"))?;
    let index = lemnos
        .list()
        .map_err(|error| format!("listing devices: {error}"))?
        .into_iter()
        .find(|device| device.name == link.device)
        .map(|device| device.index)
        .ok_or_else(|| format!("no device {:?} on lemnosd", link.device))?;

    // The gadget's state comes on a second connection, so the hold's own is never read by it.
    let mut watch = Session::connect(&config.lemnosd_socket, LEMNOSD_WATCH_NAME)
        .map_err(|error| format!("lemnosd: {error}"))?;
    let mut handle = watch
        .open(&link.device, TAG)
        .map_err(|error| format!("opening {:?}: {error}", link.device))?;
    watch
        .subscribe(&mut handle, STATE, 100_000, true)
        .map_err(|error| format!("watching {:?}: {error}", link.device))?;
    wait_for_up(&mut watch, true)?;

    let pair = lemnos
        .usb_endpoints(index, Arc::new(LinuxStreams))
        .map_err(|error| format!("taking the endpoints: {error}"))?;
    let _comm = offer(config, link, &pair)?;
    log(&format!("link {}: offered the endpoints", link.link));
    // `pair`'s descriptors are closed here: the comm core holds its own copies. The hold stays with
    // `lemnos`, and it ends with it.
    drop(pair);

    wait_for_up(&mut watch, false)?;
    lemnos
        .usb_endpoints_release(index)
        .or_else(|error| match error {
            // Already ended by the service when the gadget went down.
            Error::Refused { .. } => Ok(()),
            other => Err(format!("releasing the endpoints: {other}")),
        })?;
    log(&format!("link {}: the gadget went down", link.link));
    Ok(())
}

/// Waits until the gadget's `up` is `want`.
fn wait_for_up(watch: &mut Session, want: bool) -> Result<(), String> {
    // The state record is a block: its length, then its four bools, `up` first.
    let mut state = [0u8; 64];
    loop {
        match watch.next_value(&mut state, Duration::from_secs(1)) {
            Ok(Some(info)) => {
                let body = state.get(4..info.len).unwrap_or(&[]);
                if body.first().is_some_and(|up| (*up != 0) == want) {
                    return Ok(());
                }
            }
            Ok(None) => {}
            Err(error) => return Err(format!("watching the gadget: {error}")),
        }
    }
}

/// Sends the offer to the comm core, with the endpoints as `[read, write]`: the OUT endpoint, then
/// the IN endpoint. Returns the comm core's session, which stays open with the link.
fn offer(config: &Config, link: &Link, pair: &UsbEndpoints) -> Result<Comm, String> {
    let mut comm = Comm::connect(&config.orion, COMM_NAME, Options::default())
        .map_err(|error| format!("the comm core: {error}"))?;
    let offer = LinkOffer {
        link: &link.link,
        transport: LinkTransport::Endpoints,
        interface: 0,
        endpoint_in: 0,
        endpoint_out: 0,
    };
    let seq = comm
        .send_with_fds(&offer, &[pair.out.as_fd(), pair.input.as_fd()])
        .map_err(|error| format!("offering the endpoints: {error}"))?;
    // Taken only once the comm core answers LinkTaken; a refusal (no such link, say) is an error.
    comm.answer(seq, <LinkTaken as Body>::KIND)
        .map_err(|error| format!("the comm core refused the endpoints: {error}"))?;
    Ok(comm)
}
