use lemnos_contract_temperature::Temperature;
use lemnos_core::{Contract, Shape};

#[test]
fn the_tag_and_numbers_are_stable() {
    let info = <dyn Temperature as Contract>::INFO;
    assert_eq!(info.tag.as_str(), "temperature/1");
    assert_eq!(info.methods[0].id, 1);
    assert_eq!(info.methods[0].name, "read");
    assert_eq!(info.methods[0].returns, Shape::F32);
}
