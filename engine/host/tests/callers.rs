//! A driver that holds something for a caller learns who is calling and when they have gone.

use std::sync::Arc;
use std::time::Duration;

use lemnos_core::{CallerId, Config, Lifecycle, MethodId, Result, contract, driver};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

#[contract("holder", version = 1)]
pub trait Holding {
    #[method(1)]
    fn take(&mut self) -> Result<()>;
    #[method(2)]
    fn held(&mut self) -> Result<u32>;
    #[method(3)]
    fn panic_when_told(&mut self) -> Result<()>;
}

#[driver("holder", provides = [Holding])]
pub struct Holder {
    calling: CallerId,
    holders: [Option<CallerId>; 8],
    panic_when_told: bool,
}

impl Lifecycle for Holder {
    type Needs = HolderNeeds;
    const TRACKS_CALLERS: bool = true;

    fn start(_: HolderNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {
            calling: CallerId::LOCAL,
            holders: [None; 8],
            panic_when_told: false,
        })
    }

    fn caller(&mut self, caller: CallerId) {
        self.calling = caller;
    }

    fn caller_gone(&mut self, caller: CallerId) {
        assert!(!self.panic_when_told, "asked to");
        for holder in &mut self.holders {
            if *holder == Some(caller) {
                *holder = None;
            }
        }
    }
}

impl Holding for Holder {
    fn take(&mut self) -> Result<()> {
        if let Some(free) = self.holders.iter_mut().find(|h| h.is_none()) {
            *free = Some(self.calling);
        }
        Ok(())
    }

    fn held(&mut self) -> Result<u32> {
        Ok(self.holders.iter().flatten().count() as u32)
    }

    fn panic_when_told(&mut self) -> Result<()> {
        self.panic_when_told = true;
        Ok(())
    }
}

fn host() -> (Host, SimClock) {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                [[devices]]\nid = \"h\"\ndriver = \"holder\"\n";
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(text).unwrap(),
        &[entry::<Holder>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .unwrap();
    (host, clock)
}

fn method(method: u16) -> MethodId {
    MethodId {
        contract: 0,
        method,
    }
}

fn take(host: &Host, caller: CallerId) {
    let device = host.device("h").unwrap().index;
    host.call_as(caller, device, method(1), &[], &mut [])
        .unwrap();
}

fn held(host: &Host) -> u32 {
    let device = host.device("h").unwrap().index;
    let mut reply = [0; 4];
    host.call(device, method(2), &[], &mut reply).unwrap();
    u32::from_le_bytes(reply)
}

#[test]
fn only_what_the_gone_caller_held_is_given_up() {
    let (host, _) = host();
    take(&host, CallerId(7));
    take(&host, CallerId(7));
    take(&host, CallerId(8));
    assert_eq!(held(&host), 3);
    host.caller_gone(CallerId(7));
    assert_eq!(held(&host), 1);
    host.caller_gone(CallerId(8));
    assert_eq!(held(&host), 0);
}

#[test]
fn a_caller_nobody_knows_changes_nothing() {
    let (host, _) = host();
    take(&host, CallerId(7));
    host.caller_gone(CallerId(9));
    assert_eq!(held(&host), 1);
}

#[test]
fn a_call_without_a_caller_is_local() {
    let (host, _) = host();
    let device = host.device("h").unwrap().index;
    host.call(device, method(1), &[], &mut []).unwrap();
    host.caller_gone(CallerId(7));
    assert_eq!(held(&host), 1);
    host.caller_gone(CallerId::LOCAL);
    assert_eq!(held(&host), 0, "the driver saw it as LOCAL");
}

#[test]
fn a_panic_when_told_is_contained_and_the_device_comes_back() {
    let (host, clock) = host();
    let device = host.device("h").unwrap().index;
    host.call(device, method(3), &[], &mut []).unwrap();
    host.caller_gone(CallerId(7));
    assert!(matches!(
        host.device("h").unwrap().state,
        DeviceState::Backoff { .. }
    ));
    clock.advance(Duration::from_secs(60));
    host.tick();
    assert_eq!(host.device("h").unwrap().state, DeviceState::Running);
    assert_eq!(held(&host), 0);
}

/// Whether a caller other than `caller` is using the device `h`.
fn others_use(host: &Host, caller: CallerId) -> bool {
    let device = host.device("h").unwrap().index;
    host.others_use(caller, device).unwrap()
}

#[test]
fn a_caller_that_has_called_is_a_user_until_it_goes() {
    let (host, _) = host();
    assert!(!others_use(&host, CallerId(7)), "nobody has called");
    take(&host, CallerId(7));
    assert!(others_use(&host, CallerId(8)), "another caller uses it");
    assert!(
        !others_use(&host, CallerId(7)),
        "the caller itself is not another user"
    );
    host.caller_gone(CallerId(7));
    assert!(!others_use(&host, CallerId(8)), "the caller has gone");
}

#[test]
fn a_call_from_the_local_caller_is_a_user_too() {
    let (host, _) = host();
    let device = host.device("h").unwrap().index;
    host.call(device, method(1), &[], &mut []).unwrap();
    assert!(
        others_use(&host, CallerId(7)),
        "an in-process driver uses it"
    );
    assert!(!others_use(&host, CallerId::LOCAL));
}

#[test]
fn a_device_with_more_callers_than_it_keeps_is_in_use_for_every_asker() {
    let (host, _) = host();
    for id in 100..(100 + 16) {
        take(&host, CallerId(id));
    }
    // Sixteen callers is the set's size: an asker that is not among them is still answered "in use".
    assert!(others_use(&host, CallerId(7)));
    // The asker that is one of them is answered by the others that are kept.
    assert!(others_use(&host, CallerId(100)));
}

#[test]
fn an_unknown_device_is_not_found() {
    let (host, _) = host();
    assert_eq!(
        host.others_use(CallerId(7), 99),
        Err(lemnos_core::Status::NOT_FOUND)
    );
}
