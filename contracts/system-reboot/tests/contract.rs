use lemnos_contract_system_reboot::Reboot;
use lemnos_core::Contract;

#[test]
fn the_tag_and_numbers_are_stable() {
    let info = <dyn Reboot as Contract>::INFO;
    assert_eq!(info.tag.as_str(), "system.reboot/1");
    let ids: Vec<_> = info.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(ids, [("reboot", 1)]);
}
