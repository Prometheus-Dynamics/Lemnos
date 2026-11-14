//! A board with `slots` and no flash device: `flash` is an optional need of the switch, so a board
//! that names only `slots` (and no `flash`) binds, asks only `slots` for its answer and its mark, and
//! needs no trust root of its own to judge a change to on. Run through a host, with a stand-in for
//! the slots device that serves the stakeholder contract and counts what the switch asks of it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lemnos_contract_security_precondition::SecurityPrecondition;
use lemnos_core::{Config, Encode, Lifecycle, MethodId, Result, Writer, driver};
use lemnos_driver_system_security::SystemSecurity;
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};

static ASKED: AtomicU32 = AtomicU32::new(0);
static APPLIED: AtomicU32 = AtomicU32::new(0);

/// Stands in for `system-slots`: it answers a change to on with a missing fact and counts its calls.
#[driver("stub-slots", provides = [SecurityPrecondition])]
pub struct StubSlots {}

impl Lifecycle for StubSlots {
    type Needs = StubSlotsNeeds;

    fn start(_: StubSlotsNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {})
    }
}

impl SecurityPrecondition for StubSlots {
    fn precondition(&mut self, on: bool, into: &mut [u8]) -> Result<usize> {
        ASKED.fetch_add(1, Ordering::SeqCst);
        let text = if on {
            b"missing: the slots stub is not ready".as_slice()
        } else {
            b"".as_slice()
        };
        into[..text.len()].copy_from_slice(text);
        Ok(text.len())
    }

    fn apply(&mut self, _on: bool) -> Result<()> {
        APPLIED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn start(record: &str, uses: &str) -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [security]\nrecord = \"{record}\"\n\
         [[devices]]\nid = \"slots\"\ndriver = \"stub-slots\"\n\
         [[devices]]\nid = \"security\"\ndriver = \"system-security\"\nuses = {{ {uses} }}\n"
    );
    let sim = lemnos_sim::SimBoard::new();
    let clock = sim.clock();
    Host::start(
        &parse(&text).unwrap(),
        &[entry::<StubSlots>(), entry::<SystemSecurity>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
    )
    .unwrap()
}

fn precheck(host: &Host, on: bool) -> String {
    let index = host.device("security").unwrap().index;
    let mut args = [0u8; 8];
    let len = {
        let mut w = Writer::new(&mut args);
        on.encode(&mut w).unwrap();
        // An out-buffer method takes the buffer's capacity after its arguments.
        1024u32.encode(&mut w).unwrap();
        w.len()
    };
    let mut reply = [0u8; 1024];
    let n = host
        .call(
            index,
            MethodId {
                contract: 0,
                method: 2,
            },
            &args[..len],
            &mut reply,
        )
        .unwrap();
    String::from_utf8(reply[..n].to_vec()).unwrap()
}

#[test]
fn with_slots_and_no_flash_the_switch_binds_asks_only_slots_and_applies_only_to_slots() {
    let dir = std::env::temp_dir().join(format!("lemnos-sec-slots-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let record = dir.join("security.rec");
    let host = start(&record.to_string_lossy(), "slots = \"slots\"");
    let info = host.device("security").unwrap();
    // `flash` is not named, so its absence is not a fault: the switch is running and healthy.
    assert_eq!(info.state, DeviceState::Running);
    assert_eq!(info.health, lemnos_core::Health::Ok);
    // At start the switch makes the slots' mark follow the record: one `apply`, to slots alone.
    assert_eq!(APPLIED.load(Ordering::SeqCst), 1);

    let asked = ASKED.load(Ordering::SeqCst);
    let on = precheck(&host, true);
    assert_eq!(
        ASKED.load(Ordering::SeqCst),
        asked + 1,
        "slots is asked once"
    );
    assert!(
        on.contains("slots") && on.contains("the slots stub is not ready"),
        "{on}"
    );
    assert!(!on.contains("flash"), "{on}");
    // Slots answers for the board, so the switch does not add a trust-root answer of its own.
    assert!(!on.contains("no trust root"), "{on}");

    let off = precheck(&host, false);
    assert!(!off.contains("missing"), "{off}");
    assert!(!off.contains("flash"), "{off}");
    drop(host);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn naming_a_flash_device_the_board_does_not_have_is_refused_not_degraded() {
    let dir = std::env::temp_dir().join(format!("lemnos-sec-noflash-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let record = dir.join("security.rec");
    let host = start(
        &record.to_string_lossy(),
        "slots = \"slots\", flash = \"flash\"",
    );
    let info = host.device("security").unwrap();
    match info.state {
        DeviceState::Backoff { reason, .. } => assert_eq!(
            reason,
            "device \"security\": `uses` key \"flash\" names device \"flash\", which the board does not have"
        ),
        other => panic!("{other:?}"),
    }
    drop(host);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_switch_reads_only_the_keys_the_host_gives_it() {
    let cfg = |entries: &[(&str, lemnos_core::ConfigValue<'_>)]| {
        <SystemSecurity as lemnos_core::Driver>::check_config(&Config::new(entries))
            .map_err(|e| e.to_string())
    };
    use lemnos_core::ConfigValue::{Bool, Str};
    assert!(
        cfg(&[
            ("record", Str("/r")),
            ("keys", Str("/k")),
            ("orion", Bool(false))
        ])
        .is_ok()
    );
    assert_eq!(
        cfg(&[("run", Str("/tmp"))]).unwrap_err(),
        "setting \"run\" is not a setting this driver reads"
    );
    assert_eq!(
        cfg(&[("security_view", Str("/tmp"))]).unwrap_err(),
        "setting \"security_view\" is not a setting this driver reads"
    );
}
