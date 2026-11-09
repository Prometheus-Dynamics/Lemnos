use lemnos_contract_fan::Fan;
use lemnos_core::{Contract, Shape};

#[test]
fn the_tag_and_numbers_are_stable() {
    let info = <dyn Fan as Contract>::INFO;
    assert_eq!(info.tag.as_str(), "fan/1");
    let ids: Vec<_> = info.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(
        ids,
        [("set_duty", 1), ("duty", 2), ("rpm", 3), ("release", 4)]
    );
    assert_eq!(info.methods[0].params[0].shape, Shape::U16);
    assert_eq!(info.methods[2].returns, Shape::Option(&Shape::U32));
}
