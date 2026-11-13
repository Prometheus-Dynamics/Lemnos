//! Offers a USB function's bulk endpoints to the comm core as a link.
//!
//! lemnosd lends the bulk pair to one connection at a time, with a third descriptor, END. This
//! thread holds them, sends a `LinkOffer` with the pair to the comm core over the board's socket,
//! and keeps the hold while the gadget is up. The pair goes as `[read, write]`: the OUT endpoint's
//! file first (the host's writes, which the comm core reads), then the IN endpoint's.
//!
//! END is the hold's own word. It reads EOF or hangs up when the hold is over for good (the
//! service closed it because the driver ended the endpoints, or the hold was given up). This thread
//! then gives all three back and offers again once the gadget is back. A gadget that goes down
//! ends the hold too, as lemnosd sees it, so this thread gives the hold back and offers again once
//! the gadget is up. Failures wait at 1 s, doubling, capped at 8 s, and never give up.

use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::time::Duration;

use lemnos_client::{Error, Session, UsbEndpointsEnd};
use lemnos_linux_stream::LinuxStreams;
use orion_connect::{Options, Session as Comm};
use orion_msg_session::{LinkOffer, LinkTaken, LinkTransport};
use orion_wire::Body;
use rustix::event::{PollFd, PollFlags};

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
    wait_for_up(&mut watch, true, None)?;

    let UsbEndpointsEnd { out, input, end } = lemnos
        .usb_endpoints_end(index, Arc::new(LinuxStreams))
        .map_err(|error| format!("taking the endpoints: {error}"))?;
    let _comm = offer(config, link, &out, &input)?;
    log(&format!("link {}: offered the endpoints", link.link));
    // OUT and IN are closed here: the comm core holds its own copies. END stays with this hold,
    // and it is what says the hold is over for good.
    drop((out, input));

    let over = wait_for_up(&mut watch, false, Some(&end))?;
    lemnos
        .usb_endpoints_release(index)
        .or_else(|error| match error {
            // Already ended by the service when the hold went.
            Error::Refused { .. } => Ok(()),
            other => Err(format!("releasing the endpoints: {other}")),
        })?;
    if over {
        log(&format!("link {}: the endpoints' hold is over", link.link));
    } else {
        log(&format!("link {}: the gadget went down", link.link));
    }
    Ok(())
}

/// Whether END reads EOF or hangs up: the hold is over for good. Never blocks.
fn over(end: &OwnedFd) -> bool {
    let mut fds = [PollFd::new(end, PollFlags::IN)];
    let now = rustix::fs::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    match rustix::event::poll(&mut fds, Some(&now)) {
        Ok(_) => !fds[0].revents().is_empty(),
        Err(_) => false,
    }
}

/// Waits until the gadget's `up` is `want`. With `end`, also returns `Ok(true)` as soon as END says
/// the hold is over for good; `Ok(false)` when the gadget got to `want` first.
fn wait_for_up(watch: &mut Session, want: bool, end: Option<&OwnedFd>) -> Result<bool, String> {
    // The state record is a block: its length, then its four bools, `up` first.
    let mut state = [0u8; 64];
    loop {
        if end.is_some_and(over) {
            return Ok(true);
        }
        match watch.next_value(&mut state, Duration::from_secs(1)) {
            Ok(Some(info)) => {
                let body = state.get(4..info.len).unwrap_or(&[]);
                if body.first().is_some_and(|up| (*up != 0) == want) {
                    return Ok(false);
                }
            }
            Ok(None) => {}
            Err(error) => return Err(format!("watching the gadget: {error}")),
        }
    }
}

/// Sends the offer to the comm core, with the endpoints as `[read, write]`: the OUT endpoint, then
/// the IN endpoint. END is not sent: the pinned comm core's `LinkOffer` takes two. Returns the comm
/// core's session, which stays open with the link.
fn offer(config: &Config, link: &Link, out: &OwnedFd, input: &OwnedFd) -> Result<Comm, String> {
    let mut comm = Comm::connect(&config.orion, COMM_NAME, Options::default())
        .map_err(|error| format!("the comm core: {error}"))?;
    let offer = LinkOffer {
        link: &link.link,
        transport: LinkTransport::Endpoints,
        interface: 0,
        endpoint_in: 0,
        endpoint_out: 0,
    };
    // ORION-R2: at R2 the descriptors are `[OUT, IN]` plus an optional END, in that order (END is
    // R1f's third descriptor, which the bridge keeps); `LinkOffer` takes the same list then.
    let seq = comm
        .send_with_fds(&offer, &[out.as_fd(), input.as_fd()])
        .map_err(|error| format!("offering the endpoints: {error}"))?;
    // Taken only once the comm core answers LinkTaken; a refusal (no such link, say) is an error.
    comm.answer(seq, <LinkTaken as Body>::KIND)
        .map_err(|error| format!("the comm core refused the endpoints: {error}"))?;
    Ok(comm)
}
