use lemnos_contract_fan_control::{FanControl, POINT_BYTES, Point, Settings, State};
use lemnos_core::{Contract, Decode, Encode, Reader, Shape, Status, Writer};

#[test]
fn the_tag_and_numbers_are_stable() {
    let info = <dyn FanControl as Contract>::INFO;
    assert_eq!(info.tag.as_str(), "fan-control/1");
    let ids: Vec<_> = info.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(
        ids,
        [
            ("state", 1),
            ("settings", 2),
            ("set_settings", 3),
            ("curve", 4),
            ("set_curve", 5),
            ("hold", 6),
            ("release_hold", 7),
            ("set_enabled", 8),
        ]
    );
    assert!(info.methods[3].has_out_buffer);
    assert_eq!(info.methods[4].params[1].shape, Shape::Bytes);
}

#[test]
fn a_curve_round_trips() {
    let points = [
        Point {
            value: 40,
            duty: 200,
        },
        Point {
            value: -5,
            duty: 1000,
        },
    ];
    let mut bytes = [0; 16];
    assert_eq!(Point::write_all(&points, &mut bytes), Ok(2 * POINT_BYTES));
    let back: Vec<_> = Point::read_all(&bytes).unwrap().collect();
    assert_eq!(back, points);
}

#[test]
fn a_broken_curve_is_refused() {
    assert_eq!(
        Point::read_all(&[0; 9]).err(),
        Some(Status::INVALID_ARGUMENT)
    );
    let points = [Point::default(); 2];
    assert_eq!(
        Point::write_all(&points, &mut [0; 15]),
        Err(Status::BUFFER_TOO_SMALL)
    );
}

fn round_trip<T: Encode + for<'a> Decode<'a> + PartialEq + core::fmt::Debug>(value: T) {
    let mut buffer = [0; 64];
    let mut writer = Writer::new(&mut buffer);
    value.encode(&mut writer).unwrap();
    let used = writer.len();
    assert_eq!(T::decode(&mut Reader::new(&buffer[..used])).unwrap(), value);
}

#[test]
fn records_round_trip() {
    round_trip(Settings {
        floor: 200,
        hysteresis: 3,
        slew_per_s: 50,
        kick_ms: 400,
    });
    round_trip(State {
        demand: 500,
        duty: 450,
        degraded: false,
        source: Some(1),
        held: true,
    });
}
