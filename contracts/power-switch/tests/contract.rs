use lemnos_contract_power_switch::PowerSwitch;
use lemnos_core::{Contract, Shape};

#[test]
fn the_tag_and_numbers_are_stable() {
    let info = <dyn PowerSwitch as Contract>::INFO;
    assert_eq!(info.tag.as_str(), "power-switch/1");
    let ids: Vec<_> = info.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(ids, [("set", 1), ("is_on", 2), ("fault", 3), ("cycle", 4)]);
    assert_eq!(info.methods[2].returns, Shape::Option(&Shape::Bool));
}
