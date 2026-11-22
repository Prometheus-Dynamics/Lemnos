use lemnos_contract_system_reboot::Reboot;
use lemnos_core::{Contract, ContractInfo, layout::contract_hash};

#[test]
fn the_tag_and_numbers_are_stable() {
    let info = <dyn Reboot as Contract>::INFO;
    assert_eq!(info.tag.as_str(), "system.reboot/1");
    let ids: Vec<_> = info.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(ids, [("reboot", 1), ("reboot_to", 2)]);
}

/// `reboot_to` is an addition: a client built with it still binds to an older daemon's restart devices, and the
/// contract's base hash is the hash of `reboot` alone.
#[test]
fn reboot_to_is_added_and_the_base_hash_does_not_move() {
    let info = <dyn Reboot as Contract>::INFO;
    let to = info.methods.iter().find(|m| m.name == "reboot_to").unwrap();
    assert!(to.added);
    assert!(!info.methods[0].added);
    let base = ContractInfo {
        methods: &info.methods[..1],
        ..info
    };
    assert_eq!(<dyn Reboot as Contract>::HASH, contract_hash(&base));
}
