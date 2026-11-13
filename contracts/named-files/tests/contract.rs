use lemnos_contract_named_files::NamedFiles;
use lemnos_core::{Contract, Shape};

#[test]
fn the_tag_and_numbers_are_stable() {
    let info = <dyn NamedFiles as Contract>::INFO;
    assert_eq!(info.tag.as_str(), "lemnos.files/1");
    let ids: Vec<_> = info.methods.iter().map(|m| (m.name, m.id, m.job)).collect();
    assert_eq!(
        ids,
        [
            ("index", 1, false),
            ("open", 2, false),
            ("size", 3, false),
            ("read", 4, false),
            ("close", 5, false),
        ]
    );
    assert!(info.methods[0].has_out_buffer);
    assert!(info.methods[3].has_out_buffer);
    assert_eq!(info.methods[1].params[0].shape, Shape::Str);
    assert_eq!(info.methods[2].returns, Shape::U64);
}
