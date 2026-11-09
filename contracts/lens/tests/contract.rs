use lemnos_contract_lens::{Lens, Range};
use lemnos_core::{Contract, Decode, Encode, Reader, Shape, Writer};

#[test]
fn the_tag_and_numbers_are_stable() {
    let info = <dyn Lens as Contract>::INFO;
    assert_eq!(info.tag.as_str(), "lens/1");
    let ids: Vec<_> = info.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(ids, [("range", 1), ("set_position", 2), ("position", 3)]);
    assert_eq!(info.methods[1].params[0].shape, Shape::U16);
}

#[test]
fn a_range_round_trips() {
    let range = Range { min: 0, max: 1023 };
    let mut buffer = [0; 4];
    let mut writer = Writer::new(&mut buffer);
    range.encode(&mut writer).unwrap();
    let used = writer.len();
    assert_eq!(
        Range::decode(&mut Reader::new(&buffer[..used])).unwrap(),
        range
    );
}
