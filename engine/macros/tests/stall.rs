//! A job method's stall bound is declared in the contract and reaches its `INFO`; a method that is
//! not a job carries none. The bound is not part of the method's hash.

use lemnos_core::{Contract, JobId, Result, contract, layout};

#[contract("stall.thing", version = 1)]
pub trait Stalls {
    #[method(1, job, stall = 900)]
    fn long(&mut self) -> Result<JobId>;
    #[method(2, job, stall = 5)]
    fn short(&mut self) -> Result<JobId>;
    #[method(3)]
    fn read(&mut self) -> Result<u32>;
}

#[test]
fn a_job_method_declares_its_stall_bound_in_its_contract_info() {
    let methods = <dyn Stalls as Contract>::INFO.methods;
    let bound = |id: u16| methods.iter().find(|m| m.id == id).unwrap().stall_secs;
    assert_eq!(bound(1), 900);
    assert_eq!(bound(2), 5);
    assert_eq!(bound(3), 0);
}

#[test]
fn the_stall_bound_is_not_part_of_the_contract_hash() {
    // The same contract with a different bound hashes the same: the bound is host policy.
    #[allow(dead_code)]
    #[contract("stall.thing", version = 1)]
    pub trait Other {
        #[method(1, job, stall = 1)]
        fn long(&mut self) -> Result<JobId>;
        #[method(2, job, stall = 1)]
        fn short(&mut self) -> Result<JobId>;
        #[method(3)]
        fn read(&mut self) -> Result<u32>;
    }
    assert_eq!(
        layout::contract_hash(&<dyn Stalls as Contract>::INFO),
        layout::contract_hash(&<dyn Other as Contract>::INFO),
    );
}
