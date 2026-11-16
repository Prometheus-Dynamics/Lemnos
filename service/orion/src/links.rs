//! Offers a USB function's bulk endpoints to the comm core as a link.
//!
//! lemnosd lends the bulk pair to one connection at a time, with a third descriptor, END. This
//! thread holds them, sends a `LinkOffer` with the pair to the comm core over the board's socket,
//! and keeps the hold while the gadget is up. The pair goes as `[read, write]`: the OUT endpoint's
//! file first (the host's writes, which the comm core reads), then the IN endpoint's.
//!
//! END is the hold's own word. It reads EOF or hangs up when the hold is over for good: the device was
//! switched off, reloaded or shut down, or the driver ended its endpoints. Only then does this thread give
//! all three back and offer again, because only then are the endpoint files new. A controller that lets go
//! of the gadget and takes it back (`UDC` cleared and set) leaves the pair the same, so the hold and the
//! comm core's stream on it are kept: the comm core's reader retries an endpoint that is down (its
//! `ESHUTDOWN` and `EAGAIN`), and a new host's HELLO begins a new session on the same stream (C57). The
//! gadget's state is still watched and logged. Failures wait at 1 s, doubling, capped at 8 s, and never
//! give up.

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

/// What the bridge says once at start when no link is configured: an image that forgot
/// `LEMNOS_ORION_LINKS` shows it in the log, not by a link that never comes.
pub const NONE_CONFIGURED: &str =
    "no USB link is configured (LEMNOS_ORION_LINKS is unset): none is offered";

/// The line to log once at start, if `links` is empty.
#[must_use]
pub fn start_notice(links: &[Link]) -> Option<&'static str> {
    links.is_empty().then_some(NONE_CONFIGURED)
}

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
    wait_for_ready(&mut watch)?;

    let UsbEndpointsEnd { out, input, end } = lemnos
        .usb_endpoints_end(index, Arc::new(LinuxStreams))
        .map_err(|error| format!("taking the endpoints: {error}"))?;
    let _comm = offer(config, link, &out, &input)?;
    log(&format!("link {}: offered the endpoints", link.link));
    // OUT and IN are closed here: the comm core holds its own copies. END stays with this hold,
    // and it is what says the hold is over for good.
    drop((out, input));

    let over = wait_while_held(&mut watch, link, &end)?;
    lemnos
        .usb_endpoints_release(index)
        .or_else(|error| match error {
            // Already ended by the service when the hold went.
            Error::Refused { .. } => Ok(()),
            other => Err(format!("releasing the endpoints: {other}")),
        })?;
    if over {
        log(&format!("link {}: the endpoints' hold is over", link.link));
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

/// Waits until the gadget is ready (`up` and the host has chosen its configuration), and returns when it
/// is. The pair is offered only to a configured host: the comm core's reader takes an endpoint that is not
/// enabled as its own reading, so the first offer waits for the host.
fn wait_for_ready(watch: &mut Session) -> Result<(), String> {
    // The state record is a block: its length, then its four bools, `up` and `configured` first.
    let mut state = [0u8; 64];
    loop {
        match watch.next_value(&mut state, Duration::from_secs(1)) {
            Ok(Some(info)) => {
                let body = state.get(4..info.len).unwrap_or(&[]);
                if let [up, configured, ..] = body
                    && *up != 0
                    && *configured != 0
                {
                    return Ok(());
                }
            }
            Ok(None) => {}
            Err(error) => return Err(format!("watching the gadget: {error}")),
        }
    }
}

/// Holds the endpoints while END says the hold is not over, and logs each change of the gadget's state
/// (`up` and the host has chosen its configuration) without acting on it. Returns `Ok(true)` when END says the
/// hold is over for good: the endpoints are new, and the caller offers them again. A controller's let-go
/// does not end the hold (see the module's header).
fn wait_while_held(watch: &mut Session, link: &Link, end: &OwnedFd) -> Result<bool, String> {
    // The state record is a block: its length, then its four bools, `up` and `configured` first.
    let mut state = [0u8; 64];
    let mut ready: Option<bool> = None;
    loop {
        if over(end) {
            return Ok(true);
        }
        match watch.next_value(&mut state, Duration::from_secs(1)) {
            Ok(Some(info)) => {
                let body = state.get(4..info.len).unwrap_or(&[]);
                if let [up, configured, ..] = body {
                    let now = *up != 0 && *configured != 0;
                    if ready != Some(now) {
                        ready = Some(now);
                        log(&format!(
                            "link {}: the gadget is {}; the hold is kept",
                            link.link,
                            if now {
                                "configured"
                            } else {
                                "down or unconfigured"
                            }
                        ));
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_configured_link_is_said_once_and_a_configured_one_is_not() {
        assert_eq!(
            start_notice(&[]),
            Some("no USB link is configured (LEMNOS_ORION_LINKS is unset): none is offered")
        );
        let link = Link {
            device: "gadget".to_owned(),
            link: "orion-link".to_owned(),
        };
        assert_eq!(start_notice(&[link]), None);
    }
}
