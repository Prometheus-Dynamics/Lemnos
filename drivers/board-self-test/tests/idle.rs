//! A board with no members costs nothing while nothing runs: its ticks allocate nothing.

use lemnos_core::{Config, ConfigValue, Driver, Provider, Resources, Status, Tag};
use lemnos_driver_board_self_test::BoardSelfTest;
use lemnos_measure::test_alloc::{Counting, allocations};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A board with no devices to ask: a member is never named, so none is asked for.
struct NoDevices;

impl Provider for NoDevices {
    fn provided(&self, _need: &str) -> bool {
        false
    }

    fn device(&mut self, _need: &str, _tag: Tag) -> lemnos_core::Result<lemnos_core::LocalCaller> {
        Err(Status::NOT_FOUND)
    }

    /// No device on the board serves the contract.
    fn members(&mut self, _tag: Tag, _found: &mut dyn FnMut(&str)) -> lemnos_core::Result<()> {
        Ok(())
    }
}

#[test]
fn an_idle_board_allocates_nothing_on_its_ticks() {
    let mut provider = NoDevices;
    let mut resources = Resources::new(&mut provider, None);
    let entries = [("members", ConfigValue::Str("all"))];
    let mut board = match BoardSelfTest::bind(&mut resources, &Config::new(&entries)) {
        Ok(board) => board,
        Err(status) => panic!("bind refused: {status:?}"),
    };
    let ((), count) = allocations(|| {
        for at in 0..1000u64 {
            Driver::tick(&mut board, at * 1_000_000);
        }
    });
    assert_eq!(count, 0, "allocations in 1000 idle ticks");
}
