//! Additions: a method or a field marked `#[added]` is left out of a contract's base hash and has
//! a hash of its own; `#[breaking]` makes it base again. The rules are read from the generated
//! `INFO`, so these tests check what the macros actually write.

use lemnos_core::{Contract, Decode, Encode, Result, contract, layout};

#[derive(Debug, Encode, Decode)]
pub struct Base {
    pub a: u32,
}

#[derive(Debug, Encode, Decode)]
pub struct WithAddedField {
    pub a: u32,
    #[added]
    pub b: u8,
}

#[derive(Debug, Encode, Decode)]
pub struct WithBreakingField {
    pub a: u32,
    #[added]
    #[breaking]
    pub b: u8,
}

#[contract("additions.thing", version = 1)]
pub trait OnlyBase {
    #[method(1)]
    fn go(&mut self, args: Base) -> Result<u32>;
}

#[contract("additions.thing", version = 1)]
pub trait AddedMethod {
    #[method(1)]
    fn go(&mut self, args: Base) -> Result<u32>;
    #[added]
    #[method(2)]
    fn more(&mut self) -> Result<u8>;
}

#[contract("additions.thing", version = 1)]
pub trait AddedField {
    #[method(1)]
    fn go(&mut self, args: WithAddedField) -> Result<u32>;
}

#[contract("additions.thing", version = 1)]
pub trait BreakingField {
    #[method(1)]
    fn go(&mut self, args: WithBreakingField) -> Result<u32>;
}

#[contract("additions.thing", version = 1)]
pub trait BreakingMethod {
    #[method(1)]
    fn go(&mut self, args: Base) -> Result<u32>;
    #[added]
    #[breaking]
    #[method(2)]
    fn more(&mut self) -> Result<u8>;
}

#[contract("additions.thing", version = 1)]
pub trait BaseMethod {
    #[method(1)]
    fn go(&mut self, args: Base) -> Result<u32>;
    #[method(2)]
    fn more(&mut self) -> Result<u8>;
}

#[test]
fn an_added_method_does_not_move_the_base() {
    assert_eq!(
        <dyn AddedMethod as Contract>::HASH,
        <dyn OnlyBase as Contract>::HASH
    );
}

#[test]
fn an_added_method_is_counted_in_the_level_and_has_its_own_hash() {
    let info = <dyn AddedMethod as Contract>::INFO;
    assert!(info.methods[1].added);
    assert_eq!(layout::level(&info), 1);
    assert_ne!(layout::method_hash(&info.methods[1]), 0);
    assert_eq!(layout::level(&<dyn OnlyBase as Contract>::INFO), 0);
}

#[test]
fn a_breaking_added_method_moves_the_base() {
    assert!(!<dyn BreakingMethod as Contract>::INFO.methods[1].added);
    assert_eq!(
        <dyn BreakingMethod as Contract>::HASH,
        <dyn BaseMethod as Contract>::HASH
    );
    assert_ne!(
        <dyn BreakingMethod as Contract>::HASH,
        <dyn OnlyBase as Contract>::HASH
    );
    assert_eq!(layout::level(&<dyn BreakingMethod as Contract>::INFO), 0);
}

#[test]
fn an_added_field_does_not_move_the_base_or_its_method() {
    assert_eq!(
        <dyn AddedField as Contract>::HASH,
        <dyn OnlyBase as Contract>::HASH
    );
    // The method's hash covers its base shape: the argument struct gained a field, and the
    // method is the same method as before.
    assert_eq!(
        layout::method_hash(&<dyn AddedField as Contract>::INFO.methods[0]),
        layout::method_hash(&<dyn OnlyBase as Contract>::INFO.methods[0]),
    );
}

#[test]
fn an_added_field_is_found_by_its_name_and_hash() {
    let method = <dyn AddedField as Contract>::INFO.methods[0];
    let mut found = Vec::new();
    layout::added_fields(&method, &mut |name, hash| found.push((name, hash)));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, "b");
}

#[test]
fn a_breaking_added_field_moves_the_base() {
    assert_ne!(
        <dyn BreakingField as Contract>::HASH,
        <dyn OnlyBase as Contract>::HASH
    );
}
