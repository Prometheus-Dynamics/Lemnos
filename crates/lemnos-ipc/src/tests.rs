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
    });
    round_trip_request(Request::List);
    round_trip_request(Request::Read {
        device: "imu".into(),
    });
    round_trip_request(Request::Subscribe {
        device: "imu".into(),
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
    // A frame from a peer that predates the flags byte: not a test intent.
    let mut old = Request::Led(LedRequest::new(LedShow::Color(0x123456))).encode();
    old.pop();
    let length = u32::from_le_bytes(old[..4].try_into().unwrap()) - 1;
    old[..4].copy_from_slice(&length.to_le_bytes());
    match decode_request(&old).unwrap().unwrap().0 {
        Request::Led(led) => assert!(!led.test && led.show == LedShow::Color(0x123456)),
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
        LedShow::Locate,
    ] {
        round_trip_request(Request::Led(LedRequest::new(show)));
    }
}

#[test]
fn messages_round_trip() {
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
        status: DeviceStatus::Available,
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
