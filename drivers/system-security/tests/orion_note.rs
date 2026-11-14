//! On a board whose file says `orion = false` the switch can be turned on while Orion stays open,
//! so its replies say what it does not cover: a line in `state`, and a `note:` line in the reply to
//! a `set` to on. With `orion = true` the state has no such line, and a change to on stays refused.

use std::sync::Arc;

use lemnos_contract_security_precondition::SecurityPrecondition;
use lemnos_core::{Config, Encode, Lifecycle, MethodId, Result, Writer, driver};
use lemnos_driver_system_security::SystemSecurity;
use lemnos_host::board::parse;
use lemnos_host::{Host, Options, entry};

/// A stakeholder with nothing to say: every change is fine by it.
#[driver("quiet-slots", provides = [SecurityPrecondition])]
pub struct QuietSlots {}

impl Lifecycle for QuietSlots {
    type Needs = QuietSlotsNeeds;

    fn start(_: QuietSlotsNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {})
    }
}

impl SecurityPrecondition for QuietSlots {
    fn precondition(&mut self, _on: bool, _into: &mut [u8]) -> Result<usize> {
        Ok(0)
    }

    fn apply(&mut self, _on: bool) -> Result<()> {
        Ok(())
    }
}

const NOT_COVERED: &str =
    "orion: not covered by this switch on this board (the board file says orion = false)";

fn start(dir: &std::path::Path, orion: bool) -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [security]\nrecord = \"{}\"\norion = {orion}\n\
         [[devices]]\nid = \"slots\"\ndriver = \"quiet-slots\"\n\
         [[devices]]\nid = \"security\"\ndriver = \"system-security\"\nuses = {{ slots = \"slots\" }}\n",
        dir.join("security.rec").display()
    );
    let sim = lemnos_sim::SimBoard::new();
    let clock = sim.clock();
    Host::start(
        &parse(&text).unwrap(),
        &[entry::<QuietSlots>(), entry::<SystemSecurity>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
    )
    .unwrap()
}

fn call(host: &Host, method: u16, args: impl FnOnce(&mut Writer<'_>)) -> Result<String> {
    let index = host.device("security").unwrap().index;
    let mut buffer = [0u8; 128];
    let len = {
        let mut w = Writer::new(&mut buffer);
        args(&mut w);
        1024u32.encode(&mut w).unwrap();
        w.len()
    };
    let mut reply = [0u8; 1024];
    let n = host.call(
        index,
        MethodId {
            contract: 0,
            method,
        },
        &buffer[..len],
        &mut reply,
    )?;
    Ok(String::from_utf8(reply[..n].to_vec()).unwrap())
}

fn state(host: &Host) -> String {
    call(host, 1, |_| {}).unwrap()
}

fn set_on(host: &Host) -> Result<String> {
    call(host, 3, |w| {
        true.encode(w).unwrap();
        // The authorization: empty, as a change to on takes none.
        (&[][..] as &[u8]).encode(w).unwrap();
    })
}

fn dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lemnos-sec-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn state_says_orion_is_not_covered_when_the_board_file_says_orion_is_false() {
    let dir = dir("state-false");
    let state = state(&start(&dir, false));
    assert!(state.lines().any(|line| line == NOT_COVERED), "{state}");
    assert!(state.contains("orion absent"), "{state}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn state_has_no_such_line_when_orion_is_true() {
    let dir = dir("state-true");
    let state = state(&start(&dir, true));
    assert!(!state.contains("not covered"), "{state}");
    assert!(state.contains("orion present"), "{state}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_set_to_on_with_orion_false_answers_the_note() {
    let dir = dir("set-false");
    let host = start(&dir, false);
    let reply = set_on(&host).expect("the change is made");
    assert_eq!(reply, format!("note: {NOT_COVERED}\n"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_set_to_on_with_orion_true_stays_refused() {
    let dir = dir("set-true");
    let host = start(&dir, true);
    assert_eq!(set_on(&host), Err(lemnos_core::Status::NOT_PERMITTED));
    let _ = std::fs::remove_dir_all(dir);
}
