use lemnos_contract_lens::{FocusDirection, Lens, Range};
use lemnos_core::{Contract, Decode, Encode, Reader, Shape, Writer};

#[test]
fn the_tag_and_numbers_are_stable() {
    let info = <dyn Lens as Contract>::INFO;
    assert_eq!(info.tag.as_str(), "lens/1");
    let ids: Vec<_> = info.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(
        ids,
        [
            ("range", 1),
            ("set_position", 2),
            ("position", 3),
            ("direction", 4),
            ("settle_us", 5),
        ]
    );
    assert_eq!(info.methods[1].params[0].shape, Shape::U16);
    assert_eq!(info.methods[4].params[0].shape, Shape::U16);
}

#[test]
fn a_range_round_trips() {
    let range = Range { min: 0, max: 1023 };
    let mut buffer = [0; 8];
    let mut writer = Writer::new(&mut buffer);
    range.encode(&mut writer).unwrap();
    let used = writer.len();
    assert_eq!(used, 4 + 2 + 2);
    assert_eq!(
        Range::decode(&mut Reader::new(&buffer[..used])).unwrap(),
        range
    );
}

#[test]
fn every_direction_round_trips_and_unknown_tags_are_refused() {
    for (direction, byte) in [
        (FocusDirection::Unknown, 0u8),
        (FocusDirection::TowardNear, 1),
        (FocusDirection::TowardFar, 2),
    ] {
        let mut buffer = [0; 1];
        let mut writer = Writer::new(&mut buffer);
        direction.encode(&mut writer).unwrap();
        assert_eq!(buffer[0], byte);
        assert_eq!(
            FocusDirection::decode(&mut Reader::new(&buffer)).unwrap(),
            direction
        );
    }
    assert!(FocusDirection::decode(&mut Reader::new(&[3])).is_err());
    assert_eq!(FocusDirection::default(), FocusDirection::Unknown);
}
