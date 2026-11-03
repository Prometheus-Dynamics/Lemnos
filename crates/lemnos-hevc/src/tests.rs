//! Parser tests: bit reading and NAL splitting on constructed bytes, and the
//! parameter sets and an IDR slice header of a stream encoded on the host (a
//! 320x240 frame from the VA-API encoder; see `docs/dma.md`'s HEVC section).

use crate::*;
use std::vec::Vec;

/// The fixture: an AUD, VPS, SPS, PPS and one IDR_W_RADL slice, Annex B.
const IDR_STREAM: &[u8] = include_bytes!("../tests/fixtures/idr320.h265");

/// Bits from a string of `0` and `1` (spaces ignored), packed MSB first.
fn pack(bits: &str) -> Vec<u8> {
    let bits: Vec<u8> = bits.bytes().filter(|b| *b == b'0' || *b == b'1').collect();
    bits.chunks(8)
        .map(|c| {
            let mut byte = 0u8;
            for (i, b) in c.iter().enumerate() {
                byte |= (b - b'0') << (7 - i);
            }
            byte
        })
        .collect()
}

#[test]
fn the_fixture_splits_into_its_nal_units() {
    let types: Vec<u8> = nal_units(IDR_STREAM).map(|n| n.unit_type).collect();
    assert_eq!(types, [35, 32, 33, 34, 19]);
    let slice = nal_units(IDR_STREAM).last().unwrap();
    assert_eq!(slice.layer_id, 0);
    assert_eq!(slice.temporal_id_plus1, 1);
    // The unit's bytes end where the stream does (no trailing zeros are kept).
    assert_eq!(slice.raw.last(), IDR_STREAM.last());
}

#[test]
fn exp_golomb_codes_decode() {
    let data = pack("1 010 011 00100 00101 0001000");
    let mut r = BitReader::new(&data);
    assert_eq!(r.ue().unwrap(), 0);
    assert_eq!(r.ue().unwrap(), 1);
    assert_eq!(r.ue().unwrap(), 2);
    assert_eq!(r.ue().unwrap(), 3);
    assert_eq!(r.ue().unwrap(), 4);
    assert_eq!(r.ue().unwrap(), 7);

    let data = pack("010 011 00100 00101");
    let mut r = BitReader::new(&data);
    assert_eq!(r.se().unwrap(), 1);
    assert_eq!(r.se().unwrap(), -1);
    assert_eq!(r.se().unwrap(), 2);
    assert_eq!(r.se().unwrap(), -2);
}

#[test]
fn reads_past_the_end_are_truncated() {
    let data = pack("1");
    let mut r = BitReader::new(&data);
    assert_eq!(r.bits(9), Err(Error::Truncated));
    let data = [0u8; 2];
    assert_eq!(BitReader::new(&data).ue(), Err(Error::Truncated));
}

#[test]
fn emulation_prevention_bytes_are_removed_and_indexed() {
    let raw = [0x26, 0x01, 0x00, 0x00, 0x03, 0x01, 0x00, 0x00, 0x03, 0x00];
    let mut out = [0u8; 16];
    let n = unescape(&raw, &mut out).unwrap();
    assert_eq!(&out[..n], [0x26, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]);
    // RBSP byte 4 (the 0x01 after the escape) is raw byte 5.
    assert_eq!(raw_index(&raw, 4), Some(5));
    assert_eq!(raw_index(&raw, 0), Some(0));
    assert_eq!(raw_index(&raw, 99), None);
    assert_eq!(unescape(&raw, &mut out[..3]), Err(Error::Buffer));
}

#[test]
fn a_four_byte_start_code_does_not_leak_a_zero_into_the_unit() {
    let stream = [
        0, 0, 0, 1, 0x42, 0x01, 0xAA, 0, 0, 0, 1, 0x44, 0x01, 0xBB, 0, 0,
    ];
    let units: Vec<&[u8]> = nal_units(&stream).map(|n| n.raw).collect();
    assert_eq!(units, [&[0x42, 0x01, 0xAA][..], &[0x44, 0x01, 0xBB][..]]);
}

#[test]
fn the_vps_gives_its_identifier_and_sub_layers() {
    let vps = nal_units(IDR_STREAM).find(|n| n.unit_type == 32).unwrap();
    let mut buf = [0u8; 64];
    let n = unescape(vps.raw, &mut buf).unwrap();
    let v = parse_vps(&buf[..n]).unwrap();
    assert_eq!(v.id, 0);
    assert_eq!(v.max_layers_minus1, 0);
    assert_eq!(v.max_sub_layers_minus1, 0);
}

#[test]
fn the_sps_of_the_host_stream_gives_its_size_and_tools() {
    let sps = nal_units(IDR_STREAM).find(|n| n.unit_type == 33).unwrap();
    let mut buf = [0u8; 128];
    let n = unescape(sps.raw, &mut buf).unwrap();
    let s = parse_sps(&buf[..n]).unwrap();
    assert_eq!((s.width, s.height), (320, 240));
    assert_eq!(s.id, 0);
    assert_eq!(s.chroma_format_idc, 1);
    assert_eq!(s.chroma_array_type(), 1);
    assert_eq!((s.bit_depth_luma_minus8, s.bit_depth_chroma_minus8), (0, 0));
    assert_eq!(s.log2_ctb(), 6);
    assert_eq!(s.ctbs(), (5, 4));
    assert!(!s.scaling_list_enabled);
}

#[test]
fn the_pps_of_the_host_stream_is_read_to_its_end() {
    let pps = nal_units(IDR_STREAM).find(|n| n.unit_type == 34).unwrap();
    let mut buf = [0u8; 128];
    let n = unescape(pps.raw, &mut buf).unwrap();
    let p = parse_pps(&buf[..n]).unwrap();
    assert_eq!((p.id, p.sps_id), (0, 0));
    assert!(!p.tiles_enabled);
    assert!(!p.scaling_list_data_present);
}

#[test]
fn the_idr_slice_header_points_at_its_data() {
    let mut sps_buf = [0u8; 128];
    let mut pps_buf = [0u8; 128];
    let mut slice_buf = [0u8; 4096];
    let mut sps = None;
    let mut pps = None;
    let mut slice = None;
    for nal in nal_units(IDR_STREAM) {
        match nal.unit_type {
            33 => {
                let n = unescape(nal.raw, &mut sps_buf).unwrap();
                sps = Some(parse_sps(&sps_buf[..n]).unwrap());
            }
            34 => {
                let n = unescape(nal.raw, &mut pps_buf).unwrap();
                pps = Some(parse_pps(&pps_buf[..n]).unwrap());
            }
            19 => {
                let n = unescape(nal.raw, &mut slice_buf).unwrap();
                slice = Some((nal, n));
            }
            _ => {}
        }
    }
    let (sps, pps) = (sps.unwrap(), pps.unwrap());
    let (nal, n) = slice.unwrap();
    let h = parse_slice_header(&slice_buf[..n], nal.unit_type, &sps, &pps).unwrap();
    assert!(h.first_slice_segment_in_pic);
    assert_eq!(h.pps_id, 0);
    assert_eq!(h.slice_type, SLICE_I);
    assert!(!h.dependent);
    assert_eq!(h.segment_address, 0);
    assert!(
        h.data_offset > 2 && h.data_offset < n,
        "data after the header, inside the unit"
    );
    // The data is the rest of the unit: its bytes are in the raw NAL unit.
    let raw_at = raw_index(nal.raw, h.data_offset).unwrap();
    assert!(raw_at > 0 && raw_at < nal.raw.len());
}

#[test]
fn a_slice_of_a_non_idr_picture_is_refused() {
    let sps = Sps {
        width: 64,
        height: 64,
        ..Sps::default()
    };
    let pps = Pps::default();
    let data = [0u8; 8];
    assert!(matches!(
        parse_slice_header(&data, 1, &sps, &pps),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn an_explicit_short_term_set_reads_its_pictures() {
    // num_negative_pics 2 (011), num_positive_pics 1 (010); negatives -1
    // (ue 0, used 1) and -3 (ue 1, used 0); positive +2 (ue 1, used 1).
    let data = pack("011 010 1 1 010 0 010 1");
    let mut r = BitReader::new(&data);
    let set = parse_st_rps(&mut r, 0, &StRps::default()).unwrap();
    assert_eq!((set.num_negative, set.num_positive), (2, 1));
    assert_eq!(&set.delta_poc_s0[..2], &[-1, -3]);
    assert_eq!(&set.used_s0[..2], &[true, false]);
    assert_eq!(set.delta_poc_s1[0], 2);
    assert!(set.used_s1[0]);
}

#[test]
fn a_predicted_short_term_set_derives_its_pictures() {
    // The previous set is {-1}. This set predicts with deltaRps = -1 and
    // keeps both entries (used flags 1 1): {-1, -2}, no positive pictures.
    let previous = StRps {
        num_negative: 1,
        delta_poc_s0: [-1; 16],
        used_s0: [true; 16],
        ..StRps::default()
    };
    let data = pack("1 1 1 1 1");
    let mut r = BitReader::new(&data);
    let set = parse_st_rps(&mut r, 1, &previous).unwrap();
    assert_eq!((set.num_negative, set.num_positive), (2, 0));
    assert_eq!(&set.delta_poc_s0[..2], &[-1, -2]);
}
