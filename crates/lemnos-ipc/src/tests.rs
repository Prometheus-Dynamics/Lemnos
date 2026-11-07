use super::*;
use lemnos_device::{
    Axis, CalibrationCommand, CalibrationPart, CalibrationRoutine, CalibrationStatus, DeviceClass,
    DeviceStatus, Quantity,
};

fn round_trip_request(request: Request) {
    let frame = request.encode();
    let (decoded, used) = decode_request(&frame).unwrap().unwrap();
    assert_eq!(used, frame.len());
    assert_eq!(decoded, request);
    assert!(decode_request(&frame[..frame.len() - 1]).unwrap().is_none());
}

fn round_trip_message(message: Message) {
    let frame = message.encode();
    let (decoded, used) = decode_message(&frame).unwrap().unwrap();
    assert_eq!(used, frame.len());
    assert_eq!(decoded, message);
}

#[test]
fn requests_round_trip() {
    round_trip_request(Request::WatchFrames {
        id: 7,
        device: "ring".into(),
        fps: 20,
    });
    round_trip_request(Request::LightSetting {
        id: 8,
        device: "ring".into(),
        look_brightness: 250,
        persist: true,
    });
    round_trip_request(Request::Looks {
        id: 9,
        op: LooksOp::PresetSave {
            name: "night".into(),
            text: "[looks.\"detect.targets\"]\n".into(),
        },
    });
    round_trip_request(Request::Looks {
        id: 10,
        op: LooksOp::Delete("detect.searching".into()),
    });
    round_trip_request(Request::Hello {
        version: VERSION,
        client: "app".into(),
        priority: 80,
        keep: true,
        events: false,
    });
    round_trip_request(Request::List);
    round_trip_request(Request::Stats);
    round_trip_request(Request::AllocStats);
    round_trip_request(Request::Read {
        id: 6,
        device: "imu".into(),
    });
    round_trip_request(Request::Subscribe {
        id: 9,
        device: "imu".into(),
        period_ms: 10,
    });
    round_trip_request(Request::SubscribeChannels {
        id: 10,
        device: "imu".into(),
        channels: vec!["angular_rate.z".into(), "acceleration.*".into()],
        period_ms: 10,
    });
    round_trip_request(Request::Set {
        id: 7,
        device: "fan".into(),
        control: "duty".into(),
        value: 0.8,
    });
    round_trip_request(Request::Get {
        id: 8,
        device: "fan".into(),
        control: "duty".into(),
    });
    let mut led = LedRequest::new(LedShow::Status(LedStatus::Warn));
    led.effect = Some(EffectKind::Breathe);
    led.period_ms = Some(1500);
    led.depth = Some(400);
    led.brightness = Some(500);
    led.fade_ms = Some(250);
    led.easing = Some(Easing::CubicBezier {
        x1: 420,
        y1: 0,
        x2: 580,
        y2: 1000,
    });
    led.duration_ms = Some(10_000);
    round_trip_request(Request::Led(led));
    round_trip_request(Request::Led(
        LedRequest::new(LedShow::Frame(vec![0xff0000, 0x00ff00])).test(),
    ));
    round_trip_request(Request::Led(LedRequest::new(LedShow::Clear).test()));
    round_trip_request(Request::Release {
        id: 9,
        device: "fan".into(),
    });
    // A frame from a peer that predates the flags and the reply id: not a
    // test intent, and no reply is asked for.
    let mut old = Request::Led(LedRequest::new(LedShow::Color(0x123456))).encode();
    old.truncate(old.len() - 5);
    let length = u32::from_le_bytes(old[..4].try_into().unwrap()) - 5;
    old[..4].copy_from_slice(&length.to_le_bytes());
    match decode_request(&old).unwrap().unwrap().0 {
        Request::Led(led) => {
            assert!(!led.test && led.id == 0 && led.show == LedShow::Color(0x123456));
        }
        other => panic!("{other:?}"),
    }
    for show in [
        LedShow::Clear,
        LedShow::Color(0x00ff00),
        LedShow::Frame(vec![0xff00_0000, 0x0000_00ff]),
        LedShow::Pixels(vec![(0, 0xff0000), (15, 0x0000ff)]),
        LedShow::Progress {
            fraction: 500,
            color: Some(0x00ff00),
            background: None,
        },
        LedShow::Indeterminate { color: None },
        LedShow::System(SystemState::Updating {
            progress: Some(250),
            phase: Phase::Writing,
        }),
        LedShow::System(SystemState::RolledBack),
        LedShow::System(SystemState::Confirmed),
        LedShow::Orbit {
            color: Some(0x8a5cff),
            tail: Some(6_500),
            heads: 2,
            base: Some(60),
        },
        LedShow::Orbit {
            color: None,
            tail: None,
            heads: 1,
            base: None,
        },
        LedShow::Locate,
    ] {
        round_trip_request(Request::Led(LedRequest::new(show)));
    }
}

#[test]
fn subscribe_from_an_older_client_has_no_id() {
    // What protocol 1 clients sent before the id: kind 4, device, period.
    let mut payload = Vec::new();
    payload.extend_from_slice(&3u16.to_le_bytes());
    payload.extend_from_slice(b"imu");
    payload.extend_from_slice(&10u32.to_le_bytes());
    let mut frame = Vec::new();
    frame.extend_from_slice(&(payload.len() as u32 + 2).to_le_bytes());
    frame.extend_from_slice(&4u16.to_le_bytes());
    frame.extend_from_slice(&payload);
    let (decoded, used) = decode_request(&frame).unwrap().unwrap();
    assert_eq!(used, frame.len());
    assert_eq!(
        decoded,
        Request::Subscribe {
            id: 0,
            device: "imu".into(),
            period_ms: 10,
        }
    );
}

#[test]
fn messages_round_trip() {
    round_trip_message(Message::Reply {
        id: 9,
        result: Err(Refusal::Unsupported),
    });
    round_trip_message(Message::Reply {
        id: 9,
        result: Ok(10.0),
    });
    round_trip_message(Message::Welcome {
        version: VERSION,
        board: "example".into(),
        client_id: 3,
        features: crate::FEATURE_GPU_LEASE,
    });
    round_trip_message(Message::AllocStats(None));
    round_trip_message(Message::AllocStats(Some(crate::AllocStats {
        allocs: 1_204,
        frees: 1_180,
        bytes: 393_216,
        live_bytes: 61_440,
    })));
    round_trip_message(Message::Stats(vec![
        ReadStats {
            device: "imu".into(),
            reads: 6201,
            total_us: 6201 * 180,
            max_us: 2_400,
            last_us: 170,
            misses: 412,
        },
        ReadStats {
            device: "power".into(),
            reads: 0,
            total_us: 0,
            max_us: 0,
            last_us: 0,
            misses: 0,
        },
    ]));
    round_trip_message(Message::Devices(vec![DeviceDesc {
        id: "imu".into(),
        label: "IMU".into(),
        class: DeviceClass::Imu,
        model: "BMI088".into(),
        status: DeviceStatus::Faulted,
        reason: "init: BMI088 accel power control: bus error".into(),
        channels: vec![ChannelDesc {
            name: "acceleration.x".into(),
            quantity: Quantity::Acceleration,
            axis: Axis::X,
            exponent: -3,
        }],
        controls: vec![ControlDesc {
            name: "duty".into(),
            quantity: Quantity::Ratio,
            exponent: -3,
            min: 0,
            max: 1000,
        }],
        pixels: 0,
    }]));
    round_trip_message(Message::Reading(RawReading {
        device: "imu".into(),
        timestamp_us: 123_456,
        status: DeviceStatus::Degraded,
        values: vec![9_806, i32::MIN],
        id: 0,
    }));
    round_trip_message(Message::Reading(RawReading {
        device: "imu".into(),
        timestamp_us: 7,
        status: DeviceStatus::Available,
        values: vec![1],
        id: 11,
    }));
    round_trip_message(Message::Reply {
        id: 4,
        result: Ok(0.702),
    });
    round_trip_message(Message::Reply {
        id: 5,
        result: Err(Refusal::Device(lemnos_hal::ErrorKind::Timeout)),
    });
    round_trip_message(Message::Event(Event::Control {
        device: "fan".into(),
        control: "duty".into(),
        value: 0.5,
        by: "app".into(),
    }));
    round_trip_message(Message::Event(Event::Status {
        device: "imu".into(),
        status: DeviceStatus::Missing,
        error: Some(lemnos_hal::ErrorKind::Nack),
        reason: "init: BMI088 accel chip id: bus error: nack".into(),
    }));
    round_trip_message(Message::Event(Event::LedOwner {
        device: "ring".into(),
        owner: "vision".into(),
        layer: "status".into(),
    }));
}

#[test]
fn malformed_frames_are_errors() {
    assert!(decode_request(&[1, 0, 0, 0, 0]).is_err());
    assert!(decode_request(&[2, 0, 0, 0, 99, 0]).is_err());
    let mut frame = Request::Read {
        id: 0,
        device: "imu".into(),
    }
    .encode();
    frame.truncate(8);
    frame[0] = 4;
    assert!(decode_request(&frame).is_err());
}

#[test]
fn raw_requests_and_answers_round_trip() {
    use crate::raw::{Bias, EdgeDetect, LineConfig, Polarity, PwmConfig, SafeState, SpiMode};
    round_trip_request(Request::Raw(RawRequest::LineClaim {
        id: 1,
        line: LineTarget::Chip {
            chip: "pinctrl-rp1".into(),
            offset: 5,
        },
        config: LineConfig::input()
            .with_bias(Bias::PullUp)
            .with_edge(EdgeDetect::Falling)
            .with_debounce_us(500),
        on_release: Some(SafeState::Low),
    }));
    round_trip_request(Request::Raw(RawRequest::LineClaim {
        id: 2,
        line: LineTarget::Name("aux".into()),
        config: LineConfig::output(true).active_low(),
        on_release: None,
    }));
    for raw in [
        RawRequest::LineConfigure {
            id: 3,
            handle: 7,
            config: LineConfig::high_impedance(),
        },
        RawRequest::LineGet { id: 4, handle: 7 },
        RawRequest::LineSet {
            id: 5,
            handle: 7,
            value: true,
        },
        RawRequest::PwmClaim {
            id: 6,
            pwm: PwmTarget::Chip {
                chip: 0,
                channel: 1,
            },
        },
        RawRequest::PwmClaim {
            id: 7,
            pwm: PwmTarget::Name("buzzer".into()),
        },
        RawRequest::PwmConfigure {
            id: 8,
            handle: 2,
            config: PwmConfig {
                period_ns: 1_000_000,
                duty_ns: 1,
                polarity: Polarity::Inversed,
                enabled: true,
            },
        },
        RawRequest::Unclaim { id: 9, handle: 2 },
        RawRequest::I2cTransfer {
            id: 10,
            bus: "i2c:compatible=i2c-gpio".into(),
            address: 0x50,
            ops: vec![I2cOp::Write(vec![0x10]), I2cOp::Read(2)],
        },
        RawRequest::I2cLock {
            id: 11,
            bus: "1".into(),
            address: 0x50,
            lock: true,
        },
        RawRequest::SpiTransfer {
            id: 12,
            bus: 0,
            chip_select: 1,
            transfers: vec![{
                let mut t = SpiXfer::new(vec![0x9f], 3);
                t.config.mode = SpiMode::Mode3;
                t.config.speed_hz = 8_000_000;
                t.cs_change = true;
                t.delay_us = 10;
                t
            }],
        },
        RawRequest::SpiLock {
            id: 13,
            bus: 0,
            chip_select: 1,
            lock: false,
        },
    ] {
        round_trip_request(Request::Raw(raw));
    }
    round_trip_request(Request::Restore {
        id: 14,
        device: "fan".into(),
        control: String::new(),
    });
    round_trip_message(Message::Claimed {
        id: 1,
        result: Ok(42),
    });
    round_trip_message(Message::Claimed {
        id: 2,
        result: Err(Refusal::Owned),
    });
    round_trip_message(Message::Data {
        id: 3,
        result: Ok(vec![1, 2, 3]),
    });
    round_trip_message(Message::Data {
        id: 4,
        result: Err(Refusal::Claimed),
    });
    round_trip_message(Message::Event(Event::Edge {
        handle: 7,
        rising: false,
        timestamp_ns: 123_456_789,
        seq: 3,
    }));
    round_trip_message(Message::Event(Event::Dropped { count: 9 }));
    round_trip_message(Message::Reply {
        id: 5,
        result: Err(Refusal::UnknownHandle),
    });
    // A greeting from before the events flag reads events.
    let mut old = Request::Hello {
        version: VERSION,
        client: "old".into(),
        priority: 1,
        keep: false,
        events: false,
    }
    .encode();
    old.pop();
    let length = u32::from_le_bytes(old[..4].try_into().unwrap()) - 1;
    old[..4].copy_from_slice(&length.to_le_bytes());
    assert!(matches!(
        decode_request(&old).unwrap().unwrap().0,
        Request::Hello { events: true, .. }
    ));
}

#[test]
fn looks_round_trip_by_name_inline_and_in_text() {
    use lemnos_light::{Block, Effect, Fraction, LayerSpec, LookName, LookSpec, Mode, Rgbw};
    let mut spec = LookSpec::EMPTY;
    spec.push(LayerSpec::comet(
        Rgbw::new(0x11, 0x22, 0x33, 0x44),
        1600,
        6000,
        2,
        60,
    ));
    spec.push(
        LayerSpec::arc(Fraction::Input, Rgbw::rgb(0x2f7bff), Rgbw::rgb(0x101012))
            .with_brightness(64)
            .with_mode(Mode::Add),
    );
    spec.push(LayerSpec::frame(&[
        Rgbw::rgb(0xff0000),
        Rgbw::rgb(0x00ff00),
    ]));
    spec.push(LayerSpec::ripple(Rgbw::rgb(0x2bd47d)));
    spec.envelope = Effect::Breathe {
        period_ms: 4000,
        depth: 180,
        easing: Easing::CubicBezier {
            x1: 420,
            y1: -100,
            x2: 580,
            y2: 1000,
        },
    };
    spec.brightness = 178;
    spec.floor = 16;
    let mut inline = LedRequest::new(LedShow::Inline {
        spec: Box::new(spec),
        progress: Some(500),
    });
    inline.id = 77;
    inline.duration_ms = Some(30_000);
    round_trip_request(Request::Led(inline));
    let mut named = LedRequest::new(LedShow::Look {
        name: LookName::new("detect.targets")
            .unwrap()
            .as_str()
            .to_string(),
        progress: None,
    });
    named.id = 78;
    round_trip_request(Request::Led(named));
    // Every block and envelope kind round-trips too.
    let mut blinking = LookSpec::of(LayerSpec::fill(Rgbw::rgb(0xff0000)));
    blinking.envelope = Effect::Blink {
        period_ms: 900,
        duty: 300,
    };
    round_trip_request(Request::Led(LedRequest::new(LedShow::Inline {
        spec: Box::new(blinking),
        progress: None,
    })));
    let mut arc = LookSpec::of(LayerSpec::new(Block::Arc {
        fraction: Fraction::Fixed(250),
        color: Rgbw::rgb(0x00ff40),
        track: Rgbw::OFF,
        head: 350,
        sheen: false,
    }));
    arc.envelope = Effect::Solid;
    round_trip_request(Request::Led(LedRequest::new(LedShow::Inline {
        spec: Box::new(arc),
        progress: None,
    })));
    let mut flash = LookSpec::fill(Rgbw::rgb(0x00ff20));
    flash.envelope = Effect::Pulse {
        attack_ms: 60,
        hold_ms: 600,
        decay_ms: 1200,
        repeat: 1,
    };
    round_trip_request(Request::Led(LedRequest::new(LedShow::Inline {
        spec: Box::new(flash),
        progress: None,
    })));
    // A wash under a falling sparkle (the confirmed look), and a sparkle with a palette.
    let mut sparks = LookSpec::of(LayerSpec::new(Block::Wash {
        color: Rgbw::rgb(0xc8ffd2),
        effect: Effect::Pulse {
            attack_ms: 80,
            hold_ms: 0,
            decay_ms: 500,
            repeat: 1,
        },
    }));
    sparks.push(LayerSpec::new(Block::Sparkle(lemnos_light::Sparkle {
        colors: [
            Rgbw::rgb(0x00ff20),
            Rgbw::rgb(0x00ff20),
            Rgbw::rgb(0xc8ffd2),
            Rgbw::rgb(0x78ff8c),
        ],
        count: 4,
        density: 3_500,
        density_end: 0,
        fade_ms: 2_600,
        start_ms: 300,
        min_ms: 350,
        max_ms: 950,
        base: 250,
        seed: 9,
        fall: true,
        fall_speed: 5_000,
        fall_accel: 6_000,
    })));
    round_trip_request(Request::Led(LedRequest::new(LedShow::Inline {
        spec: Box::new(sparks),
        progress: None,
    })));
    round_trip_request(Request::Looks {
        id: 5,
        op: LooksOp::List,
    });
    round_trip_request(Request::Looks {
        id: 6,
        op: LooksOp::Show("detect.targets".into()),
    });
    round_trip_request(Request::Looks {
        id: 7,
        op: LooksOp::Reload,
    });
    round_trip_request(Request::Looks {
        id: 8,
        op: LooksOp::Save {
            name: "app.saved".into(),
            text: "[looks.\"app.saved\"]\nlayers = []\n".into(),
        },
    });
    round_trip_message(Message::LightInfo(LightInfo {
        device: "ring".into(),
        count: 16,
        offset: 5,
        clockwise: true,
        look: "detect.targets".into(),
        layer: "app".into(),
        owner: "manager".into(),
        look_brightness: 250,
    }));
    round_trip_message(Message::Text {
        id: 8,
        result: Ok(String::new()),
    });
    round_trip_message(Message::Text {
        id: 9,
        result: Err("unknown look \"nope\"".into()),
    });
}

#[test]
fn a_look_with_no_layers_decodes_so_the_service_can_answer_it() {
    use lemnos_light::LookSpec;
    // The encoder writes a look with no layers as a count of 0. The decoder
    // takes it, so the service refuses the look with a reply (it needs a
    // layer) instead of closing the connection on a decode error.
    let empty = Request::Led(LedRequest::new(LedShow::Inline {
        spec: Box::new(LookSpec::EMPTY),
        progress: None,
    }))
    .encode();
    assert!(decode_request(&empty).unwrap().is_some());
}

#[test]
fn a_drain_round_trips_on_the_wire() {
    use lemnos_light::{Block, Easing, LayerSpec, LookSpec, Rgbw};
    let drain = LookSpec::of(LayerSpec::new(Block::Drain {
        color: Rgbw::rgb(0x00ff20),
        fill_ms: 700,
        start_ms: 1_100,
        duration_ms: 2_100,
        easing: Easing::EaseIn,
    }));
    round_trip_request(Request::Led(LedRequest::new(LedShow::Inline {
        spec: Box::new(drain),
        progress: None,
    })));
}

#[test]
fn calibration_requests_round_trip() {
    for command in [
        CalibrationCommand::Start(CalibrationRoutine::AccelSix),
        CalibrationCommand::Start(CalibrationRoutine::MagRotate),
        CalibrationCommand::Start(CalibrationRoutine::GyroHold),
        CalibrationCommand::Stop,
        CalibrationCommand::Apply,
        CalibrationCommand::Discard,
        CalibrationCommand::Reset,
    ] {
        round_trip_request(Request::Calibration {
            id: 4,
            device: "imu".into(),
            command,
        });
    }
    round_trip_request(Request::CalibrationStatus {
        id: 5,
        device: "magnetometer".into(),
    });
}

#[test]
fn calibration_status_message_round_trips() {
    let part = CalibrationPart {
        samples: 1234,
        confidence: 800,
        coverage: 700,
        residual: 15,
        active: true,
    };
    round_trip_message(Message::CalibrationStatus {
        id: 5,
        device: "imu".into(),
        status: CalibrationStatus {
            revision: 42,
            running: Some(CalibrationRoutine::MagRotate),
            progress: 500,
            candidate: true,
            failed: false,
            parts: [part, CalibrationPart::default(), part],
        },
    });
    round_trip_message(Message::CalibrationStatus {
        id: 0,
        device: "x".into(),
        status: CalibrationStatus::default(),
    });
}

#[test]
fn bad_calibration_codes_are_refused() {
    // A frame of kind CALIBRATION (12) whose command code is unknown.
    let mut frame = Vec::new();
    let payload = [0u8, 0, 0, 0, 1, 0, b'x', 9];
    frame.extend_from_slice(&((payload.len() + 2) as u32).to_le_bytes());
    frame.extend_from_slice(&12u16.to_le_bytes());
    frame.extend_from_slice(&payload);
    assert!(decode_request(&frame).is_err());
}

#[test]
fn encode_reading_matches_the_message_frame() {
    let values = [9_806, i32::MIN, 0];
    let message = Message::Reading(RawReading {
        device: "imu".into(),
        timestamp_us: 42,
        status: DeviceStatus::Available,
        values: values.to_vec(),
        id: 0,
    });
    assert_eq!(
        crate::wire::encode_reading("imu", 42, DeviceStatus::Available, &values),
        message.encode()
    );
    // Into a buffer that held a longer frame: replaced, not appended to.
    let mut frame = vec![0xaa; 100];
    crate::wire::encode_reading_into(&mut frame, "imu", 42, DeviceStatus::Available, &values);
    assert_eq!(frame, message.encode());
    assert!(frame.capacity() >= 100, "the buffer was kept");
}

#[test]
fn requests_over_the_request_limit_are_refused_before_their_body() {
    // The length alone decides: a request frame this long is refused even
    // though its body has not arrived.
    let length = (crate::wire::MAX_REQUEST_FRAME + 1) as u32;
    let mut header = length.to_le_bytes().to_vec();
    header.extend_from_slice(&3u16.to_le_bytes());
    let error = decode_request(&header).unwrap_err();
    assert!(error.0.contains("at most"), "{error}");
    // A reply of the same size is still a valid message length.
    let mut reply = length.to_le_bytes().to_vec();
    reply.extend_from_slice(&104u16.to_le_bytes());
    assert!(matches!(decode_message(&reply), Ok(None)));
}

#[test]
fn a_raw_transfer_at_the_largest_size_fits_the_request_limit() {
    let request = Request::Raw(RawRequest::SpiTransfer {
        id: 1,
        bus: 0,
        chip_select: 0,
        transfers: vec![SpiXfer::new(vec![0xA5; 64 * 1024], 0)],
    });
    let frame = request.encode();
    assert!(
        frame.len() <= crate::wire::MAX_REQUEST_FRAME,
        "{}",
        frame.len()
    );
    let (decoded, used) = decode_request(&frame).unwrap().unwrap();
    assert_eq!((decoded, used), (request, frame.len()));
}

#[test]
fn spots_looks_and_input_updates_round_trip_on_the_wire() {
    use lemnos_light::{Block, LayerSpec, LookSpec, Rgbw, Spot, SpotList, SpotSource, SpotsSpec};
    let spots = SpotList::new(&[
        Spot::new(250, 1_000),
        Spot::new(750, 600).with_color(Rgbw::rgb(0xff0000)),
    ]);
    // A named look with spots, and a look in full with them.
    round_trip_request(Request::Led(LedRequest::new(LedShow::LookSpots {
        name: "detect.spots".into(),
        progress: Some(400),
        spots,
    })));
    let inline = LookSpec::of(LayerSpec::new(Block::Spots(SpotsSpec {
        width: 2_000,
        follow_ms: 200,
        max_spots: 4,
        ..SpotsSpec::new(Rgbw::rgb(0x28c8ff), SpotSource::Input)
    })));
    round_trip_request(Request::Led(LedRequest::new(LedShow::InlineSpots {
        spec: Box::new(inline),
        progress: None,
        spots,
    })));
    // A fixed spots block in the look, with a colour of its own on a spot.
    let fixed = LookSpec::of(LayerSpec::spots(
        Rgbw::rgb(0xff8000),
        SpotSource::Fixed(SpotList::new(&[Spot::new(500, 1_000)])),
    ));
    round_trip_request(Request::Led(LedRequest::new(LedShow::Inline {
        spec: Box::new(fixed),
        progress: None,
    })));
    // The update: both inputs, one, the other, and none.
    for (progress, spots) in [
        (Some(650), Some(spots)),
        (None, Some(spots)),
        (Some(0), None),
        (None, None),
    ] {
        round_trip_request(Request::LedInputs {
            id: 12,
            device: "ring".into(),
            progress,
            spots,
        });
    }
}

#[test]
fn a_spots_look_has_its_own_codes_and_the_old_ones_are_unchanged() {
    use lemnos_light::{LayerSpec, LookSpec, Rgbw, Spot, SpotList};
    // The show's code follows the frame's length and kind (4 + 2 bytes) and
    // the device name (a u16 length, empty: 2 bytes): byte 8. A named look is
    // 10, an inline one 11; the spots forms are 12 and 13, appended.
    let code = |show: LedShow| Request::Led(LedRequest::new(show)).encode()[8];
    assert_eq!(
        code(LedShow::Look {
            name: "detect.searching".into(),
            progress: None,
        }),
        10
    );
    let fill = LookSpec::of(LayerSpec::fill(Rgbw::rgb(0)));
    assert_eq!(
        code(LedShow::Inline {
            spec: Box::new(fill),
            progress: None,
        }),
        11
    );
    let spots = SpotList::new(&[Spot::new(0, 1_000)]);
    assert_eq!(
        code(LedShow::LookSpots {
            name: "x".into(),
            progress: None,
            spots,
        }),
        12
    );
    assert_eq!(
        code(LedShow::InlineSpots {
            spec: Box::new(LookSpec::of(LayerSpec::fill(Rgbw::rgb(0)))),
            progress: None,
            spots,
        }),
        13
    );
}

#[test]
fn a_truncated_spot_list_is_a_protocol_error() {
    use lemnos_light::{Block, LayerSpec, LookSpec, Rgbw, Spot, SpotList, SpotSource, SpotsSpec};
    let spec = LookSpec::of(LayerSpec::new(Block::Spots(SpotsSpec::new(
        Rgbw::rgb(0x00ff00),
        SpotSource::Fixed(SpotList::new(&[Spot::new(1, 2)])),
    ))));
    let frame = Request::Led(LedRequest::new(LedShow::Inline {
        spec: Box::new(spec),
        progress: None,
    }))
    .encode();
    // Cut inside the spots block, and fix up the frame length so the codec
    // sees a complete frame whose body is short.
    let body = &frame[..frame.len() - 12];
    let mut short = body.to_vec();
    let len = (short.len() - 4) as u32;
    short[..4].copy_from_slice(&len.to_le_bytes());
    assert!(decode_request(&short).is_err() || decode_request(&short).unwrap().is_none());
}

/// A one-connection fake service: answers `Hello` with a `Welcome` of
/// `version` and `List` with no devices, then sends `after_list` raw, and
/// reports whether the client then closed its end.
fn fake_service(
    name: &str,
    version: u16,
    after_list: Vec<u8>,
) -> (std::path::PathBuf, std::thread::JoinHandle<bool>) {
    use std::io::{Read, Write};
    let dir = std::env::temp_dir().join(format!("lemnos-ipc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            while let Ok(Some((request, used))) = decode_request(&buf) {
                buf.drain(..used);
                let answer = match request {
                    Request::Hello { .. } => Message::Welcome {
                        version,
                        board: "fake".into(),
                        client_id: 1,
                        features: 0,
                    },
                    Request::List => Message::Devices(Vec::new()),
                    _ => continue,
                };
                stream.write_all(&answer.encode()).unwrap();
                if matches!(answer, Message::Devices(_)) {
                    stream.write_all(&after_list).unwrap();
                }
            }
            match stream.read(&mut chunk) {
                Ok(0) => return true,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => return false,
            }
        }
    });
    (path, handle)
}

#[test]
fn a_welcome_of_another_protocol_version_is_refused() {
    let (path, service) = fake_service("version", VERSION + 1, Vec::new());
    match ClientOptions::new(&path, "test").devices() {
        Err(ClientError::Protocol(e)) => assert!(e.0.contains("protocol 2"), "{e}"),
        Err(other) => panic!("expected a protocol error, got {other}"),
        Ok(_) => panic!("expected a protocol error, got a client"),
    }
    assert!(service.join().unwrap(), "the client kept the connection");
}

#[test]
fn a_bad_frame_drops_the_connection_instead_of_failing_forever() {
    // A frame of an unknown message kind.
    let (path, service) = fake_service("bad-frame", VERSION, vec![2, 0, 0, 0, 0xee, 0xee]);
    let mut client = ClientOptions::new(&path, "test")
        .timeout(std::time::Duration::from_secs(2))
        .devices()
        .unwrap();
    match client.read("imu") {
        Err(ClientError::Protocol(_)) => {}
        other => panic!("expected a protocol error, got {other:?}"),
    }
    assert!(service.join().unwrap(), "the client kept the connection");
}

#[test]
fn an_inline_look_with_no_layers_round_trips() {
    round_trip_request(Request::Led(LedRequest::new(LedShow::Inline {
        spec: Box::new(lemnos_light::LookSpec::EMPTY),
        progress: None,
    })));
}

#[test]
fn an_overlong_string_is_cut_at_a_character_boundary() {
    // 65 535 falls inside a two-byte character here.
    let device = format!("aa{}", "é".repeat(40_000));
    let frame = Request::Read { id: 0, device }.encode();
    let (decoded, used) = decode_request(&frame).unwrap().unwrap();
    assert_eq!(used, frame.len());
    let Request::Read { device, .. } = decoded else {
        panic!("not a Read: {decoded:?}");
    };
    assert_eq!(device.len(), 65_534);
}

#[test]
fn raw_tags_other_than_zero_and_one_are_refused() {
    use crate::raw::LineConfig;
    // Length, kind and id come first: the target kind is byte 10.
    let mut frame = Request::Raw(RawRequest::LineClaim {
        id: 2,
        line: LineTarget::Name("aux".into()),
        config: LineConfig::output(true),
        on_release: None,
    })
    .encode();
    assert_eq!(frame[10], 1);
    frame[10] = 2;
    assert!(decode_request(&frame).is_err());

    // Then the bus string, address and op count: the first op tag follows.
    let bus = "1";
    let mut frame = Request::Raw(RawRequest::I2cTransfer {
        id: 10,
        bus: bus.into(),
        address: 0x50,
        ops: vec![I2cOp::Read(2)],
    })
    .encode();
    let tag = 10 + 2 + bus.len() + 2 + 2;
    assert_eq!(frame[tag], 1);
    frame[tag] = 2;
    assert!(decode_request(&frame).is_err());
}

#[test]
fn device_reasons_of_the_wrong_count_are_refused() {
    let device = DeviceDesc {
        id: "imu".into(),
        label: "IMU".into(),
        class: DeviceClass::Imu,
        model: "BMI088".into(),
        status: DeviceStatus::Faulted,
        reason: "x".into(),
        channels: Vec::new(),
        controls: Vec::new(),
        pixels: 0,
    };
    let mut frame = Message::Devices(vec![device]).encode();
    // The tail is the reason count, then one string: u16 1, u16 1, "x".
    let count = frame.len() - 5;
    assert_eq!(frame[count], 1);
    frame[count] = 2;
    assert!(decode_message(&frame).is_err());
}

#[test]
fn optional_values_never_meet_their_sentinel() {
    let mut led = LedRequest::new(LedShow::Clear);
    led.period_ms = Some(u32::MAX);
    led.depth = Some(u16::MAX);
    let (decoded, _) = decode_request(&Request::Led(led).encode())
        .unwrap()
        .unwrap();
    let Request::Led(led) = decoded else {
        panic!("not a Led: {decoded:?}");
    };
    assert_eq!(led.period_ms, Some(u32::MAX - 1));
    assert_eq!(led.depth, Some(u16::MAX - 1));
    assert_eq!(led.brightness, None);
}

#[test]
fn a_read_without_its_appended_id_reads_as_id_zero() {
    let old = crate::wire::encode_reading("imu", 1, DeviceStatus::Available, &[]);
    let (decoded, _) = decode_message(&old).unwrap().unwrap();
    assert!(matches!(
        decoded,
        Message::Reading(RawReading { id: 0, .. })
    ));
    // A read request from an older client: the device alone.
    let mut frame = Request::Read {
        id: 9,
        device: "imu".into(),
    }
    .encode();
    frame.truncate(frame.len() - 4);
    frame[0] -= 4;
    let (decoded, _) = decode_request(&frame).unwrap().unwrap();
    assert_eq!(
        decoded,
        Request::Read {
            id: 0,
            device: "imu".into()
        }
    );
}

#[test]
fn a_frame_encoded_into_a_reused_buffer_is_the_messages_frame() {
    let frame = crate::LightFrame {
        device: "status-ring".into(),
        seq: 7,
        pixels: vec![0x00ff_0000, 0x0000_ff00, 0x1100_00ff],
    };
    let mut buf = vec![0xaa; 300];
    crate::encode_frame_into(
        &mut buf,
        &frame.device,
        frame.seq,
        frame.pixels.iter().copied(),
    );
    assert_eq!(buf, Message::Frame(frame).encode());
}

#[test]
fn a_device_list_encoded_from_views_is_the_messages_frame() {
    use lemnos_device::{Channel, ControlInfo};
    static CHANNELS: [Channel; 2] = [
        Channel::new("temperature", Quantity::Temperature, -3),
        Channel::new("duty", Quantity::Ratio, -3),
    ];
    static CONTROLS: [ControlInfo; 1] = [ControlInfo::new("duty", Quantity::Ratio, -3, 0, 1000)];
    let views = [
        crate::DeviceView {
            id: "fan",
            label: "Fan",
            class: DeviceClass::Fan,
            model: "hwmon-fan",
            status: DeviceStatus::Available,
            reason: "",
            channels: &CHANNELS,
            controls: &CONTROLS,
            pixels: 0,
        },
        crate::DeviceView {
            id: "ring",
            label: "ring",
            class: DeviceClass::Other,
            model: "ws2812",
            status: DeviceStatus::Missing,
            reason: "no /dev/leds0",
            channels: &[],
            controls: &[],
            pixels: 12,
        },
    ];
    let descs: Vec<DeviceDesc> = views
        .iter()
        .map(|v| DeviceDesc {
            id: v.id.into(),
            label: v.label.into(),
            class: v.class,
            model: v.model.into(),
            status: v.status,
            reason: v.reason.into(),
            channels: v
                .channels
                .iter()
                .map(|c| ChannelDesc {
                    name: c.name.into(),
                    quantity: c.quantity,
                    axis: c.axis,
                    exponent: c.exponent,
                })
                .collect(),
            controls: v
                .controls
                .iter()
                .map(|c| ControlDesc {
                    name: c.name.into(),
                    quantity: c.quantity,
                    exponent: c.exponent,
                    min: c.min,
                    max: c.max,
                })
                .collect(),
            pixels: v.pixels,
        })
        .collect();
    let mut buf = vec![0xaa; 500];
    crate::encode_devices_into(&mut buf, views.iter().copied());
    assert_eq!(buf, Message::Devices(descs).encode());
}
