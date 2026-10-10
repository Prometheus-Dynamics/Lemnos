use super::*;
use lemnos_device::{Axis, DeviceClass, DeviceStatus, Quantity};

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
    round_trip_request(Request::Hello {
        version: VERSION,
        client: "helios".into(),
        priority: 80,
        keep: true,
        events: false,
    });
    round_trip_request(Request::List);
    round_trip_request(Request::Read {
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
        board: "raze".into(),
        client_id: 3,
    });
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
        by: "helios".into(),
    }));
    round_trip_message(Message::Event(Event::Status {
        device: "imu".into(),
        status: DeviceStatus::Missing,
        error: Some(lemnos_hal::ErrorKind::Nack),
        reason: "init: BMI088 accel chip id: bus error: nack".into(),
    }));
    round_trip_message(Message::Event(Event::LedOwner {
        device: "ring".into(),
        owner: "photonvision".into(),
        layer: "status".into(),
    }));
}

#[test]
fn malformed_frames_are_errors() {
    assert!(decode_request(&[1, 0, 0, 0, 0]).is_err());
    assert!(decode_request(&[2, 0, 0, 0, 99, 0]).is_err());
    let mut frame = Request::Read {
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
        name: LookName::new("pv.targets").unwrap().as_str().to_string(),
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
        op: LooksOp::Show("pv.targets".into()),
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
fn a_look_with_no_layers_or_an_unknown_block_is_a_protocol_error() {
    use lemnos_light::LookSpec;
    // A look with no layers has no shape on the wire: refused.
    let empty = Request::Led(LedRequest::new(LedShow::Inline {
        spec: Box::new(LookSpec::EMPTY),
        progress: None,
    }))
    .encode();
    assert!(decode_request(&empty).is_err());
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
