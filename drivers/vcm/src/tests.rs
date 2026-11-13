//! The chip layer, and the driver on a simulated bus: the chip's registers are
//! the sim's bytes.

use super::*;
use crate::chip::{Chip, DEFAULT_ADDRESS};
use lemnos_core::{Contract, Description, Driver, Resources};
use lemnos_sim::{Layout, SimBoard, SimI2c};

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

const ADDRESS: u8 = DEFAULT_ADDRESS;

/// A board with one bus, `bus`, and a chip at the default address.
fn board(layout: Layout) -> (SimBoard, SimI2c) {
    let board = SimBoard::new();
    let sim = board.add_i2c("bus");
    sim.attach(ADDRESS, layout);
    (board, sim)
}

/// Binds the driver to `board` with the given config entries.
fn bind(board: &mut SimBoard, entries: &[(&str, ConfigValue<'_>)]) -> Result<Vcm> {
    let config = Config::new(entries);
    let mut res = Resources::new(board, None);
    Vcm::bind(&mut res, &config)
}

/// The status binding refuses `entries` with, if it refuses them.
fn refused(board: &mut SimBoard, entries: &[(&str, ConfigValue<'_>)]) -> Option<Status> {
    bind(board, entries).err()
}

fn chip_config(name: &str) -> [(&str, ConfigValue<'_>); 1] {
    [("chip", ConfigValue::Str(name))]
}

#[test]
fn chip_messages_match_the_datasheets() {
    let msg = |chip: Chip, position: u16| {
        let (message, written) = chip.format().encode(position);
        (message.as_slice().to_vec(), written)
    };
    // DW9714: 0x1ff << 4, no register.
    assert_eq!(msg(Chip::Dw9714, 0x1ff), (vec![0x1f, 0xf0], 0x1ff));
    assert_eq!(msg(Chip::Dw9807, 0x2a5), (vec![0x03, 0x02, 0xa5], 0x2a5));
    assert_eq!(msg(Chip::Dw9807, 5000), (vec![0x03, 0x03, 0xff], 1023));
    assert_eq!(msg(Chip::Dw9817, 0), (vec![0x03, 0x00, 0x00], 0));
    assert_eq!(msg(Chip::Ak7375, 0xabc), (vec![0x00, 0xab, 0xc0], 0xabc));
    assert_eq!(msg(Chip::Ak7375, 9000), (vec![0x00, 0xff, 0xf0], 4095));
    assert_eq!(Chip::Ak7375.format().max_position(), 4095);
    assert_eq!(Chip::Dw9714.format().max_position(), 1023);
    assert_eq!(Chip::from_name("DW9817"), Some(Chip::Dw9817));
    assert_eq!(Chip::from_name("imx708"), None);
    assert_eq!(Chip::from_name("custom"), None);
}

#[test]
fn dw9817_powers_up_moves_and_clamps() {
    let (mut board, sim) = board(Layout::Bytes);
    let mut lens = bind(&mut board, &chip_config("dw9817")).unwrap();
    assert_eq!(
        Driver::health(&lens),
        Health::Degraded("not powered: powers up on the next move")
    );
    assert_eq!(lens.position(), Err(Status::NOT_SUPPORTED));

    // First move: power-up only, then the host's next call writes.
    assert_eq!(lens.set_position(445), Err(Status::AGAIN));
    assert_eq!(sim.byte(ADDRESS, 0x02), Some(0x00));
    assert_eq!(sim.byte(ADDRESS, 0x03), None);
    assert_eq!(
        Driver::health(&lens),
        Health::Degraded("powering up: the next move writes the position")
    );

    assert_eq!(lens.set_position(445), Ok(()));
    assert_eq!(sim.byte(ADDRESS, 0x03), Some(0x01));
    assert_eq!(sim.byte(ADDRESS, 0x04), Some(0xbd));
    assert_eq!(lens.position(), Ok(445));
    assert_eq!(Driver::health(&lens), Health::Ok);

    lens.set_position(2000).unwrap();
    assert_eq!(lens.position(), Ok(1023));
    assert_eq!(sim.byte(ADDRESS, 0x03), Some(0x03));
    assert_eq!(sim.byte(ADDRESS, 0x04), Some(0xff));
    // Power-up plus two moves: one transaction each.
    assert_eq!(sim.transactions(), 3);
}

#[test]
fn ak7375_powers_up_and_moves_with_a_register() {
    let (mut board, sim) = board(Layout::Bytes);
    let mut lens = bind(&mut board, &chip_config("ak7375")).unwrap();
    assert_eq!(lens.set_position(0), Err(Status::AGAIN));
    assert_eq!(sim.byte(ADDRESS, 0x02), Some(0x00));
    lens.set_position(0xabc).unwrap();
    assert_eq!(sim.byte(ADDRESS, 0x00), Some(0xab));
    assert_eq!(sim.byte(ADDRESS, 0x01), Some(0xc0));
    assert_eq!(lens.range(), Ok(Range { min: 0, max: 4095 }));
}

#[test]
fn dw9714_moves_without_a_register() {
    // The DW9714 takes its value as the whole message. The simulated chip
    // keeps the first byte as its pointer and the second as the word there.
    let (mut board, sim) = board(Layout::Words);
    let mut lens = bind(&mut board, &chip_config("dw9714")).unwrap();
    // No power-up writes, but the first call still hands the host a retry.
    assert_eq!(lens.set_position(0x1ff), Err(Status::AGAIN));
    assert_eq!(sim.transactions(), 0);
    lens.set_position(0x1ff).unwrap();
    assert_eq!(sim.word(ADDRESS, 0x1f).as_deref(), Some(&[0xf0][..]));
    assert_eq!(sim.transactions(), 1);
}

#[test]
fn power_down_writes_the_standby_messages() {
    let (_board, sim) = board(Layout::Bytes);
    let mut bus = sim.clone();
    chip::power_down(&mut bus, ADDRESS, &Chip::Dw9807.format()).unwrap();
    assert_eq!(sim.byte(ADDRESS, 0x02), Some(0x01));
    chip::power_down(&mut bus, ADDRESS, &Chip::Ak7375.format()).unwrap();
    assert_eq!(sim.byte(ADDRESS, 0x02), Some(0x40));
}

#[test]
fn range_comes_from_config_and_clamps_moves() {
    let (mut board, sim) = board(Layout::Bytes);
    let mut lens = bind(
        &mut board,
        &[
            ("chip", ConfigValue::Str("dw9807")),
            ("min", ConfigValue::Int(100)),
            ("max", ConfigValue::Int(600)),
        ],
    )
    .unwrap();
    assert_eq!(lens.range(), Ok(Range { min: 100, max: 600 }));
    assert_eq!(lens.set_position(0), Err(Status::AGAIN));
    lens.set_position(50).unwrap();
    assert_eq!(lens.position(), Ok(100));
    assert_eq!(sim.byte(ADDRESS, 0x04), Some(100));
    lens.set_position(700).unwrap();
    assert_eq!(lens.position(), Ok(600));
    assert_eq!(sim.byte(ADDRESS, 0x03), Some(0x02));
    assert_eq!(sim.byte(ADDRESS, 0x04), Some(0x58));
}

#[test]
fn range_defaults_to_the_chip() {
    let (mut board, _sim) = board(Layout::Bytes);
    let mut lens = bind(&mut board, &chip_config("dw9817")).unwrap();
    assert_eq!(lens.range(), Ok(Range { min: 0, max: 1023 }));
}

#[test]
fn config_rejects_bad_chips_ranges_and_addresses() {
    let (mut board, _sim) = board(Layout::Bytes);
    let board = &mut board;
    assert_eq!(refused(board, &[]), Some(Status::INVALID_ARGUMENT));
    assert_eq!(
        refused(board, &chip_config("imx708")),
        Some(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        refused(board, &chip_config("custom")),
        Some(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        refused(board, &[("chip", ConfigValue::Int(3))]),
        Some(Status::INVALID_ARGUMENT)
    );
    // Above the DW9807's 10 bits.
    assert_eq!(
        refused(
            board,
            &[
                ("chip", ConfigValue::Str("dw9807")),
                ("max", ConfigValue::Int(1024)),
            ]
        ),
        Some(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        refused(
            board,
            &[
                ("chip", ConfigValue::Str("dw9807")),
                ("min", ConfigValue::Int(700)),
                ("max", ConfigValue::Int(600)),
            ]
        ),
        Some(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        refused(
            board,
            &[
                ("chip", ConfigValue::Str("dw9807")),
                ("address", ConfigValue::Int(0x80)),
            ]
        ),
        Some(Status::INVALID_ARGUMENT)
    );
}

#[test]
fn bus_fault_reports_and_powers_up_again_on_the_next_move() {
    let (mut board, sim) = board(Layout::Bytes);
    let mut lens = bind(&mut board, &chip_config("dw9807")).unwrap();
    assert_eq!(lens.set_position(300), Err(Status::AGAIN));
    lens.set_position(300).unwrap();

    // A move that fails reports on that call; the position stays the last
    // one written.
    sim.set_dead(ADDRESS, true);
    assert_eq!(lens.set_position(500), Err(Status::IO));
    assert_eq!(lens.position(), Ok(300));
    assert_eq!(
        Driver::health(&lens),
        Health::Degraded("bus error: powers up again on the next move")
    );

    // Still dead: the power-up fails on the call, and the driver keeps trying.
    assert_eq!(lens.set_position(500), Err(Status::IO));

    // The chip answers again: power-up, then the move.
    sim.set_dead(ADDRESS, false);
    assert_eq!(lens.set_position(500), Err(Status::AGAIN));
    lens.set_position(500).unwrap();
    assert_eq!(lens.position(), Ok(500));
    assert_eq!(sim.byte(ADDRESS, 0x03), Some(0x01));
    assert_eq!(sim.byte(ADDRESS, 0x04), Some(0xf4));
    assert_eq!(Driver::health(&lens), Health::Ok);
}

#[test]
fn an_absent_chip_is_a_nack_until_it_answers() {
    let mut board = SimBoard::new();
    let sim = board.add_i2c("bus");
    let mut lens = bind(&mut board, &chip_config("dw9817")).unwrap();
    assert_eq!(lens.set_position(1), Err(Status::IO));
    sim.attach(ADDRESS, Layout::Bytes);
    assert_eq!(lens.set_position(1), Err(Status::AGAIN));
    lens.set_position(1).unwrap();
}

#[test]
fn manifest_provides_the_lens_contract() {
    let tags: Vec<_> = <Vcm as Driver>::MANIFEST
        .provides
        .iter()
        .map(|info| info.tag.as_str())
        .collect();
    assert_eq!(tags, [<dyn Lens as Contract>::INFO.tag.as_str()]);
    assert_eq!(tags, ["lens/1"]);
}

#[test]
fn describes_the_position() {
    let (mut board, _sim) = board(Layout::Bytes);
    let lens = bind(&mut board, &chip_config("dw9817")).unwrap();
    let mut out = Description::new();
    Driver::describe(&lens, &mut out);
    assert_eq!(out.items().count(), 1);
    let item = out.find("position").unwrap();
    assert_eq!(item.unit, Unit::One);
}

#[test]
fn direction_and_settle_are_unknown_for_every_chip_unless_the_board_says() {
    for chip in ["dw9714", "dw9807", "dw9817", "ak7375"] {
        let (mut board, _sim) = board(Layout::Bytes);
        let mut lens = bind(&mut board, &chip_config(chip)).unwrap();
        assert_eq!(lens.direction(), Ok(FocusDirection::Unknown), "{chip}");
        assert_eq!(lens.settle_us(1), Ok(None), "{chip}");
        assert_eq!(lens.settle_us(1023), Ok(None), "{chip}");
    }
}

#[test]
fn the_board_gives_the_direction_and_one_settle_time_for_every_move() {
    for (word, expected) in [
        ("near", FocusDirection::TowardNear),
        ("Far", FocusDirection::TowardFar),
        ("unknown", FocusDirection::Unknown),
    ] {
        let (mut board, _sim) = board(Layout::Bytes);
        let mut lens = bind(
            &mut board,
            &[
                ("chip", ConfigValue::Str("dw9807")),
                ("direction", ConfigValue::Str(word)),
                ("settle_us", ConfigValue::Int(12_000)),
            ],
        )
        .unwrap();
        assert_eq!(lens.direction(), Ok(expected), "{word}");
        assert_eq!(lens.settle_us(0), Ok(Some(12_000)));
        assert_eq!(lens.settle_us(1023), Ok(Some(12_000)));
    }
}

#[test]
fn a_zero_settle_time_is_a_figure_not_an_unknown() {
    let (mut board, _sim) = board(Layout::Bytes);
    let mut lens = bind(
        &mut board,
        &[
            ("chip", ConfigValue::Str("ak7375")),
            ("settle_us", ConfigValue::Int(0)),
        ],
    )
    .unwrap();
    assert_eq!(lens.settle_us(5), Ok(Some(0)));
}

#[test]
fn a_bad_direction_or_settle_time_is_refused() {
    let (mut board, _sim) = board(Layout::Bytes);
    let board = &mut board;
    let with =
        |extra: (&'static str, ConfigValue<'static>)| [("chip", ConfigValue::Str("dw9817")), extra];
    for entry in [
        ("direction", ConfigValue::Str("sideways")),
        ("direction", ConfigValue::Int(1)),
        ("settle_us", ConfigValue::Int(-1)),
        ("settle_us", ConfigValue::Int(1 << 32)),
        ("settle_us", ConfigValue::Str("10")),
    ] {
        assert_eq!(
            refused(board, &with(entry)),
            Some(Status::INVALID_ARGUMENT),
            "{entry:?}"
        );
    }
}
