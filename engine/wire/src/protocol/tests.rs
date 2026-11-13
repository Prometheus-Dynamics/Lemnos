use std::collections::BTreeMap;

use super::*;

fn roundtrip(frame: Frame<'_>) {
    let mut buf = [0; 256];
    let n = encode(&frame, &mut buf).unwrap();
    let (back, used) = decode(&buf[..n]).unwrap().unwrap();
    assert_eq!(back, frame);
    assert_eq!(used, n);
}

/// Every frame in the specification, with the values its vector uses.
fn vectors() -> BTreeMap<&'static str, Frame<'static>> {
    BTreeMap::from([
        (
            "hello",
            Frame::Hello {
                version: 1,
                name: "tool",
                events: true,
            },
        ),
        (
            "welcome",
            Frame::Welcome {
                version: 1,
                features: 0,
                host: "h",
            },
        ),
        ("list", Frame::List { id: 1 }),
        (
            "device",
            Frame::Device {
                id: 1,
                device: 2,
                name: "a",
                driver: "b",
                tags: "c/1",
                state: state::BACKOFF,
                health: health::FAILED,
                reason: "r",
                link: Appended("d"),
            },
        ),
        ("list-done", Frame::ListDone { id: 1 }),
        ("packages", Frame::Packages { id: 14 }),
        (
            "package",
            Frame::Package {
                id: 14,
                path: "/p",
                driver: "d",
                reason: "",
            },
        ),
        (
            "packages-done",
            Frame::PackagesDone {
                id: 14,
                mode: package_mode::SIGNED,
                note: "",
            },
        ),
        (
            "open",
            Frame::Open {
                id: 2,
                device: "",
                tag: "c/1",
            },
        ),
        (
            "opened",
            Frame::Opened {
                id: 2,
                device: 3,
                contract: 1,
            },
        ),
        ("describe", Frame::Describe { id: 3, device: 3 }),
        ("description", Frame::Description { id: 3, bytes: &[9] }),
        (
            "call",
            Frame::Call {
                id: 7,
                device: 3,
                contract: 1,
                method: 2,
                reply_capacity: 16,
                args: &[0xaa, 0xbb],
            },
        ),
        ("reply", Frame::Reply { id: 7, bytes: &[1] }),
        (
            "subscribe",
            Frame::Subscribe {
                id: 8,
                device: 3,
                contract: 0,
                method: 1,
                period_us: 5000,
                only_changes: true,
            },
        ),
        (
            "subscribed",
            Frame::Subscribed {
                id: 8,
                subscription: 4,
                period_us: 5000,
            },
        ),
        (
            "unsubscribe",
            Frame::Unsubscribe {
                id: 9,
                subscription: 4,
            },
        ),
        (
            "value",
            Frame::Value {
                subscription: 4,
                stamp_ns: 1000,
                bytes: &[5, 6],
            },
        ),
        (
            "set-power",
            Frame::SetPower {
                id: 10,
                device: 3,
                on: false,
            },
        ),
        (
            "stream-open",
            Frame::StreamOpen {
                id: 11,
                device: 3,
                name: "s",
            },
        ),
        (
            "stream-ready",
            Frame::StreamReady {
                id: 11,
                buffers: 4,
                descriptors: 3,
                buffer_bytes: 256,
                frame_slots: 4,
                job_slots: 2,
                job_bytes: 8,
            },
        ),
        (
            "stream-close",
            Frame::StreamClose {
                id: 12,
                device: 3,
                name: "s",
            },
        ),
        (
            "stream-open-handles",
            Frame::StreamOpenHandles {
                id: 13,
                device: 3,
                name: "s",
            },
        ),
        (
            "stream-handles",
            Frame::StreamHandles {
                id: 13,
                buffers: 4,
                descriptors: 6,
                buffer_bytes: 256,
                frame_slots: 4,
                job_slots: 2,
                job_bytes: 8,
                plane_bytes: &[],
            },
        ),
        (
            "stream-handles-planes",
            Frame::StreamHandles {
                id: 14,
                buffers: 2,
                descriptors: 6,
                buffer_bytes: 256,
                frame_slots: 2,
                job_slots: 1,
                job_bytes: 0,
                plane_bytes: &[0xc0, 0, 0, 0, 0x40, 0, 0, 0],
            },
        ),
        (
            "event",
            Frame::Event {
                device: 3,
                state: state::RUNNING,
                health: health::OK,
                reason: "",
            },
        ),
        (
            "refusal",
            Frame::Refusal {
                id: 12,
                status: -2,
                text: "no",
            },
        ),
    ])
}

/// Pulls `name -> hex` out of `docs/wire.md`.
fn spec() -> BTreeMap<String, Vec<u8>> {
    let text = include_str!("../../../../docs/wire.md");
    let mut out = BTreeMap::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let Some(name) = line
            .strip_prefix("<!-- vector: ")
            .and_then(|l| l.strip_suffix(" -->"))
        else {
            continue;
        };
        lines.next();
        let hex = lines.next().unwrap();
        let bytes = hex
            .split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect();
        out.insert(name.to_string(), bytes);
    }
    out
}

#[test]
fn every_vector_in_the_spec_matches_byte_for_byte() {
    let spec = spec();
    let vectors = vectors();
    assert_eq!(
        spec.keys().map(String::as_str).collect::<Vec<_>>(),
        vectors.keys().copied().collect::<Vec<_>>(),
        "the spec and this table list different frames"
    );
    for (name, frame) in vectors {
        let mut buf = [0; 256];
        let n = encode(&frame, &mut buf).unwrap();
        assert_eq!(&buf[..n], spec[name].as_slice(), "encoding {name}");
        let (back, used) = decode(&spec[name]).unwrap().unwrap();
        assert_eq!(back, frame, "decoding {name}");
        assert_eq!(used, spec[name].len());
    }
}

#[test]
fn a_stream_of_handles_has_two_descriptors_more_than_buffers_and_at_most_128_buffers() {
    assert_eq!(handle_descriptors(0, 1), None);
    assert_eq!(handle_descriptors(1, 1), Some(3));
    assert_eq!(handle_descriptors(MAX_HANDLE_BUFFERS, 1), Some(130));
    assert_eq!(handle_descriptors(MAX_HANDLE_BUFFERS + 1, 1), None);
    assert_eq!(handle_descriptors(u32::MAX, 1), None);
    // A descriptor per plane: the same limit over every plane of every buffer.
    assert_eq!(handle_descriptors(4, 2), Some(10));
    assert_eq!(handle_descriptors(MAX_HANDLE_BUFFERS / 2, 2), Some(130));
    assert_eq!(handle_descriptors(MAX_HANDLE_BUFFERS / 2 + 1, 2), None);
    assert_eq!(handle_descriptors(1, 0), None);
    assert_eq!(handle_descriptors(1, MAX_PLANES + 1), None);
    assert_eq!(handle_descriptors(u32::MAX, u32::MAX), None);
}

#[test]
fn planes_are_checked_against_the_buffer_and_travel_as_lengths() {
    let planes = Planes::new(&[192, 64], 256).unwrap();
    assert_eq!(planes.count(), 2);
    assert_eq!(planes.offset(0), Some(0));
    assert_eq!(planes.offset(1), Some(192));
    assert_eq!(planes.offset(2), None);
    assert_eq!(planes.locate(0), Some((0, 0, 192)));
    assert_eq!(planes.locate(191), Some((0, 191, 1)));
    assert_eq!(planes.locate(192), Some((1, 0, 64)));
    assert_eq!(planes.locate(256), None);
    let mut out = [0; PLANE_LENGTH_BYTES * MAX_PLANES as usize];
    let n = planes.encode(&mut out);
    assert_eq!(&out[..n], &[0xc0, 0, 0, 0, 0x40, 0, 0, 0]);
    assert_eq!(Planes::decode(&out[..n], 256), Some(planes));
    // One plane travels as nothing, and nothing reads back as one plane.
    assert_eq!(Planes::one(256).encode(&mut out), 0);
    assert_eq!(Planes::decode(&[], 256), Some(Planes::one(256)));
    assert_eq!(Planes::decode(&[], 0), None);
    for (bytes, total) in [
        (&[0xc0, 0, 0, 0, 0x40, 0, 0, 0][..], 255),
        (&[0xc0, 0, 0, 0, 0x40, 0, 0][..], 256),
        (&[0, 0, 0, 0, 0, 1, 0, 0][..], 256),
        (&[0xff, 0xff, 0xff, 0xff, 1, 1, 0, 0][..], 256),
        (&[1, 0, 0, 0][..].repeat(9)[..], 9),
    ] {
        assert_eq!(Planes::decode(bytes, total), None, "{bytes:?} of {total}");
    }
    assert_eq!(Planes::new(&[], 0), None);
    assert_eq!(Planes::new(&[1; 9], 9), None);
}

#[test]
fn a_partial_frame_needs_more_bytes() {
    let mut buf = [0; 64];
    let n = encode(&Frame::List { id: 1 }, &mut buf).unwrap();
    for cut in 0..n {
        assert_eq!(decode(&buf[..cut]), Ok(None), "cut at {cut}");
    }
}

#[test]
fn two_frames_in_one_read_come_out_in_order() {
    let mut out = Vec::new();
    append(&Frame::List { id: 1 }, &mut out).unwrap();
    append(&Frame::ListDone { id: 2 }, &mut out).unwrap();
    let (first, used) = decode(&out).unwrap().unwrap();
    assert_eq!(first, Frame::List { id: 1 });
    let (second, _) = decode(&out[used..]).unwrap().unwrap();
    assert_eq!(second, Frame::ListDone { id: 2 });
}

#[test]
fn an_oversized_length_is_refused_before_it_is_read() {
    let header = (MAX_FRAME as u32).to_le_bytes();
    assert!(matches!(decode(&header), Err(WireError::FrameTooLarge(_))));
}

#[test]
fn a_frame_too_short_for_its_kind_is_refused() {
    assert_eq!(decode(&[1, 0, 0, 0, 9]), Err(WireError::Malformed));
}

#[test]
fn an_unknown_kind_is_refused() {
    assert_eq!(
        decode(&[2, 0, 0, 0, 0xff, 0xff]),
        Err(WireError::UnknownKind(0xffff))
    );
}

#[test]
fn a_truncated_field_is_refused_not_guessed() {
    // A `List` whose id is cut short.
    assert_eq!(decode(&[4, 0, 0, 0, 3, 0, 1, 0]), Err(WireError::Malformed));
}

#[test]
fn a_bool_other_than_0_or_1_is_refused() {
    let mut buf = [0; 64];
    let n = encode(
        &Frame::SetPower {
            id: 1,
            device: 1,
            on: true,
        },
        &mut buf,
    )
    .unwrap();
    buf[n - 1] = 2;
    assert_eq!(decode(&buf[..n]), Err(WireError::Malformed));
}

#[test]
fn bad_utf8_is_refused_not_cut() {
    let mut buf = [0; 64];
    let n = encode(
        &Frame::Refusal {
            id: 1,
            status: 0,
            text: "ab",
        },
        &mut buf,
    )
    .unwrap();
    buf[n - 1] = 0xff;
    assert_eq!(decode(&buf[..n]), Err(WireError::Malformed));
}

#[test]
fn appended_fields_are_skipped() {
    let mut buf = [0; 64];
    let n = encode(&Frame::List { id: 5 }, &mut buf).unwrap();
    buf[n] = 0xee;
    let grown = (n - 4 + 1) as u32;
    buf[..4].copy_from_slice(&grown.to_le_bytes());
    let (frame, used) = decode(&buf[..n + 1]).unwrap().unwrap();
    assert_eq!(frame, Frame::List { id: 5 });
    assert_eq!(used, n + 1);
}

#[test]
fn a_device_from_before_its_link_reads_as_having_none() {
    let with = Frame::Device {
        id: 1,
        device: 2,
        name: "a",
        driver: "b",
        tags: "c/1",
        state: state::BACKOFF,
        health: health::FAILED,
        reason: "r",
        link: Appended("d"),
    };
    let mut buf = [0; 64];
    let n = encode(&with, &mut buf).unwrap();
    // What an older host sends: the same frame, ending after `reason`.
    let older = n - 5;
    buf[..4].copy_from_slice(&((older - 4) as u32).to_le_bytes());
    let (frame, used) = decode(&buf[..older]).unwrap().unwrap();
    assert_eq!(used, older);
    let Frame::Device { link, reason, .. } = frame else {
        panic!("{frame:?}");
    };
    assert_eq!((reason, link), ("r", Appended("")));
    // A link that is there but cut short is malformed, not empty.
    buf[..4].copy_from_slice(&((older - 4 + 2) as u32).to_le_bytes());
    assert_eq!(decode(&buf[..older + 2]), Err(WireError::Malformed));
}

#[test]
fn a_frame_over_the_limit_is_not_built() {
    let args = vec![0; MAX_FRAME];
    let frame = Frame::Call {
        id: 1,
        device: 1,
        contract: 0,
        method: 0,
        reply_capacity: 0,
        args: &args,
    };
    let mut out = vec![7];
    assert_eq!(append(&frame, &mut out), Err(WireError::TooLarge));
    assert_eq!(out, [7]);
}

#[test]
fn a_buffer_too_small_is_an_error_and_writes_no_length() {
    let mut buf = [0xee; 8];
    assert_eq!(
        encode(
            &Frame::Refusal {
                id: 1,
                status: 0,
                text: "long text"
            },
            &mut buf
        ),
        Err(WireError::TooLarge)
    );
}

#[test]
fn append_grows_for_a_large_frame() {
    let args = vec![1; 10_000];
    let frame = Frame::Reply {
        id: 1,
        bytes: &args,
    };
    let mut out = Vec::new();
    append(&frame, &mut out).unwrap();
    assert_eq!(decode(&out).unwrap().unwrap().0, frame);
}

#[test]
fn frames_round_trip() {
    for frame in vectors().into_values() {
        roundtrip(frame);
    }
}

mod description {
    use lemnos_core::{
        ContractInfo, Description, Field, Item, Manifest, MethodId, MethodInfo, Param, Shape, Tag,
        Unit,
    };

    use super::{OwnedShape, decode_description, encode_description};

    const CONTRACT: ContractInfo = ContractInfo {
        tag: Tag::new("example.thing/1"),
        methods: &[MethodInfo {
            id: 3,
            name: "read",
            params: &[Param {
                name: "n",
                shape: Shape::Option(&Shape::U16),
            }],
            returns: Shape::Record(&[Field {
                name: "x",
                shape: Shape::Array(&Shape::F32, 2),
            }]),
            has_out_buffer: true,
        }],
    };

    #[test]
    fn a_description_round_trips() {
        let manifest = Manifest {
            name: "n",
            needs: &[],
            provides: &[CONTRACT],
        };
        let method = MethodId {
            contract: 0,
            method: 3,
        };
        let mut description = Description::new();
        description.add(
            Item::value("level", method)
                .unit(Unit::Volt, -3)
                .range(-1, 1)
                .field("x"),
        );
        let mut buf = [0; 512];
        let n = encode_description(&manifest, &description, &mut buf).unwrap();
        let back = decode_description(&buf[..n]).unwrap();

        let m = &back.contracts[0].methods[0];
        assert_eq!(back.contracts[0].tag, "example.thing/1");
        assert_eq!((m.id, m.name.as_str(), m.has_out_buffer), (3, "read", true));
        assert_eq!(
            m.params[0].shape,
            OwnedShape::Option(Box::new(OwnedShape::U16))
        );
        assert_eq!(
            m.returns,
            OwnedShape::Record(vec![(
                "x".to_string(),
                OwnedShape::Array(Box::new(OwnedShape::F32), 2)
            )])
        );
        let item = &back.items[0];
        assert_eq!(item.unit, "V");
        assert_eq!((item.exponent, item.min, item.max), (-3, Some(-1), Some(1)));
        assert_eq!((item.contract, item.method), (0, 3));
        assert_eq!(item.field.as_deref(), Some("x"));
    }

    #[test]
    fn a_shape_nested_without_end_is_refused() {
        // One contract, one method, one param, then Option forever.
        let mut bytes = vec![1, 0];
        bytes.extend([3, 0, 0, 0, b'a', b'/', b'1']);
        bytes.extend([1, 0, 0, 0]);
        bytes.extend([0, 0, 0, 0]);
        bytes.extend([1, 0, 0, 0, 0, 0]);
        bytes.extend([14; 64]);
        assert!(decode_description(&bytes).is_err());
    }
}
