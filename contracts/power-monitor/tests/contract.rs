use lemnos_contract_power_monitor::{PowerMonitor, PowerReading};
use lemnos_core::{Contract, Decode, Encode, Reader, Shape, Writer};

#[test]
fn the_tag_and_numbers_are_stable() {
    let info = <dyn PowerMonitor as Contract>::INFO;
    assert_eq!(info.tag.as_str(), "power-monitor/1");
    let ids: Vec<_> = info.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(ids, [("read", 1)]);
    let Shape::Record(fields) = info.methods[0].returns else {
        panic!("a reading is a record");
    };
    let names: Vec<_> = fields.iter().map(|f| f.name).collect();
    assert_eq!(names, ["bus_v", "shunt_v", "current_a", "power_w"]);
}

#[test]
fn a_reading_round_trips() {
    let reading = PowerReading {
        bus_v: 12.1,
        shunt_v: 0.02,
        current_a: -1.5,
        power_w: 18.15,
    };
    let mut buffer = [0; 32];
    let mut writer = Writer::new(&mut buffer);
    reading.encode(&mut writer).unwrap();
    let used = writer.len();
    assert_eq!(used, 16);
    let decoded = PowerReading::decode(&mut Reader::new(&buffer[..used])).unwrap();
    assert_eq!(decoded, reading);
}
