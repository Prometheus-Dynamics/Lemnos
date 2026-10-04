use super::*;

#[test]
fn builds_output_requests() {
    // Styx's OV9782 power lines: one plain output, two active-low.
    let lines = [
        (5, LineSettings::output(true)),
        (7, LineSettings::output(false).active_low()),
        (9, LineSettings::output(true).active_low()),
    ];
    let req = build_request("styx-ov9782", &lines).unwrap();
    assert_eq!(&req.offsets[..3], &[5, 7, 9]);
    assert_eq!(req.num_lines, 3);
    assert_eq!(c_string(&req.consumer), "styx-ov9782");
    assert_eq!(req.config.flags, flag::OUTPUT);
    assert_eq!(req.config.num_attrs, 2);
    let flags = req.config.attrs[0];
    assert_eq!(flags.attr.id, attr::FLAGS);
    assert_eq!(flags.attr.value, flag::OUTPUT | flag::ACTIVE_LOW);
    assert_eq!(flags.mask, 0b110);
    let values = req.config.attrs[1];
    assert_eq!(values.attr.id, attr::OUTPUT_VALUES);
    assert_eq!((values.attr.value, values.mask), (0b101, 0b111));
}

#[test]
fn builds_input_requests_with_bias_edges_and_debounce() {
    let button = LineSettings::input()
        .with_bias(LineBias::PullUp)
        .with_edge(LineEdge::Falling)
        .with_debounce_us(5000);
    let req = build_request("lemnos", &[(3, button), (4, LineSettings::output(true))]).unwrap();
    assert_eq!(
        req.config.flags,
        flag::INPUT | flag::BIAS_PULL_UP | flag::EDGE_FALLING
    );
    let attrs = &req.config.attrs[..req.config.num_attrs as usize];
    assert!(
        attrs
            .iter()
            .any(|a| a.attr.id == attr::DEBOUNCE && a.attr.value == 5000 && a.mask == 0b01)
    );
    assert!(
        attrs
            .iter()
            .any(|a| a.attr.id == attr::FLAGS && a.attr.value == flag::OUTPUT && a.mask == 0b10)
    );
    assert!(
        attrs
            .iter()
            .any(|a| a.attr.id == attr::OUTPUT_VALUES && a.attr.value == 0b10 && a.mask == 0b10)
    );
}

#[test]
fn rejects_bad_requests() {
    let req = build_request(&"x".repeat(40), &[(0, LineSettings::input())]).unwrap();
    assert_eq!(c_string(&req.consumer).len(), 31);
    assert!(build_request("c", &[]).is_err());
    let bad = LineSettings::output(false).with_edge(LineEdge::Rising);
    assert!(build_request("c", &[(0, bad)]).is_err());
    let bad = LineSettings::input().with_drive(LineDrive::OpenDrain);
    assert!(bad.flags().is_err());
    let many: Vec<(u32, LineSettings)> = (0..12)
        .map(|i| (i, LineSettings::input().with_debounce_us(100 + i)))
        .collect();
    assert!(build_request("c", &many).is_err());
}

#[test]
fn decodes_line_info() {
    let mut raw = sys::LineInfo {
        name: [0; sys::MAX_NAME_SIZE],
        consumer: [0; sys::MAX_NAME_SIZE],
        offset: 4,
        num_attrs: 1,
        flags: flag::USED | flag::INPUT | flag::BIAS_PULL_DOWN | flag::EDGE_RISING,
        attrs: [sys::LineAttribute::default(); sys::NUM_ATTRS_MAX],
        padding: [0; 4],
    };
    raw.name[..8].copy_from_slice(b"CAM_GPIO");
    raw.attrs[0] = sys::LineAttribute {
        id: attr::DEBOUNCE,
        padding: 0,
        value: 250,
    };
    let info = GpioLineInfo::from_raw(&raw);
    assert_eq!(info.name, "CAM_GPIO");
    assert!(info.used);
    assert_eq!(info.settings.bias, LineBias::PullDown);
    assert_eq!(info.settings.edge, LineEdge::Rising);
    assert_eq!(info.settings.debounce_us, Some(250));
}

/// Reads chip and line info from every chip present, read-only.
#[test]
fn reads_present_chips() {
    let Ok(paths) = GpioChip::paths() else { return };
    for path in paths {
        let Ok(chip) = GpioChip::open(&path) else {
            continue;
        };
        if chip.num_lines() > 0 {
            assert_eq!(chip.line_info(0).unwrap().offset, 0);
        }
    }
}
