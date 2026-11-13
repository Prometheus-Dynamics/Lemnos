//! A contract's encoding hash comes from its method signatures and the argument and result types'
//! layouts, so a struct that gains a field moves it, and an unchanged one does not.

use lemnos_core::{Contract, Decode, Encode, Result, contract};

#[derive(Debug, Encode, Decode)]
pub struct Args {
    pub a: u32,
}

#[derive(Debug, Encode, Decode)]
pub struct ArgsWithField {
    pub a: u32,
    pub b: u8,
}

#[contract("hashed.thing", version = 1)]
pub trait Before {
    #[method(1)]
    fn go(&mut self, args: Args) -> Result<u32>;
}

#[contract("hashed.thing", version = 1)]
pub trait Same {
    #[method(1)]
    fn go(&mut self, args: Args) -> Result<u32>;
}

#[contract("hashed.thing", version = 1)]
pub trait After {
    #[method(1)]
    fn go(&mut self, args: ArgsWithField) -> Result<u32>;
}

#[test]
fn an_unchanged_contract_hashes_the_same_on_both_sides() {
    assert_eq!(<dyn Before as Contract>::HASH, <dyn Same as Contract>::HASH);
}

#[test]
fn a_field_added_to_an_argument_struct_moves_the_contract_hash() {
    assert_ne!(
        <dyn Before as Contract>::HASH,
        <dyn After as Contract>::HASH
    );
}
