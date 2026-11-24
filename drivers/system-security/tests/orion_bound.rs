//! The `orion` stakeholder: on a board whose file says Orion ships, a change to on is asked of the
//! device bound as `uses = { orion = ... }`, and its answer is the switch's answer. On a board that
//! says `orion = false` the device is not asked.

use std::sync::Arc;

use lemnos_contract_security_precondition::SecurityPrecondition;
use lemnos_core::{Config, Encode, Lifecycle, MethodId, Result, Writer, driver};
use lemnos_driver_system_security::SystemSecurity;
use lemnos_host::board::parse;
use lemnos_host::{Host, entry};

/// A stakeholder with nothing to say: every change is fine by it.
#[driver("quiet", provides = [SecurityPrecondition])]
pub struct Quiet {}

impl Lifecycle for Quiet {
    type Needs = QuietNeeds;

    fn start(_: QuietNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {})
    }
}

impl SecurityPrecondition for Quiet {
    fn precondition(&mut self, _on: bool, _into: &mut [u8]) -> Result<usize> {
        Ok(0)
    }

    fn apply(&mut self, _on: bool) -> Result<()> {
        Ok(())
    }
}

/// A stakeholder that says no, as Orion does on a node with no operator: a `missing:` line.
#[driver("says-no", provides = [SecurityPrecondition])]
pub struct SaysNo {}

impl Lifecycle for SaysNo {
    type Needs = SaysNoNeeds;

    fn start(_: SaysNoNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {})
    }
}

const NO: &str = "missing: orion: stub says no\n";

impl SecurityPrecondition for SaysNo {
    fn precondition(&mut self, _on: bool, into: &mut [u8]) -> Result<usize> {
        into.get_mut(..NO.len())
            .ok_or(lemnos_core::Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(NO.as_bytes());
        Ok(NO.len())
    }

    fn apply(&mut self, _on: bool) -> Result<()> {
        Ok(())
    }
}

fn start(dir: &std::path::Path, orion: bool, bind_orion: bool) -> Host {
    let uses = if bind_orion {
        "uses = { slots = \"slots\", orion = \"orion\" }"
    } else {
        "uses = { slots = \"slots\" }"
    };
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [security]\nrecord = \"{}\"\norion = {orion}\n\
         [[devices]]\nid = \"slots\"\ndriver = \"quiet\"\n\
         [[devices]]\nid = \"orion\"\ndriver = \"says-no\"\n\
         [[devices]]\nid = \"security\"\ndriver = \"system-security\"\n{uses}\n",
        dir.join("security.rec").display()
    );
    let sim = lemnos_sim::SimBoard::new();
    let clock = sim.clock();
    Host::start(
        &parse(&text).unwrap(),
        &[
            entry::<Quiet>(),
            entry::<SaysNo>(),
            entry::<SystemSecurity>(),
        ],
        Box::new(sim),
        Arc::new(clock),
        lemnos_test_options::options(),
    )
    .unwrap()
}

/// The `precheck` method (2) of `security/1`: the switch's own answer for a change to `on`.
fn precheck(host: &Host, on: bool) -> Result<String> {
    let index = host.device("security").unwrap().index;
    let mut buffer = [0u8; 128];
    let len = {
        let mut w = Writer::new(&mut buffer);
        on.encode(&mut w).unwrap();
        // The reply buffer's length, as the host's calls carry it.
        1024u32.encode(&mut w).unwrap();
        w.len()
    };
    let mut reply = [0u8; 1024];
    let n = host.call(
        index,
        MethodId {
            contract: 0,
            method: 2,
        },
        &buffer[..len],
        &mut reply,
    )?;
    Ok(String::from_utf8(reply[..n].to_vec()).unwrap())
}

fn dir(name: &str) -> lemnos_scratch::Scratch {
    let dir = lemnos_scratch::Scratch::adopt(
        std::env::temp_dir().join(format!("lemnos-orion-bound-{name}-{}", std::process::id())),
    );
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// With Orion on the board and its device bound, Orion's answer is the switch's answer: its `missing:`
/// line comes back by name, and the unbound refusal does not.
#[test]
fn a_bound_orion_device_answers_for_the_switch_by_name() {
    let dir = dir("bound-no");
    let host = start(&dir, true, true);
    let reply = precheck(&host, true).expect("the precheck answers");
    assert!(reply.contains("missing: orion: stub says no"), "{reply}");
    assert!(!reply.contains("no orion device is bound"), "{reply}");
}

/// With Orion on the board and no device bound, a change to on is refused by name.
#[test]
fn an_unbound_orion_refuses_a_change_to_on_by_name() {
    let dir = dir("unbound");
    let host = start(&dir, true, false);
    let reply = precheck(&host, true).expect("the precheck answers");
    assert!(
        reply.contains("no orion device is bound to ask it"),
        "{reply}"
    );
}

/// On a board whose file says `orion = false` the bound device is not asked: its refusal is not in
/// the answer.
#[test]
fn a_board_without_orion_does_not_ask_the_bound_device() {
    let dir = dir("no-orion");
    let host = start(&dir, false, true);
    let reply = precheck(&host, true).expect("the precheck answers");
    assert!(!reply.contains("stub says no"), "{reply}");
}
