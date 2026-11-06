//! Pins the test vectors in `docs/ipc.md` to the crate's encoder. Each vector
//! is a fenced `hex` block after a `<!-- vector: name -->` marker. The test
//! builds the value with the crate's types, encodes it, and compares the bytes
//! with the doc's hex; it then decodes the doc's bytes and checks they give the
//! same value back. A vector added to the doc without a pin here fails, and a
//! pin without a vector in the doc fails too.

use crate::wire::{
    ChannelDesc, DeviceDesc, Event, FEATURE_GPU_LEASE, GpuAcquire, GpuLeaseState, GpuMode, I2cOp,
    LedRequest, LedShow, Message, RawReading, RawRequest, Refusal, Request, VERSION,
    decode_message, decode_request, encode_reading,
};
use lemnos_device::{Axis, DeviceClass, DeviceStatus, NO_VALUE, Quantity};
use lemnos_hal::ErrorKind;

const SPEC: &str = include_str!("../../../docs/ipc.md");

/// Every vector the doc must carry, and that this module pins.
const PINNED: &[&str] = &[
    "hello",
    "subscribe",
    "i2c-transfer",
    "led-look",
    "welcome",
    "reading",
    "reply-refused",
    "reply-ok",
    "devices",
    "event-control",
    "data-reply",
    "gpu-acquire",
    "gpu-lease",
];

/// The bytes of the doc's vector `name`.
fn vector_bytes(name: &str) -> Vec<u8> {
    let marker = format!("<!-- vector: {name} -->");
    let start = SPEC
        .find(&marker)
        .unwrap_or_else(|| panic!("docs/ipc.md has no vector {name:?}"));
    let after = &SPEC[start + marker.len()..];
    let open = "```hex\n";
    let body_start = after
        .find(open)
        .unwrap_or_else(|| panic!("vector {name:?}: no hex block after its marker"))
        + open.len();
    let body = &after[body_start..];
    let body = &body[..body
        .find("```")
        .unwrap_or_else(|| panic!("vector {name:?}: hex block not closed"))];
    body.split_whitespace()
        .map(|h| {
            u8::from_str_radix(h, 16)
                .unwrap_or_else(|_| panic!("vector {name:?}: {h:?} is not a hex byte"))
        })
        .collect()
}

/// The names of every vector marker in the doc, in order.
fn documented() -> Vec<&'static str> {
    SPEC.lines()
        .filter_map(|line| {
            line.strip_prefix("<!-- vector: ")
                .and_then(|rest| rest.strip_suffix(" -->"))
        })
        .collect()
}

fn pin_request(name: &str, request: Request) {
    let bytes = request.encode();
    assert_eq!(
        bytes,
        vector_bytes(name),
        "{name}: encoder output differs from docs/ipc.md"
    );
    let (decoded, used) = decode_request(&bytes)
        .unwrap_or_else(|e| panic!("{name}: {e}"))
        .unwrap_or_else(|| panic!("{name}: the whole frame did not decode"));
    assert_eq!(used, bytes.len(), "{name}: bytes used");
    assert_eq!(decoded, request, "{name}: decode(doc bytes) differs");
    assert!(
        decode_request(&bytes[..bytes.len() - 1]).unwrap().is_none(),
        "{name}: a short frame must wait for more bytes"
    );
}

fn pin_message(name: &str, message: Message) {
    let bytes = message.encode();
    assert_eq!(
        bytes,
        vector_bytes(name),
        "{name}: encoder output differs from docs/ipc.md"
    );
    let (decoded, used) = decode_message(&vector_bytes(name))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
        .unwrap_or_else(|| panic!("{name}: the whole frame did not decode"));
    assert_eq!(used, bytes.len(), "{name}: bytes used");
    assert_eq!(decoded, message, "{name}: decode(doc bytes) differs");
}

#[test]
fn every_documented_vector_is_pinned() {
    let mut doc = documented();
    doc.sort_unstable();
    let mut pinned = PINNED.to_vec();
    pinned.sort_unstable();
    assert_eq!(
        doc, pinned,
        "vectors in docs/ipc.md and pinned in spec_vectors.rs"
    );
    assert!(pinned.len() >= 8, "the spec carries at least 8 vectors");
}

#[test]
fn request_vectors_match_the_encoder() {
    pin_request(
        "hello",
        Request::Hello {
            version: VERSION,
            client: "example".into(),
            priority: 50,
            keep: false,
            events: true,
        },
    );
    pin_request(
        "subscribe",
        Request::Subscribe {
            id: 7,
            device: "imu".into(),
            period_ms: 10,
        },
    );
    pin_request(
        "i2c-transfer",
        Request::Raw(RawRequest::I2cTransfer {
            id: 5,
            bus: "i2c-1".into(),
            address: 0x48,
            ops: vec![I2cOp::Write(vec![0x00]), I2cOp::Read(2)],
        }),
    );
    pin_request(
        "led-look",
        Request::Led(LedRequest {
            device: String::new(),
            show: LedShow::Look {
                name: "arc".into(),
                progress: Some(250),
            },
            effect: None,
            period_ms: None,
            depth: None,
            brightness: Some(500),
            fade_ms: None,
            easing: None,
            duration_ms: Some(3_000),
            test: false,
            id: 4,
        }),
    );
    pin_request(
        "gpu-acquire",
        Request::GpuAcquire {
            id: 6,
            acquire: GpuAcquire {
                gpu: "v3d".into(),
                mode: GpuMode::Exclusive,
                priority: 50,
                wait: true,
                max_hold_ms: 60_000,
                label: "bench".into(),
            },
        },
    );
}

/// A welcome from a service before the features field decodes with none.
#[test]
fn a_welcome_without_features_has_none() {
    let mut bytes = vector_bytes("welcome");
    bytes.truncate(bytes.len() - 4);
    bytes[0] -= 4;
    let (message, _) = decode_message(&bytes).unwrap().unwrap();
    assert_eq!(
        message,
        Message::Welcome {
            version: VERSION,
            board: "example-board".into(),
            client_id: 3,
            features: 0,
        }
    );
}

#[test]
fn message_vectors_match_the_encoder() {
    pin_message(
        "welcome",
        Message::Welcome {
            version: VERSION,
            board: "example-board".into(),
            client_id: 3,
            features: FEATURE_GPU_LEASE,
        },
    );
    let values = [9_806, NO_VALUE, -3];
    pin_message(
        "reading",
        Message::Reading(RawReading {
            device: "imu".into(),
            timestamp_us: 1_000_000,
            status: DeviceStatus::Available,
            values: values.to_vec(),
        }),
    );
    // The service's fast path encodes a reading without building the message.
    assert_eq!(
        encode_reading("imu", 1_000_000, DeviceStatus::Available, &values),
        vector_bytes("reading"),
    );
    pin_message(
        "reply-refused",
        Message::Reply {
            id: 4,
            result: Err(Refusal::Device(ErrorKind::Busy)),
        },
    );
    pin_message(
        "reply-ok",
        Message::Reply {
            id: 2,
            result: Ok(0.5),
        },
    );
    pin_message(
        "devices",
        Message::Devices(vec![DeviceDesc {
            id: "imu".into(),
            label: "IMU".into(),
            class: DeviceClass::Imu,
            model: "bmi088".into(),
            status: DeviceStatus::Available,
            reason: String::new(),
            channels: vec![
                ChannelDesc {
                    name: "acceleration.x".into(),
                    quantity: Quantity::Acceleration,
                    axis: Axis::X,
                    exponent: -3,
                },
                ChannelDesc {
                    name: "angular_rate.z".into(),
                    quantity: Quantity::AngularRate,
                    axis: Axis::Z,
                    exponent: -6,
                },
            ],
            controls: Vec::new(),
            pixels: 0,
        }]),
    );
    pin_message(
        "event-control",
        Message::Event(Event::Control {
            device: "fan0".into(),
            control: "duty".into(),
            value: 0.5,
            by: "example".into(),
        }),
    );
    pin_message(
        "data-reply",
        Message::Data {
            id: 5,
            result: Ok(vec![0x12, 0x34]),
        },
    );
    pin_message(
        "gpu-lease",
        Message::GpuLease {
            id: 6,
            lease: 1,
            state: GpuLeaseState::Queued { ahead: 2 },
        },
    );
}
