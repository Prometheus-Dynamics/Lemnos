use lemnos_contract_pixels::{MAX_LOCAL_LEDS, Pixels};
use lemnos_core::{Contract, LOCAL_ARGS, Shape};

const _: () = assert!(2 + 4 + 4 * MAX_LOCAL_LEDS <= LOCAL_ARGS);
const _: () = assert!(2 + 4 + 4 * (MAX_LOCAL_LEDS + 1) > LOCAL_ARGS);

#[test]
fn the_tag_and_numbers_are_stable() {
    let info = <dyn Pixels as Contract>::INFO;
    assert_eq!(info.tag.as_str(), "pixels/1");
    let ids: Vec<_> = info.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(ids, [("pixel_count", 1), ("set", 2), ("show", 3)]);
}

#[test]
fn the_shapes_are_a_count_a_placed_frame_and_nothing() {
    let info = <dyn Pixels as Contract>::INFO;
    assert_eq!(info.methods[0].returns, Shape::U16);
    assert_eq!(info.methods[1].params[0].name, "offset");
    assert_eq!(info.methods[1].params[0].shape, Shape::U16);
    assert_eq!(info.methods[1].params[1].name, "pixels");
    assert_eq!(info.methods[1].params[1].shape, Shape::Bytes);
    assert!(info.methods[2].params.is_empty());
}

#[test]
fn a_local_set_holds_fourteen_leds() {
    assert_eq!(MAX_LOCAL_LEDS, 14);
}
