use lemnos_contract_system_slots::Slots;
use lemnos_core::{Contract, Shape};

#[test]
fn the_tag_and_numbers_are_stable() {
    let info = <dyn Slots as Contract>::INFO;
    assert_eq!(info.tag.as_str(), "system.slots/1");
    let ids: Vec<_> = info.methods.iter().map(|m| (m.name, m.id, m.job)).collect();
    assert_eq!(
        ids,
        [
            ("list", 1, false),
            ("install", 2, true),
            ("write", 3, false),
            ("set_default", 4, false),
            ("trial", 5, false),
            ("confirm", 6, false),
            ("remove", 7, false),
            ("pin", 8, false),
            ("set_keep", 9, false),
            ("accept", 10, false),
            ("install_file", 11, true),
            ("boots", 12, false),
            ("list_page", 13, false),
            ("info", 14, false),
        ]
    );
    let method = |name: &str| info.methods.iter().find(|m| m.name == name).unwrap();
    assert!(method("list").has_out_buffer);
    assert!(method("boots").has_out_buffer, "boots writes into a buffer");
    let page = method("list_page");
    assert!(page.has_out_buffer);
    assert_eq!(page.params[0].shape, Shape::Str);
    assert!(method("info").has_out_buffer);
    assert_eq!(info.methods[1].returns, Shape::U64);
    assert_eq!(info.methods[2].params[1].shape, Shape::Bytes);
    assert_eq!(info.methods[3].params[0].shape, Shape::Str);
}
