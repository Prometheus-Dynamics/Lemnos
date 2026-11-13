//! When a client's connection closes, drivers are told, whatever way it closed.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lemnos_client::{Client, Error};
use lemnos_core::{CallerId, Config as DriverConfig, Lifecycle, Result, Status, contract, driver};
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

#[contract("holder", version = 1)]
pub trait Holding {
    #[method(1)]
    fn take(&mut self) -> Result<()>;
    #[method(2)]
    fn held(&mut self) -> Result<u32>;
}

#[driver("holder", provides = [Holding])]
pub struct Holder {
    calling: CallerId,
    holders: [Option<CallerId>; 8],
}

impl Lifecycle for Holder {
    type Needs = HolderNeeds;
    const TRACKS_CALLERS: bool = true;

    fn start(_: HolderNeeds, _: &DriverConfig<'_>) -> Result<Self> {
        Ok(Self {
            calling: CallerId::LOCAL,
            holders: [None; 8],
        })
    }

    fn caller(&mut self, caller: CallerId) {
        self.calling = caller;
    }

    fn caller_gone(&mut self, caller: CallerId) {
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
}

fn start() -> (Service, std::path::PathBuf) {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnosd-callers-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("sock");
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                [[devices]]\nid = \"h\"\ndriver = \"holder\"\n";
    let service = Service::start(Config {
        socket: socket.clone(),
        name: "test-host".to_string(),
        board: parse(text).unwrap(),
        drivers: vec![entry::<Holder>()],
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    })
    .unwrap();
    (service, socket)
}

#[test]
fn a_closed_connection_gives_up_only_its_own_holds() {
    let (_service, socket) = start();
    let watcher = Client::connect(&socket, "watcher").unwrap();
    let mut watch = watcher.open::<dyn Holding>("h").unwrap();
    watch.take().unwrap();

    let leaving = Client::connect(&socket, "leaving").unwrap();
    {
        let mut holder = leaving.open::<dyn Holding>("h").unwrap();
        holder.take().unwrap();
        holder.take().unwrap();
    }
    assert_eq!(watch.held().unwrap(), 3);

    drop(leaving);
    let deadline = Instant::now() + Duration::from_secs(20);
    while watch.held().unwrap() != 1 {
        assert!(Instant::now() < deadline, "the holds were never given up");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_method_the_contract_does_not_have_is_refused_by_number() {
    let (_service, socket) = start();
    let client = Client::connect(&socket, "t").unwrap();
    let mut handle = client.open_tag("h", "holder/1").unwrap();
    let mut reply = [0u8; 16];
    let error = client.call(&mut handle, 99, &[], &mut reply).unwrap_err();
    assert_eq!(
        error,
        Error::Refused {
            status: Status::NOT_SUPPORTED,
            text: "contract holder/1 has no method 99".to_string(),
        }
    );
}
