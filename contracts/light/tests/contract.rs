use lemnos_contract_light::{Layout, Light};
use lemnos_core::{Contract, Decode, Encode, Reader, Shape, Writer};

#[test]
fn the_tag_and_numbers_are_stable() {
    let info = <dyn Light as Contract>::INFO;
    assert_eq!(info.tag.as_str(), "light/1");
    let ids: Vec<_> = info.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(
        ids,
        [
            ("layout", 1),
            ("show", 2),
            ("clear", 3),
            ("clear_all", 4),
            ("set_brightness", 5),
        ]
    );
}

#[test]
fn a_look_is_sent_inline_with_its_layer_and_priority() {
    let show = <dyn Light as Contract>::INFO.methods[1];
    let params: Vec<_> = show.params.iter().map(|p| (p.name, p.shape)).collect();
    assert_eq!(
        params,
        [
            ("layer", Shape::U8),
            ("priority", Shape::U8),
            ("look", Shape::Bytes),
        ]
    );
}

#[test]
fn a_layout_round_trips() {
    let layout = Layout {
        pixels: 24,
        layers: 4,
    };
    let mut buffer = [0; 8];
    let mut writer = Writer::new(&mut buffer);
    layout.encode(&mut writer).unwrap();
    let used = writer.len();
    assert_eq!(
        Layout::decode(&mut Reader::new(&buffer[..used])).unwrap(),
        layout
    );
}
