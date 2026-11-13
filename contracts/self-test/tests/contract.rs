//! The contract's numbers, names and byte layouts. Changing any of them is a breaking change.

use lemnos_contract_self_test::{
    LAST_BYTES, Measure, Outcome, RECORD_BYTES, Record, Spec, TEST_BYTES, Text, read_last,
    read_tests, reason, write_last, write_tests,
};
use lemnos_core::{Contract, Reader, Shape, Writer};

#[test]
fn the_tag_methods_and_numbers_are_stable() {
    let info = <dyn lemnos_contract_self_test::SelfTest as Contract>::INFO;
    assert_eq!(info.tag.as_str(), "self-test/1");
    let ids: Vec<_> = info.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(ids, [("tests", 1), ("run", 2), ("last", 3)]);
    let run = info.methods[1];
    assert!(run.job, "run is a long job");
    let names: Vec<_> = run.params.iter().map(|p| p.name).collect();
    assert_eq!(names, ["name", "confirmed"]);
    assert!(info.methods[0].has_out_buffer);
    assert!(info.methods[2].has_out_buffer);
    assert_eq!(info.methods[0].returns, Shape::Bytes);
}

#[test]
fn byte_sizes_are_pinned() {
    assert_eq!(TEST_BYTES, 249);
    assert_eq!(RECORD_BYTES, 373);
    assert_eq!(LAST_BYTES, 454);
}

#[test]
fn a_record_is_always_its_full_size_and_round_trips() {
    let record = Record::pass(reason::OK)
        .said("within limits")
        .measure(Measure::new("rpm", 2350, 0, "rpm").within(1000, 4000))
        .measure(Measure::new("bus", 12345, -3, "V"));
    let mut buffer = [0u8; RECORD_BYTES + 10];
    let mut writer = Writer::new(&mut buffer);
    record.write(&mut writer).unwrap();
    let used = writer.len();
    assert_eq!(used, RECORD_BYTES);
    let back = Record::read(&mut Reader::new(&buffer[..used])).unwrap();
    assert_eq!(back, record);
    assert_eq!(back.outcome, Outcome::Pass);
    assert_eq!(back.detail.as_str(), "within limits");
    let measures: Vec<_> = back.measures().collect();
    assert_eq!(measures.len(), 2);
    assert_eq!(measures[0].low, Some(1000));
    assert_eq!(measures[0].high, Some(4000));
    assert_eq!(measures[1].low, None, "a value without limits says so");
    assert_eq!(measures[1].exponent, -3);
    assert_eq!(measures[1].unit.as_str(), "V");
}

#[test]
fn the_byte_layout_is_as_documented() {
    let record = Record::fail(reason::OUT_OF_RANGE).measure(Measure::new("x", -2, 0, ""));
    let mut buffer = [0u8; RECORD_BYTES];
    let mut writer = Writer::new(&mut buffer);
    record.write(&mut writer).unwrap();
    assert_eq!(buffer[0], 1, "outcome: fail");
    assert_eq!(&buffer[1..3], &8u16.to_le_bytes(), "reason: out of range");
    assert_eq!(buffer[3], 0, "empty detail");
    assert_eq!(buffer[3 + 1 + 96], 1, "one measure");
    let first = 3 + 1 + 96 + 1;
    assert_eq!(buffer[first], 1, "measure name length");
    assert_eq!(buffer[first + 1], b'x');
    let value = first + 1 + 32;
    assert_eq!(&buffer[value..value + 8], &(-2i64).to_le_bytes());
    let flags = first + 68 - 1;
    assert_eq!(buffer[flags], 0, "no limits");
}

#[test]
fn a_record_that_does_not_fit_is_refused_by_a_fixed_writer() {
    let mut small = [0u8; 10];
    let mut writer = Writer::new(&mut small);
    assert!(Record::pass(0).write(&mut writer).is_err());
}

#[test]
fn malformed_records_are_refused() {
    let mut bytes = [0u8; RECORD_BYTES];
    bytes[0] = 9;
    assert!(
        Record::read(&mut Reader::new(&bytes)).is_err(),
        "unknown outcome"
    );

    let mut bytes = [0u8; RECORD_BYTES];
    let count_at = 3 + 1 + 96;
    bytes[count_at] = 5;
    assert!(
        Record::read(&mut Reader::new(&bytes)).is_err(),
        "more than four measures"
    );

    let mut bytes = [0u8; RECORD_BYTES];
    bytes[count_at] = 0;
    bytes[count_at + 1 + 68] = 1;
    assert!(
        Record::read(&mut Reader::new(&bytes)).is_err(),
        "an unused measure slot that is not empty"
    );

    let mut bytes = [0u8; RECORD_BYTES];
    bytes[3] = 2;
    bytes[4] = 0xff;
    assert!(
        Record::read(&mut Reader::new(&bytes)).is_err(),
        "text that is not UTF-8"
    );
}

#[test]
fn text_is_cut_at_its_size_and_never_inside_a_character() {
    assert_eq!(Text::<3>::new("abcdef").as_str(), "abc");
    assert_eq!(
        Text::<3>::new("aé").as_str(),
        "aé",
        "three bytes fit exactly"
    );
    assert_eq!(
        Text::<2>::new("aé").as_str(),
        "a",
        "the second byte of é is not cut into"
    );
    assert_eq!(Text::<8>::empty().as_str(), "");
}

#[test]
fn names_are_checked() {
    assert!(Spec::valid_name("identity"));
    assert!(Spec::valid_name("bus_voltage_2"));
    assert!(!Spec::valid_name(""));
    assert!(!Spec::valid_name("Identity"));
    assert!(!Spec::valid_name("a-b"));
    assert!(!Spec::valid_name("all"), "all is the board's name");
    assert!(!Spec::valid_name(&"a".repeat(33)));
    assert!(Spec::valid_name(&"a".repeat(32)));
}

#[test]
fn the_tests_list_round_trips_in_order() {
    const SPECS: [Spec; 2] = [
        Spec::read("identity", "the chip ID reads 0x32", 20),
        Spec::intrusive("spin", "the fan spins", "the fan free to spin", 3000),
    ];
    let mut buffer = [0u8; 1 + 2 * TEST_BYTES];
    let used = write_tests(&SPECS, &mut buffer).unwrap();
    assert_eq!(used, 1 + 2 * TEST_BYTES);
    let entries: Vec<_> = read_tests(&buffer[..used]).unwrap().collect();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name.as_str(), "identity");
    assert!(!entries[0].intrusive);
    assert_eq!(entries[1].name.as_str(), "spin");
    assert!(entries[1].intrusive);
    assert_eq!(entries[1].fixture.as_str(), "the fan free to spin");
    assert_eq!(entries[1].expect_ms, 3000);
}

#[test]
fn a_tests_list_that_does_not_fit_is_refused() {
    const SPECS: [Spec; 1] = [Spec::read("identity", "the chip ID", 20)];
    let mut small = [0u8; TEST_BYTES];
    assert!(write_tests(&SPECS, &mut small).is_err());
}

#[test]
fn the_last_list_round_trips_and_names_each_result() {
    let pass = Record::pass(reason::OK);
    let skip = Record::skipped(reason::ABSENT).said("no tachometer");
    let mut buffer = [0u8; 1 + 2 * LAST_BYTES];
    let used = write_last(&[("identity", pass), ("spin", skip)], &mut buffer).unwrap();
    assert_eq!(used, 1 + 2 * LAST_BYTES);
    let back: Vec<_> = read_last(&buffer[..used]).unwrap().collect();
    assert_eq!(back.len(), 2);
    assert_eq!(back[0].name.as_str(), "identity");
    assert_eq!(back[0].record, pass);
    assert_eq!(back[1].record, skip);
}
