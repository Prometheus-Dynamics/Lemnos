//! Three devices become one, through nothing but `uses` and tags.

mod common;

use common::{MAG, SIX_AXIS, host, method};
use lemnos_core::{Decode, Health, Reader, Vector3};
use lemnos_host::DeviceState;

#[test]
fn three_devices_become_one() {
    let (host, _) = host(&format!("{SIX_AXIS}{MAG}"));
    let info = host.device("orientation").unwrap();
    assert_eq!(info.state, DeviceState::Running);
    assert_eq!(info.health, Health::Ok);

    let mut reply = [0; 32];
    let n = host.call(info.index, method(1), &[], &mut reply).unwrap();
    let value = Vector3::decode(&mut Reader::new(&reply[..n])).unwrap();
    assert!(value.x.abs() < 1e-3 && value.y.abs() < 1e-3, "{value:?}");
}

#[test]
fn found_by_tag_and_described() {
    let (host, _) = host(&format!("{SIX_AXIS}{MAG}"));
    let found = host.find("orientation/1");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "orientation");
    assert!(host.find("nothing/1").is_empty());

    let description = host.describe(found[0].index).unwrap();
    let roll = description.find("roll").unwrap();
    assert_eq!(roll.unit.symbol(), "rad");
    assert_eq!(roll.field, Some("x"));
}

#[test]
fn children_start_first_whatever_the_file_order() {
    // The parent comes first in the file and still binds.
    let (host, _) = host(&format!("{SIX_AXIS}{MAG}"));
    assert_eq!(
        host.device("orientation").unwrap().state,
        DeviceState::Running
    );
}

#[test]
fn link_is_handed_over_as_written() {
    let (host, _) =
        host("[[devices]]\nid = \"a\"\ndriver = \"fake-counter\"\nlink = \"somewhere\"\n");
    assert_eq!(host.link("a").as_deref(), Some("somewhere"));
}
